#!/usr/bin/env bash
# Installs the ld64.lld that the aarch64-apple-darwin rustflags in
# .cargo/config.toml link with (-fuse-ld=lld, as deno's release does), from
# denoland/deno_third_party at the commit tools/util.js pins, and checks it
# against the SHA-256 below before anything links with it: the release jobs
# that run this hold id-token and attestation rights, so a substituted linker
# would land in attested binaries. Adds its directory to GITHUB_PATH.
#
# When tools/util.js moves to another deno_third_party commit, update both
# values (the hash is of the decompressed binary).
set -euo pipefail

LD64_COMMIT=e593815dcfe5d260b27e6556cc9e44dcfcaeda3d
LD64_SHA256=2a4d2e121c4a08ebad474a6ec93516118c2a9ce322f68e686e00e7ab73adc584

source "$(dirname "$0")/lib.sh"

grep -q "const commitId = \"$LD64_COMMIT\";" tools/util.js || {
  echo "::error::tools/util.js no longer pins deno_third_party $LD64_COMMIT: update LD64_COMMIT and LD64_SHA256 in $0"
  exit 1
}
# tools/util.js imports the std submodule.
git submodule update --init --depth=1 -- ./tests/util/std
./tools/install_prebuilt.js ld64.lld
bin=third_party/prebuilt/mac/ld64.lld
got=$(sha256_of "$bin")
if [ "$got" != "$LD64_SHA256" ]; then
  echo "::error::$bin has SHA-256 $got, expected $LD64_SHA256"
  rm -f "$bin"
  exit 1
fi
echo "ld64.lld $got verified"
echo "$GITHUB_WORKSPACE/third_party/prebuilt/mac" >>"$GITHUB_PATH"
