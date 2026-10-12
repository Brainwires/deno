#!/usr/bin/env bash
# Smoke-test a `deno` CLI archive from assemble_cli.sh: unpack it and check
# that the binary names its denext build, keeps a plain `Deno.version.deno`,
# has `Deno.SQL`, and runs a Vitest suite under `deno test`
# (smoke/cli/vitest_test.ts).
#
# Env: TARGET VERSION ARCHIVE WORK_DIR (a directory that does not exist yet)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${ARCHIVE:?}" "${WORK_DIR:?}"

work=$(unix_path "$WORK_DIR")
test ! -e "$work" || { echo "$work already exists" >&2; exit 1; }
mkdir -p "$work/cli" "$work/app" "$work/deno-dir"
unpack_archive "$(unix_path "$ARCHIVE")" "$work/cli"
bin=$(cli_bin_name "$TARGET")
deno="$work/cli/$bin"
test -f "$deno" || { echo "::error::no $bin in $ARCHIVE" >&2; exit 1; }
# A clean cache, and no config or lockfile from the checkout.
export DENO_DIR="$work/deno-dir" DENO_NO_UPDATE_CHECK=1 NO_COLOR=1

echo "::group::deno --version"
"$deno" --version | tee "$work/version.txt"
echo "::endgroup::"
first=$(head -1 "$work/version.txt" | tr -d '\r')
case "$first" in
  "deno $VERSION ("*) ;;
  *) echo "::error::deno --version says '$first', want 'deno $VERSION (...)'" >&2; exit 1 ;;
esac
short=$("$deno" -V | tr -d '\r')
test "$short" = "deno $VERSION" || { echo "::error::deno -V says '$short', want 'deno $VERSION'" >&2; exit 1; }

# Deno.version.deno stays the plain, semver-parseable Deno version.
deno_version=$("$deno" eval 'console.log(Deno.version.deno)' | tr -d '\r')
case "$VERSION" in
  "$deno_version"-*) echo "Deno.version.deno: $deno_version" ;;
  *) echo "::error::Deno.version.deno is '$deno_version', not the base of $VERSION" >&2; exit 1 ;;
esac

sql=$("$deno" eval 'console.log(typeof Deno.SQL)' | tr -d '\r')
test "$sql" = function || { echo "::error::typeof Deno.SQL is '$sql', want 'function'" >&2; exit 1; }
echo "typeof Deno.SQL: $sql"

cp "$here/smoke/cli/vitest_test.ts" "$work/app/"
echo "::group::deno test (vitest)"
# -A as the vitest specs run it: the compatibility layer reads the env.
(cd "$work/app" && "$deno" test -A --no-config --no-lock vitest_test.ts) 2>&1 | tee "$work/test.txt"
echo "::endgroup::"
grep -q "ok | 2 passed" "$work/test.txt" || { echo "::error::the Vitest suite did not pass its 2 tests" >&2; exit 1; }
echo "cli smoke ok: $first"
