#!/usr/bin/env bash
# Build the Node-API probe addon (probe.c) for this machine.
#
# Usage: build.sh <output .node path>
#
# macOS: Node-API symbols left undefined (`-undefined dynamic_lookup`).
# Linux: undefined symbols allowed in a shared object by default.
# Windows (MSVC on PATH, e.g. from a VS developer shell): the node-gyp setup,
#   an import library for `node.exe` and `/DELAYLOAD:node.exe` with the
#   delay-load hook in probe.c. Options use the `-` spelling so MSYS bash does
#   not rewrite them as paths.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: build.sh <output .node>}
mkdir -p "$(dirname "$out")"

case "$(uname -s)" in
  Darwin)
    cc -O2 -Wall -bundle -undefined dynamic_lookup -o "$out" "$here/probe.c"
    ;;
  Linux)
    cc -O2 -Wall -shared -fPIC -o "$out" "$here/probe.c"
    ;;
  MINGW* | MSYS* | CYGWIN*)
    tmp=$(mktemp -d)
    cp "$here/probe.c" "$here/node.def" "$tmp/"
    (
      cd "$tmp"
      lib -nologo -def:node.def -name:node.exe -machine:x64 -out:node.lib
      cl -nologo -O2 -W3 -LD probe.c -Fe:probe.node \
        -link node.lib delayimp.lib -DELAYLOAD:node.exe
    )
    cp "$tmp/probe.node" "$out"
    rm -rf "$tmp"
    ;;
  *)
    echo "unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac
echo "built $out"
