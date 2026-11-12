#!/usr/bin/env bash
# Copyright 2018-2026 the Deno authors. MIT license.
# Writes <dir>/<tool> wrappers that run the Command Line Tools' /usr/bin
# shims (make, cc, ar, xcrun, ...) as arm64, for an x86_64-apple-darwin build
# on an arm64 Mac with the x86_64 Rust toolchain under Rosetta 2
# (plan_matrix.py `rosetta`). Put <dir> first on PATH.
#
# Every process below an x86_64 one prefers the x86_64 slice of a universal
# binary, even across an arm64-only program (cmake). The /usr/bin shims are
# universal, but load the CLT's libxcrun, which recent Command Line Tools
# ship without an x86_64 slice ("unable to load libxcrun ... need
# 'x86_64'"), so cargo's `cc`, the cc crate, cmake's `make` and libffi's
# configure all fail. The wrappers start the shim's arm64e slice; what they
# build still follows its flags (`-arch x86_64`, --target), so the output
# stays x86_64 (the build jobs check it with lipo).
#
# CMake would still run /usr/bin/make by its absolute path, so with <env
# file> ($GITHUB_ENV) it also selects the Ninja generator (Homebrew's ninja,
# arm64; the mac-arm pool's image has it) for the cmake crate.
#
# bindgen in an x86_64 build script (libsqlite3-sys, ...) dlopens libclang,
# and the CLT's is arm64 only: with <env file> it also fetches the x86_64
# libclang of the PyPI `libclang` wheel below, checked against its pinned
# SHA-256, and sets LIBCLANG_PATH to it (the arm64 clang still answers
# bindgen's include path query through the wrappers).
#
# On an Intel Mac (the overflow to macos-15-intel) the build is native: no
# wrappers, nothing written to <env file>.
#
#   rosetta_tools.sh <dir> [<env file>]
set -euo pipefail
dir=${1:?usage: rosetta_tools.sh <dir> [<env file>]}
env_file=${2:-}
LIBCLANG_WHEEL_URL=https://files.pythonhosted.org/packages/e2/e5/fc61bbded91a8830ccce94c5294ecd6e88e496cc85f6704bf350c0634b70/libclang-18.1.1-py2.py3-none-macosx_10_9_x86_64.whl
LIBCLANG_WHEEL_SHA256=6f14c3f194704e5d09769108f03185fce7acaf1d1ae4bbb2f30a72c2400cb7c5
mkdir -p "$dir"
if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" != 1 ]; then
  echo "rosetta_tools.sh: not an arm64 Mac, no wrappers"
  exit 0
fi
n=0
for tool in make gnumake cc c++ clang clang++ gcc g++ ld ar ranlib libtool nm \
  strip lipo install_name_tool otool xcrun xcodebuild dsymutil; do
  [ -x "/usr/bin/$tool" ] || continue
  printf '#!/bin/sh\nexec /usr/bin/arch -arm64e /usr/bin/%s "$@"\n' "$tool" >"$dir/$tool"
  chmod +x "$dir/$tool"
  n=$((n + 1))
done
echo "rosetta_tools.sh: $n wrappers in $dir"
if [ -n "$env_file" ]; then
  ninja=$(command -v ninja) || { echo "::error::rosetta_tools.sh: no ninja on PATH (CMake would run /usr/bin/make as x86_64)" >&2; exit 1; }
  echo "CMAKE_GENERATOR=Ninja" >>"$env_file"
  echo "rosetta_tools.sh: CMAKE_GENERATOR=Ninja ($ninja)"
  whl="$dir/../libclang-x86_64.whl"
  curl -fsSL --retry 3 -o "$whl" "$LIBCLANG_WHEEL_URL"
  got=$(shasum -a 256 "$whl" | cut -d' ' -f1)
  if [ "$got" != "$LIBCLANG_WHEEL_SHA256" ]; then
    echo "::error::rosetta_tools.sh: the libclang wheel has SHA-256 $got, expected $LIBCLANG_WHEEL_SHA256" >&2
    rm -f "$whl"
    exit 1
  fi
  lib="$dir/../libclang-x86_64"
  rm -rf "$lib"
  unzip -q -j "$whl" '*/clang/native/libclang.dylib' -d "$lib"
  file "$lib/libclang.dylib"
  echo "LIBCLANG_PATH=$lib" >>"$env_file"
  echo "rosetta_tools.sh: LIBCLANG_PATH=$lib"
fi
