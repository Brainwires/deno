#!/usr/bin/env bash
# Copyright 2018-2026 the Deno authors. MIT license.
#
# Links a Linux release build against upstream Deno's own sysroot, an Ubuntu
# 18.04 tree with glibc 2.27, so the binaries run on any glibc >= 2.27 (RHEL /
# Rocky / Alma 8 and 9, Amazon Linux 2023, Debian 10+, Ubuntu 18.04+), whatever
# the runner's glibc is (ubuntu-22.04: 2.35). This is the release build of
# upstream's .github/workflows/ci.ts (`sysRootConfig`): the same sysroot
# release, its `.env` flags, the memfd_create and libm shims in tools/ and the
# same `--wrap`s, linked by clang + lld. It leaves out upstream's linker-plugin
# LTO: the release profile's own LTO is unchanged. See DENEXT.md item 151.
#
# Usage: linux_sysroot.sh <target triple>
#   Installs clang + lld, unpacks the sysroot (checked against its pinned
#   SHA-256) at /sysroot, mounts /dev, /sys, /proc and /home into it (so a
#   built binary can be run inside it with `chroot /sysroot`), compiles the
#   shims, and writes the build environment (CC, CFLAGS and the target's cargo
#   linker and rustflags) to $GITHUB_ENV, or prints it when that is unset.
#   The target's rustflags join .cargo/config.toml's (--cfg tokio_unstable),
#   which a RUSTFLAGS variable would replace.
#
# The sysroot is native only: it runs on an x86_64 host for
# x86_64-unknown-linux-gnu and on an aarch64 host for aarch64-unknown-linux-gnu.

set -euo pipefail

target=${1:?usage: linux_sysroot.sh <target triple>}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

# denoland/deno_sysroot_build, the release upstream's ci.ts pins.
SYSROOT_RELEASE=sysroot-20250207
# Ubuntu 22.04's own clang and lld (no third-party apt source).
LLVM_VERSION=15
GLIBC_EXPECTED=2.27

case "$target" in
  x86_64-unknown-linux-gnu)
    arch=x86_64
    sysroot_sha256=64513b04d8a38da6f47c0a9c4cb47b2ce7bcb735250b10eb10751aec6afe46ee
    ;;
  aarch64-unknown-linux-gnu)
    arch=aarch64
    sysroot_sha256=0a29c63c52cc1e59228686602cf3cf41080a809655afc29d1b8a27af4b20cedb
    ;;
  *)
    echo "::error::linux_sysroot.sh: no sysroot for $target" >&2
    exit 1
    ;;
esac
if [ "$(uname -m)" != "$arch" ]; then
  echo "::error::linux_sysroot.sh: $target needs an $arch host, this is $(uname -m)" >&2
  exit 1
fi

sudo=
if [ "$(id -u)" != 0 ]; then sudo=sudo; fi

export DEBIAN_FRONTEND=noninteractive
$sudo apt-get update -qq
$sudo apt-get install -y --no-install-recommends "clang-$LLVM_VERSION" "lld-$LLVM_VERSION"
cc="clang-$LLVM_VERSION"
"$cc" --version | head -1
"ld.lld-$LLVM_VERSION" --version

tmp=${RUNNER_TEMP:-}
if [ -z "$tmp" ]; then tmp=$(mktemp -d); fi
archive="$tmp/sysroot-$arch.tar.xz"
curl -fsSL --retry 3 -o "$archive" \
  "https://github.com/denoland/deno_sysroot_build/releases/download/$SYSROOT_RELEASE/sysroot-$arch.tar.xz"
echo "$sysroot_sha256  $archive" | sha256sum -c -
$sudo rm -rf /sysroot
$sudo tar -xJf "$archive" -C /
rm -f "$archive"

# The glibc the binaries are linked against, which is what GLIBC_CEILING in
# denext_runtime.yml asserts.
libc=$(cd "/sysroot/lib/$arch-linux-gnu" && ls libc-*.so)
glibc=${libc#libc-}
glibc=${glibc%.so}
echo "sysroot: $SYSROOT_RELEASE ($(. /sysroot/etc/os-release && echo "$PRETTY_NAME"), glibc $glibc)"
if [ "$glibc" != "$GLIBC_EXPECTED" ]; then
  echo "::error::the sysroot's glibc is $glibc, not $GLIBC_EXPECTED" >&2
  exit 1
fi

for d in dev sys home; do
  $sudo mount --rbind "/$d" "/sysroot/$d"
done
$sudo mount -t proc /proc /sysroot/proc

shims="$tmp/glibc-shims"
mkdir -p "$shims"
"$cc" -c -fPIC -o "$shims/memfd_create_shim.o" "$root/tools/memfd_create_shim.c"
"$cc" -c -fPIC -o "$shims/glibc_math_shim.o" "$root/tools/glibc_math_shim.c"

echo "sysroot env:"
cat /sysroot/.env
# Sets RUSTFLAGS and CFLAGS (-L the sysroot's lib dirs, --sysroot=/sysroot).
RUSTFLAGS=
CFLAGS=
# shellcheck disable=SC1091
. /sysroot/.env
sysroot_rustflags=$RUSTFLAGS
sysroot_cflags=$CFLAGS
unset RUSTFLAGS CFLAGS

# -ldl and --allow-shlib-undefined as upstream links. The shim objects and
# --wraps keep memfd_create and five libm functions off newer symbol versions
# (tools/glibc_math_shim.c).
rustflags="-C link-arg=-fuse-ld=lld-$LLVM_VERSION
  -C link-arg=-ldl
  -C link-arg=-Wl,--allow-shlib-undefined
  -C link-arg=$shims/memfd_create_shim.o
  -C link-arg=$shims/glibc_math_shim.o
  -C link-arg=-Wl,--wrap=expf
  -C link-arg=-Wl,--wrap=powf
  -C link-arg=-Wl,--wrap=exp2f
  -C link-arg=-Wl,--wrap=log2f
  -C link-arg=-Wl,--wrap=logf
  $sysroot_rustflags"
rustflags=$(echo $rustflags)

var=CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')
env_file=${GITHUB_ENV:-/dev/stdout}
{
  echo "CC=$cc"
  echo "CFLAGS=$sysroot_cflags"
  echo "${var}_LINKER=$cc"
  echo "${var}_RUSTFLAGS=$rustflags"
} >>"$env_file"
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "build environment:"
  tail -4 "$GITHUB_ENV"
fi
