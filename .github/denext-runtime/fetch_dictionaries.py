#!/usr/bin/env python3
"""Fetch the Hunspell spellcheck dictionaries the Linux CEF runtime ships
(assemble.sh), or check a directory of them (verify.sh).

The Linux CEF host never lets Chromium download a dictionary from Google
(laufey's network-quiet defaults), so without these a fresh profile has no
spellcheck. The host copies the dictionaries of the languages in use from
`laufey/cef/build/Release/dictionaries/` (beside the host executable) into the
profile before the first browser (laufey `DictionariesToSeed`); an app adds or
replaces its own in that folder when it packages. macOS and Windows use the
OS spellchecker and get none.

The files are Chromium's prebuilt `.bdic` files from
chromium/deps/hunspell_dictionaries at the commit the CEF's Chromium pins in
its DEPS (CEF 150.0.21 = Chromium 150.0.7871.255; Chromium 149's DEPS pinned the
same commit), so each name carries the
version that Chromium asks for (`spellcheck_common.cc`,
`GetVersionedFileName`). Each is pinned by SHA-256 here; a download that does
not match fails. The license README of each dictionary and the repository's
LICENSE go into the archive's licenses/.

Usage:
  fetch_dictionaries.py <dictionaries dir> <licenses dir>   fetch + verify
  fetch_dictionaries.py --check <dictionaries dir>          verify only
"""

import base64
import hashlib
import os
import sys
import time
import urllib.request

REPO = "https://chromium.googlesource.com/chromium/deps/hunspell_dictionaries"
# src/third_party/hunspell_dictionaries in Chromium 150.0.7871.255's DEPS.
COMMIT = "cccf64a8acc951afe3f47fee023908e55699bc58"

# The common languages: file -> SHA-256.
DICTIONARIES = {
    "en-US-10-1.bdic": "a075b01d9b015c616511a9e87da77da3d9881621db32f584e4606ddabf1c1100",
    "en-GB-10-1.bdic": "eaed4a709abdbd2f14aa143f25fe9c4c1b44591c7cb1942f75f968fb2fcd3cf8",
    "de-DE-3-0.bdic": "076a470700dbd0aa62bb8b3d24c34e1340bdfe83cbbcfc035ce972f23c140400",
    "fr-FR-3-0.bdic": "6cffc13b549c33d44180a3a7b13943fe0eadfbe80bcdbccc76f67c55c2012968",
    "es-ES-3-0.bdic": "e6334dcf080aaeca679db70565762a2c296ff5780c1af263530ac7345736bfa9",
    "pt-BR-3-0.bdic": "6b2850f5a54994a5204a9a88d4b586e9d4e028a0360b67352b04cffdb2a3e0ea",
    "it-IT-3-0.bdic": "8b83a0d4bba4cfcd4a4e7ee89b703014a00a5c08e721e568a5c552a732827fff",
}

# Each dictionary's license and origin, and the repository's LICENSE.
LICENSES = {
    "LICENSE": "d63c6240d6f6330a025aade63d317d7ff04f95fc3325fe6aea3848ca452f64ea",
    "README_en_US.txt": "168b4c01cb841f766a72f310b065c04d09cff46376c37a81d799593ce751c371",
    "README_en_GB.txt": "0bcf1616b29dee3fbcbef1428b0c3d4d77f6a36cc88b9e130ee65e5382899029",
    "README_de_DE.txt": "b45a23299baa504f16579dd701b5b5e9437d747261436a0f23259e1d114e5d0f",
    "README_fr_FR.txt": "c22ed899c73989a145655cf7401422a5a682e88da0574e4a8e8c9c1b274b9b60",
    "README_es_ES.txt": "fd1c372edc60847a4c60b5a368864e760e39117adb0a5a80a4bf96a128620f2a",
    "README_pt_BR.txt": "36383a89f3f98b5d7b2b7a7620d46ed5867dafb5b1bb2c1c11a15eddb29fdb6a",
    "README_it_IT.txt": "c2b8b4b661fb6e2187a0b083fd3e6898f4f1a470d2885f4170708a5dd932d21c",
}


def get(name):
    # Gitiles serves a file's bytes base64-encoded with ?format=TEXT.
    url = f"{REPO}/+/{COMMIT}/{name}?format=TEXT"
    last = None
    for attempt in range(1, 4):
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                return base64.b64decode(r.read())
        except Exception as e:  # noqa: BLE001 - retried, then raised
            last = e
            print(f"GET {url} failed (attempt {attempt}/3): {e}", file=sys.stderr)
            time.sleep(3 * attempt)
    raise SystemExit(f"could not download {url}: {last}")


def fetch(files, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    for name, want in files.items():
        data = get(name)
        got = hashlib.sha256(data).hexdigest()
        if got != want:
            raise SystemExit(f"{name}: sha256 {got}, pinned {want}")
        with open(os.path.join(out_dir, name), "wb") as f:
            f.write(data)
        print(f"{name}: {len(data)} bytes, sha256 {got}")


def check(out_dir):
    failed = False
    present = sorted(n for n in os.listdir(out_dir)) if os.path.isdir(out_dir) else []
    for name, want in DICTIONARIES.items():
        path = os.path.join(out_dir, name)
        if not os.path.isfile(path):
            print(f"ERROR: {name} missing from {out_dir}", file=sys.stderr)
            failed = True
            continue
        with open(path, "rb") as f:
            got = hashlib.sha256(f.read()).hexdigest()
        ok = got == want
        print(f"{'ok  ' if ok else 'FAIL'} {name} {got}")
        failed = failed or not ok
    extra = [n for n in present if n not in DICTIONARIES]
    if extra:
        print(f"ERROR: unexpected files in {out_dir}: {extra}", file=sys.stderr)
        failed = True
    if failed:
        raise SystemExit(1)


def main():
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "--check":
        check(args[1])
    elif len(args) == 2 and not args[0].startswith("-"):
        fetch(DICTIONARIES, args[0])
        fetch(LICENSES, args[1])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
