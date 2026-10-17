#!/usr/bin/env bash
# Smoke-test a `deno` CLI archive from assemble_cli.sh: unpack it and check
# that the binary names its denext build (`deno --version`, `deno -V` and
# `Deno.version.deno`), has `Deno.SQL`, and runs a Vitest suite under `deno test`
# (smoke/cli/vitest_test.ts); that the bundled esbuild is ESBUILD_VERSION and
# `deno bundle` uses it with an empty DENO_DIR and no reachable npm registry.
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

# Deno.version.deno is the denext version too (denext tells its own CLI by
# the `-denext.` in it), and still semver.
deno_version=$("$deno" eval 'console.log(Deno.version.deno)' | tr -d '\r')
test "$deno_version" = "$VERSION" || { echo "::error::Deno.version.deno is '$deno_version', want '$VERSION'" >&2; exit 1; }
echo "Deno.version.deno: $deno_version"
case "$deno_version" in
  *-denext.*) ;;
  *) echo "::error::Deno.version.deno '$deno_version' has no -denext. suffix" >&2; exit 1 ;;
esac
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

cp "$here/smoke/cli/vitest_test.ts" "$work/app/"
echo "::group::deno test (vitest)"
# -A as the vitest specs run it: the compatibility layer reads the env.
(cd "$work/app" && "$deno" test -A --no-config --no-lock vitest_test.ts) 2>&1 | tee "$work/test.txt"
echo "::endgroup::"
grep -q "ok | 2 passed" "$work/test.txt" || { echo "::error::the Vitest suite did not pass its 2 tests" >&2; exit 1; }
echo "cli smoke ok: $first"
