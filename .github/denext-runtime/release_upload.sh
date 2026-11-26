#!/usr/bin/env bash
# Publish a tag's GitHub release with its assets, safely retried and idempotent
# (DENEXT.md item 558; denext_runtime.yml's publish job):
#
#   release_upload.sh <notes file> <file>...
#
#   REPO        owner/name
#   TAG         the release's (annotated, already checked) tag
#   LATEST      `true` / `false`: whether the release becomes "Latest"
#   PRERELEASE  `--prerelease` (tag_gate.py --prerelease-flag) or empty
#   UPLOAD_ATTEMPTS  tries per batch (default 5)
#   UPLOAD_BATCH     files per `gh release upload` (default 8)
#   UPLOAD_BACKOFF   seconds before the second try, doubled per try (default 15)
#
# One `gh release create ... <files>` failed on the denext.30 tag run during a
# GitHub incident: gh retried an upload GitHub had already stored, got HTTP 422
# "ReleaseAsset.name already exists", and deleted its draft. Instead:
#
#   1. The release is found by its tag among every release, drafts included,
#      and reused (a re-run of the job); with none, it is created as a draft
#      holding only the notes (`--verify-tag`: never a tag of its own).
#   2. The files go up in batches with `gh release upload --clobber`. A batch
#      that fails is retried after a backoff, with only its files the release
#      does not yet hold (name, size, state `uploaded`); an asset of one of
#      those names that is there (a half-stored upload) is deleted first.
#   3. Every file must then be on the release under its name with its size, and
#      the release must hold nothing else: only then is the draft published,
#      with the notes, `make_latest` and the pre-release flag.
#
# A release already published is never changed: a complete one (every asset,
# right sizes, nothing else) is a re-run's success, anything else an error.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

[ $# -ge 2 ] || { echo "::error::usage: release_upload.sh <notes file> <file>..." >&2; exit 1; }
notes=$1
shift
need "the release notes file" test -f "$notes"
: "${REPO:?}" "${TAG:?}" "${LATEST:?}"
PRERELEASE=${PRERELEASE:-}
attempts=${UPLOAD_ATTEMPTS:-5}
batch=${UPLOAD_BATCH:-8}
backoff=${UPLOAD_BACKOFF:-15}
case "$LATEST" in
  true | false) ;;
  *) echo "::error::LATEST must be true or false, not '$LATEST'" >&2; exit 1 ;;
esac
case "$PRERELEASE" in
  --prerelease) prerelease=true ;;
  "") prerelease=false ;;
  *) echo "::error::PRERELEASE must be --prerelease or empty, not '$PRERELEASE'" >&2; exit 1 ;;
esac

files=("$@")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# `<name>\t<size>` of every file, in $work/expected (no associative arrays:
# bash 3.2 on a Mac runs this too).
: >"$work/expected"
for f in "${files[@]}"; do
  need "the release file $f" test -f "$f"
  printf '%s\t%s\n' "$(basename "$f")" "$(size_of "$f")" >>"$work/expected"
done
dup=$(cut -f1 "$work/expected" | sort | uniq -d)
[ -z "$dup" ] || { echo "::error::two release files are named $dup" >&2; exit 1; }

# Every release of $TAG as `<id> <draft>`, drafts included (the tag endpoint
# never returns a draft).
releases_of_tag() {
  gh api --paginate "repos/$REPO/releases?per_page=100" \
    --jq '.[] | "\(.id)\t\(.draft)\t\(.tag_name)"' |
    awk -F'\t' -v t="$TAG" '$3 == t { print $1 " " $2 }'
}

# The release's assets, one `<name>\t<size>\t<state>\t<id>` per line, into
# $work/assets.
list_assets() {
  gh api --paginate "repos/$REPO/releases/$release_id/assets?per_page=100" \
    --jq '.[] | "\(.name)\t\(.size)\t\(.state)\t\(.id)"' >"$work/assets"
}

# Whether the asset named $1 is on the release, uploaded, with its file's size.
asset_ok() {
  awk -F'\t' -v n="$1" '
    FNR == NR { if ($1 == n) want = $2; next }
    $1 == n && $2 == want && $3 == "uploaded" { found = 1 }
    END { exit !found }' "$work/expected" "$work/assets"
}

# Prints each problem with the release's assets (missing, wrong size or state,
# unexpected); prints nothing when it holds exactly the files.
asset_problems() {
  list_assets
  awk -F'\t' '
    FNR == NR { want[$1] = $2; next }
    { have[$1] = have[$1] sep[$1] $2 " bytes " $3; sep[$1] = ", " }
    !($1 in want) { print "unexpected: " $1 }
    END {
      for (n in want) {
        if (!(n in have)) print "missing: " n
        else if (have[n] != want[n] " bytes uploaded") print "wrong: " n " (want " want[n] " bytes uploaded, have " have[n] ")"
      }
    }' "$work/expected" "$work/assets"
}

found=$(releases_of_tag)
count=$(printf '%s' "$found" | grep -c . || true)
if [ "$count" -gt 1 ]; then
  echo "::error::$count releases have the tag $TAG: delete the extra drafts, then re-run" >&2
  printf '%s\n' "$found" >&2
  exit 1
fi
if [ "$count" -eq 1 ]; then
  release_id=${found%% *}
  draft=${found#* }
  if [ "$draft" != true ]; then
    # Published: never changed. Complete is a re-run's success.
    problems=$(asset_problems)
    if [ -n "$problems" ]; then
      echo "::error::$TAG is already published but its assets differ from this run's (not changing a published release):" >&2
      printf '%s\n' "$problems" >&2
      exit 1
    fi
    echo "$TAG is already published with every asset: nothing to do"
    exit 0
  fi
  echo "reusing the draft release $release_id of $TAG"
else
  gh release create "$TAG" --repo "$REPO" --title "$TAG" \
    --notes-file "$notes" --verify-tag --draft
  # The new draft, found again by its tag (its URL names no tag).
  release_id=
  for try in 1 2 3 4 5; do
    found=$(releases_of_tag)
    if [ -n "$found" ]; then
      release_id=${found%% *}
      break
    fi
    sleep "$try"
  done
  [ -n "$release_id" ] ||
    { echo "::error::the draft release of $TAG was created but is not listed" >&2; exit 1; }
  echo "created the draft release $release_id of $TAG"
fi

# Uploads one batch, retrying with only the files still missing.
upload_batch() {
  local try f name aid delay=$backoff
  for ((try = 1; try <= attempts; try++)); do
    list_assets
    local -a pending=()
    for f in "$@"; do
      name=$(basename "$f")
      asset_ok "$name" && continue
      pending+=("$f")
      # A half-stored asset of that name: gone before the upload.
      while IFS= read -r aid; do
        [ -n "$aid" ] || continue
        echo "deleting the incomplete asset $name ($aid)"
        gh api -X DELETE "repos/$REPO/releases/assets/$aid" >/dev/null || true
      done < <(awk -F'\t' -v n="$name" '$1 == n { print $4 }' "$work/assets")
    done
    if [ "${#pending[@]}" -eq 0 ]; then
      return 0
    fi
    echo "uploading ${#pending[@]} file(s), try $try of $attempts: ${pending[*]}"
    if gh release upload "$TAG" --repo "$REPO" --clobber "${pending[@]}"; then
      continue
    fi
    if [ "$try" -lt "$attempts" ]; then
      echo "::warning::the upload failed (try $try of $attempts); retrying in ${delay}s"
      sleep "$delay"
      delay=$((delay * 2))
    fi
  done
  # The last try may have stored everything despite its error.
  list_assets
  for f in "$@"; do
    asset_ok "$(basename "$f")" ||
      { echo "::error::could not upload $f to $TAG after $attempts tries" >&2; return 1; }
  done
}

for ((i = 0; i < ${#files[@]}; i += batch)); do
  upload_batch "${files[@]:i:batch}"
done

problems=$(asset_problems)
if [ -n "$problems" ]; then
  echo "::error::the draft release of $TAG does not hold exactly this run's files (left as a draft):" >&2
  printf '%s\n' "$problems" >&2
  exit 1
fi
echo "every asset is on the draft (${#files[@]}): publishing $TAG"

gh api -X PATCH "repos/$REPO/releases/$release_id" \
  -F draft=false -F prerelease="$prerelease" -f make_latest="$LATEST" \
  -f name="$TAG" -F body=@"$notes" --jq '"published \(.html_url)"'
