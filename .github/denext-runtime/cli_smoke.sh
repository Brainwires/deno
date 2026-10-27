#!/usr/bin/env bash
# Smoke-test a `deno` CLI archive from assemble_cli.sh: unpack it and check
# that the binary names its denext build (`deno --version` with its
# `denext <version> (Brainwires)` line, `deno -V`, `Deno.version.deno` and
# `Deno.build.denext`), has `Deno.SQL`, and runs a Vitest suite under `deno test`
# (smoke/cli/vitest_test.ts); that the bundled esbuild is ESBUILD_VERSION and
# `deno bundle` uses it with an empty DENO_DIR and no reachable npm registry;
# that `deno lsp` defaults to the shipped tsgo build; and that `deno lint
# --type-aware` runs the shipped tsgolint offline.
#
# Env: TARGET VERSION ESBUILD_VERSION ARCHIVE WORK_DIR (a directory that does
# not exist yet)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"

: "${TARGET:?}" "${VERSION:?}" "${ESBUILD_VERSION:?}" "${ARCHIVE:?}" "${WORK_DIR:?}"

work=$(unix_path "$WORK_DIR")
test ! -e "$work" || { echo "$work already exists" >&2; exit 1; }
mkdir -p "$work/cli" "$work/app" "$work/deno-dir" "$work/bundle" "$work/bundle-deno-dir" "$work/check" "$work/check-deno-dir"
archive=$(unix_path "$ARCHIVE")
if command -v unzip >/dev/null 2>&1; then
  unzip -q "$archive" -d "$work/cli"
else
  unpack_archive "$archive" "$work/cli"
fi
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

# Deno.version.deno is the fork's version too (DENEXT.md "Versioning":
# 3.0.0-denext.N, or 3.0.0-denext.N+ci.RUN for a CI build), and semver.
deno_version=$("$deno" eval 'console.log(Deno.version.deno)' | tr -d '\r')
test "$deno_version" = "$VERSION" || { echo "::error::Deno.version.deno is '$deno_version', want '$VERSION'" >&2; exit 1; }
echo "Deno.version.deno: $deno_version"
# The identity marker (DENEXT.md item 99): the last line of `deno --version`
# and Deno.build.denext name the runtime, so denext can tell it from an
# upstream Deno release of the same number.
last=$(tail -1 "$work/version.txt" | tr -d '\r')
test "$last" = "denext $VERSION (Brainwires)" ||
  { echo "::error::deno --version ends with '$last', want 'denext $VERSION (Brainwires)'" >&2; exit 1; }
build_denext=$("$deno" eval 'console.log(Deno.build.denext)' | tr -d '\r')
test "$build_denext" = "$VERSION" || { echo "::error::Deno.build.denext is '$build_denext', want '$VERSION'" >&2; exit 1; }
echo "Deno.build.denext: $build_denext"
# `deno upgrade` must not replace a denext build with upstream Deno.
if "$deno" upgrade --dry-run >"$work/upgrade.txt" 2>&1; then
  echo "::error::deno upgrade ran in a denext build" >&2; cat "$work/upgrade.txt" >&2; exit 1
fi
grep -q "not available in denext's Deno $VERSION" "$work/upgrade.txt" ||
  { echo "::error::deno upgrade did not refuse:" >&2; cat "$work/upgrade.txt" >&2; exit 1; }
# On a Unix target the archive must keep the executable bit.
case "$TARGET" in
  *-windows-msvc) ;;
  *) test -x "$deno" || { echo "::error::$bin is not executable after unzip" >&2; exit 1; } ;;
esac

sql=$("$deno" eval 'console.log(typeof Deno.SQL)' | tr -d '\r')
test "$sql" = function || { echo "::error::typeof Deno.SQL is '$sql', want 'function'" >&2; exit 1; }
echo "typeof Deno.SQL: $sql"

# The esbuild beside deno: the pinned version, executable, licensed.
case "$TARGET" in
  *-windows-msvc) esbuild="$work/cli/esbuild.exe" ;;
  *) esbuild="$work/cli/esbuild" ;;
esac
test -f "$esbuild" || { echo "::error::no $(basename "$esbuild") in $ARCHIVE" >&2; exit 1; }
test -f "$work/cli/esbuild.LICENSE" || { echo "::error::no esbuild.LICENSE in $ARCHIVE" >&2; exit 1; }
case "$TARGET" in
  *-windows-msvc) ;;
  *) test -x "$esbuild" || { echo "::error::esbuild is not executable after unzip" >&2; exit 1; } ;;
esac
esbuild_version=$("$esbuild" --version | tr -d '\r')
test "$esbuild_version" = "$ESBUILD_VERSION" ||
  { echo "::error::esbuild --version says '$esbuild_version', want '$ESBUILD_VERSION'" >&2; exit 1; }
echo "esbuild --version: $esbuild_version"

# `deno bundle` takes that esbuild: an empty DENO_DIR and an npm registry
# nothing listens on, so a download would fail.
printf 'const greeting: string = "bundled with the shipped esbuild";\nconsole.log(greeting);\n' >"$work/bundle/main.ts"
echo "::group::deno bundle (offline)"
(cd "$work/bundle" && DENO_DIR="$work/bundle-deno-dir" NPM_CONFIG_REGISTRY=http://127.0.0.1:9/ \
  "$deno" bundle --no-config --no-lock --output=out.js main.ts)
echo "::endgroup::"
grep -q "bundled with the shipped esbuild" "$work/bundle/out.js" ||
  { echo "::error::deno bundle wrote no bundle" >&2; exit 1; }
test ! -e "$work/bundle-deno-dir/dl" ||
  { echo "::error::deno bundle downloaded an esbuild: $(ls "$work/bundle-deno-dir/dl")" >&2; exit 1; }
echo "deno bundle used the shipped esbuild"

# The TypeScript build `deno lsp`'s tsgo backend uses: next to deno, at the
# version cli/tsc/typescript_pins.json pins for "lsp".
case "$TARGET" in
  *-windows-msvc) tsc="$work/cli/typescript-lsp/tsc.exe" ;;
  *) tsc="$work/cli/typescript-lsp/tsc" ;;
esac
test -f "$tsc" || { echo "::error::no typescript-lsp/$(basename "$tsc") in $ARCHIVE" >&2; exit 1; }
test -f "$work/cli/typescript-lsp/LICENSE" || { echo "::error::no typescript-lsp/LICENSE in $ARCHIVE" >&2; exit 1; }
py=python3; command -v python3 >/dev/null 2>&1 || py=python
want_tsc=$("$py" -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["lsp"]["version"])' "$here/../../cli/tsc/typescript_pins.json")
tsc_version=$("$tsc" --version | tr -d '\r')
test "$tsc_version" = "Version $want_tsc" ||
  { echo "::error::typescript-lsp tsc --version says '$tsc_version', want 'Version $want_tsc'" >&2; exit 1; }
echo "typescript-lsp: $tsc_version"
# `deno lsp` defaults to that tsgo build in a denext release (DENEXT.md item
# 94) and keeps TypeScript 6 with DENO_UNSTABLE_TSGO_LSP=0.
mkdir -p "$work/lsp"
echo "::group::deno lsp (default backend)"
DENO_DIR="$work/lsp-deno-dir" "$py" -I "$here/smoke/cli/lsp_backend.py" "$deno" "$work/lsp" | tee "$work/lsp-default.txt"
echo "::endgroup::"
grep -q "Using the TypeScript 7 (tsgo) language service (the default of this runtime" "$work/lsp-default.txt" ||
  { echo "::error::deno lsp did not default to tsgo" >&2; exit 1; }
grep -q "typescript-lsp" "$work/lsp-default.txt" ||
  { echo "::error::deno lsp did not use the shipped typescript-lsp" >&2; exit 1; }
DENO_UNSTABLE_TSGO_LSP=0 DENO_DIR="$work/lsp-deno-dir" "$py" -I "$here/smoke/cli/lsp_backend.py" "$deno" "$work/lsp" >"$work/lsp-off.txt"
grep -q "Using the TypeScript 6 language service (DENO_UNSTABLE_TSGO_LSP=0)" "$work/lsp-off.txt" ||
  { echo "::error::DENO_UNSTABLE_TSGO_LSP=0 did not keep TypeScript 6:" >&2; cat "$work/lsp-off.txt" >&2; exit 1; }
echo "deno lsp: tsgo by default, TypeScript 6 with DENO_UNSTABLE_TSGO_LSP=0"

# The TypeScript build type checking runs: next to deno, at the version
# typescript_pins.json pins for "check". `deno check` runs it with an empty
# DENO_DIR and an npm registry nothing listens on (nothing can be downloaded):
# a clean module passes, a type error is reported with its code, and the
# in-process fallback (--check-engine=tsc) reports the same error.
case "$TARGET" in
  *-windows-msvc) tsc_check="$work/cli/typescript-check/tsc.exe" ;;
  *) tsc_check="$work/cli/typescript-check/tsc" ;;
esac
test -f "$tsc_check" || { echo "::error::no typescript-check/$(basename "$tsc_check") in $ARCHIVE" >&2; exit 1; }
want_check=$("$py" -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["check"]["version"])' "$here/../../cli/tsc/typescript_pins.json")
check_version=$("$tsc_check" --version | tr -d '\r')
test "$check_version" = "Version $want_check" ||
  { echo "::error::typescript-check tsc --version says '$check_version', want 'Version $want_check'" >&2; exit 1; }
echo "typescript-check: $check_version"
printf 'export const ok: number = 1;\n' >"$work/check/ok.ts"
printf 'export const bad: number = "one";\n' >"$work/check/bad.ts"
echo "::group::deno check (native, offline)"
(cd "$work/check" && DENO_DIR="$work/check-deno-dir" NPM_CONFIG_REGISTRY=http://127.0.0.1:9/ \
  "$deno" check --no-config --no-lock ok.ts)
set +e
(cd "$work/check" && DENO_DIR="$work/check-deno-dir" NPM_CONFIG_REGISTRY=http://127.0.0.1:9/ \
  "$deno" check --no-config --no-lock bad.ts) >"$work/check-native.txt" 2>&1
native_rc=$?
(cd "$work/check" && DENO_DIR="$work/check-deno-dir" \
  "$deno" check --check-engine=tsc --no-config --no-lock bad.ts) >"$work/check-tsc.txt" 2>&1
tsc_rc=$?
set -e
cat "$work/check-native.txt" "$work/check-tsc.txt"
echo "::endgroup::"
test "$native_rc" != 0 && grep -q "TS2322" "$work/check-native.txt" ||
  { echo "::error::native deno check did not report TS2322 (exit $native_rc)" >&2; exit 1; }
test "$tsc_rc" != 0 && grep -q "TS2322" "$work/check-tsc.txt" ||
  { echo "::error::deno check --check-engine=tsc did not report TS2322 (exit $tsc_rc)" >&2; exit 1; }
echo "deno check used the shipped typescript-check; --check-engine=tsc works"

# The tsgolint `deno lint --type-aware` runs: next to deno, matching the
# sha256 tsgolint_pin.json pins for this target. With an empty DENO_DIR and
# nothing reachable it reports a floating promise; without the flag it does
# not run.
case "$TARGET" in
  *-windows-msvc) tsgolint="$work/cli/tsgolint/tsgolint.exe" ;;
  *) tsgolint="$work/cli/tsgolint/tsgolint" ;;
esac
test -f "$tsgolint" || { echo "::error::no tsgolint/$(basename "$tsgolint") in $ARCHIVE" >&2; exit 1; }
mkdir -p "$work/lint"
printf 'async function work(): Promise<void> {}\nwork();\n' >"$work/lint/floating.ts"
echo "::group::deno lint --type-aware (offline)"
set +e
(cd "$work/lint" && DENO_DIR="$work/lint-deno-dir" NPM_CONFIG_REGISTRY=http://127.0.0.1:9/ \
  "$deno" lint --type-aware --json --no-config floating.ts) >"$work/lint-type-aware.txt" 2>&1
type_aware_rc=$?
(cd "$work/lint" && DENO_DIR="$work/lint-deno-dir" "$deno" lint --no-config floating.ts) >"$work/lint-plain.txt" 2>&1
plain_rc=$?
set -e
cat "$work/lint-type-aware.txt" "$work/lint-plain.txt"
echo "::endgroup::"
test "$type_aware_rc" != 0 && grep -q '"no-floating-promises"' "$work/lint-type-aware.txt" ||
  { echo "::error::deno lint --type-aware did not report no-floating-promises (exit $type_aware_rc)" >&2; exit 1; }
test "$plain_rc" = 0 ||
  { echo "::error::deno lint without --type-aware failed (exit $plain_rc)" >&2; exit 1; }
echo "deno lint --type-aware used the shipped tsgolint"

cp "$here/smoke/cli/vitest_test.ts" "$work/app/"
echo "::group::deno test (vitest)"
# -A as the vitest specs run it: the compatibility layer reads the env.
(cd "$work/app" && "$deno" test -A --no-config --no-lock vitest_test.ts) 2>&1 | tee "$work/test.txt"
echo "::endgroup::"
grep -q "ok | 2 passed" "$work/test.txt" || { echo "::error::the Vitest suite did not pass its 2 tests" >&2; exit 1; }
echo "cli smoke ok: $first"
