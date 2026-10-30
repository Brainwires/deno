#!/usr/bin/env python3
"""Tests for symbolicate.py, the maintainer tool that turns a release's
`deno_panic` trace into functions and lines: the VLQ trace decodes into
addresses as deno_panic encodes them, the archives and debug files are the
release's names, llvm-symbolizer gets each platform's address space, and
`check-release` flags a release's missing or empty symbol archives (as
denext.2's macOS ones were). The last cases run the script.

Usage: python3 -I .github/denext-runtime/symbolicate_test.py
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "symbolicate.py")

spec = importlib.util.spec_from_file_location("symbolicate", SCRIPT)
sym = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sym)

V = "3.0.0-denext.2"
# The symbol archives of denext.2 as released (sizes in bytes): the macOS
# ones held only a dangling `.dSYM` symlink.
DENEXT2 = {
    f"deno-desktop-runtime-symbols-{V}-aarch64-apple-darwin.tar.gz": 125,
    f"deno-desktop-runtime-symbols-{V}-aarch64-pc-windows-msvc.tar.gz": 45212168,
    f"deno-desktop-runtime-symbols-{V}-aarch64-unknown-linux-gnu.tar.gz": 16206639,
    f"deno-desktop-runtime-symbols-{V}-x86_64-apple-darwin.tar.gz": 125,
    f"deno-desktop-runtime-symbols-{V}-x86_64-pc-windows-msvc.tar.gz": 46528945,
    f"deno-desktop-runtime-symbols-{V}-x86_64-unknown-linux-gnu.tar.gz": 17977769,
    f"deno-symbols-{V}-aarch64-apple-darwin.tar.gz": 173,
    f"deno-symbols-{V}-aarch64-pc-windows-msvc.tar.gz": 104511442,
    f"deno-symbols-{V}-aarch64-unknown-linux-gnu.tar.gz": 44855535,
    f"deno-symbols-{V}-x86_64-apple-darwin.tar.gz": 173,
    f"deno-symbols-{V}-x86_64-pc-windows-msvc.tar.gz": 107213017,
    f"deno-symbols-{V}-x86_64-unknown-linux-gnu.tar.gz": 50669145,
    "SYMBOLS.SHA256SUMS": 1500,
    "SHA256SUMS": 4000,
}


class Vlq(unittest.TestCase):
    def test_decodes_a_fixture_trace(self):
        # 0x1234 = "ojJ" by hand: 0x1234 << 1 = 9320 = 8 + 3*32 + 9*1024,
        # digits 8|32 ('o'), 3|32 ('j'), 9 ('J').
        self.assertEqual(sym.vlq_decode("ojJ"), [0x1234])
        self.assertEqual(sym.vlq_decode("ojJAC"), [0x1234, 0, 1])
        self.assertEqual(sym.vlq_decode("D"), [-1])

    def test_round_trips_image_offsets(self):
        addrs = [0, 1, 31, 32, 0x3F1A2C, 0x7FFF_FFFF, -5]
        trace = "".join(sym.vlq_encode(a) for a in addrs)
        self.assertEqual(sym.vlq_decode(trace), addrs)

    def test_rejects_garbage(self):
        with self.assertRaises(ValueError):
            sym.vlq_decode("oj!")
        with self.assertRaises(ValueError):
            sym.vlq_decode("oj")  # ends inside a number


class Names(unittest.TestCase):
    def test_archives(self):
        self.assertEqual(
            sym.archives("3.0.0-denext.4", "deno", "x86_64-unknown-linux-gnu"),
            ("deno-cli-3.0.0-denext.4-x86_64-unknown-linux-gnu.zip",
             "deno-symbols-3.0.0-denext.4-x86_64-unknown-linux-gnu.tar.gz"),
        )
        self.assertEqual(
            sym.archives("3.0.0-denext.4", "denort", "aarch64-apple-darwin")[0],
            "deno-denort-3.0.0-denext.4-aarch64-apple-darwin.zip",
        )
        self.assertEqual(
            sym.archives("3.0.0-denext.4", "libdenort", "x86_64-pc-windows-msvc"),
            ("deno-desktop-runtime-3.0.0-denext.4-x86_64-pc-windows-msvc-webview.zip",
             "deno-desktop-runtime-symbols-3.0.0-denext.4-x86_64-pc-windows-msvc.tar.gz"),
        )

    def test_debug_files(self):
        self.assertEqual(sym.debug_file("deno", "x86_64-unknown-linux-gnu"), "deno.dwp")
        self.assertEqual(sym.debug_file("libdenort", "aarch64-unknown-linux-gnu"), "libdenort.dwp")
        self.assertEqual(sym.debug_file("deno", "x86_64-pc-windows-msvc"), "deno.pdb")
        self.assertEqual(sym.debug_file("libdenort", "x86_64-pc-windows-msvc"), "denort.pdb")
        self.assertEqual(
            sym.debug_file("deno", "aarch64-apple-darwin"),
            "deno.dSYM/Contents/Resources/DWARF",
        )

    def test_find_takes_a_dsyms_one_dwarf_file(self):
        with tempfile.TemporaryDirectory() as d:
            dwarf = os.path.join(d, "deno.dSYM", "Contents", "Resources", "DWARF")
            os.makedirs(dwarf)
            open(os.path.join(dwarf, "deno-ef4cf40cd73adfa2"), "w").close()
            self.assertEqual(
                sym.find(d, sym.debug_file("deno", "aarch64-apple-darwin")),
                os.path.join(dwarf, "deno-ef4cf40cd73adfa2"),
            )


class Symbolizer(unittest.TestCase):
    def test_linux_uses_the_dwp(self):
        cmd = sym.symbolizer_command("llvm-symbolizer", "x86_64-unknown-linux-gnu", "/x/deno", "/x/deno.dwp", [0x10, 0x20])
        self.assertIn("--obj=/x/deno", cmd)
        self.assertIn("--dwp=/x/deno.dwp", cmd)
        self.assertEqual(cmd[-2:], ["0x10", "0x20"])

    def test_macos_adds_the_text_vmaddr(self):
        cmd = sym.symbolizer_command("llvm-symbolizer", "aarch64-apple-darwin", "/x/deno", "/x/DWARF/deno", [0x10])
        self.assertIn("--obj=/x/DWARF/deno", cmd)
        self.assertEqual(cmd[-1], "0x100000010")

    def test_windows_addresses_are_relative(self):
        cmd = sym.symbolizer_command("llvm-symbolizer", "x86_64-pc-windows-msvc", "C:/x/deno.exe", "C:/x/deno.pdb", [0x10])
        self.assertIn("--relative-address", cmd)
        self.assertIn("--obj=C:/x/deno.exe", cmd)
        self.assertEqual(cmd[-1], "0x10")


class CheckRelease(unittest.TestCase):
    def test_flags_denext2s_empty_macos_archives(self):
        problems = sym.check_release(V, DENEXT2)
        self.assertEqual(len(problems), 4, problems)
        for p in problems:
            self.assertTrue(p.startswith("empty: "), p)
            self.assertIn("apple-darwin", p)

    def test_flags_a_missing_archive_and_checksums(self):
        assets = {k: v for k, v in DENEXT2.items() if "x86_64-unknown-linux-gnu" not in k or "desktop" in k}
        assets = {k: (v if v > 1000 else 10 << 20) for k, v in assets.items() if k != "SYMBOLS.SHA256SUMS"}
        problems = sym.check_release(V, assets)
        self.assertIn(f"missing: deno-symbols-{V}-x86_64-unknown-linux-gnu.tar.gz", problems)
        self.assertIn("missing: SYMBOLS.SHA256SUMS", problems)
        self.assertEqual(len(problems), 2, problems)

    def test_a_complete_release_passes(self):
        assets = {k: (v if v > 1000 else 10 << 20) for k, v in DENEXT2.items()}
        self.assertEqual(sym.check_release(V, assets), [])


class Script(unittest.TestCase):
    def run_script(self, *args):
        return subprocess.run([sys.executable, "-I", SCRIPT, *args], capture_output=True, text=True)

    def test_addresses(self):
        r = self.run_script("3.0.0-denext.4", "x86_64-unknown-linux-gnu", "ojJAC", "--addresses")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.split(), ["0x1234", "0x0", "0x1"])

    def test_check_release_from_a_file(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "assets.json")
            with open(path, "w") as f:
                json.dump({"assets": [{"name": k, "size": v} for k, v in DENEXT2.items()]}, f)
            r = self.run_script("check-release", V, "--assets", path)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertEqual(r.stdout.count("empty: "), 4)

    def test_check_release_from_a_directory(self):
        with tempfile.TemporaryDirectory() as d:
            for name in DENEXT2:
                with open(os.path.join(d, name), "wb") as f:
                    f.write(b"x" * (70 * 1024))
            r = self.run_script("check-release", V, "--dir", d)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            os.remove(os.path.join(d, f"deno-symbols-{V}-x86_64-unknown-linux-gnu.tar.gz"))
            r = self.run_script("check-release", V, "--dir", d)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn(f"missing: deno-symbols-{V}-x86_64-unknown-linux-gnu.tar.gz", r.stdout)

    def test_from_a_directory_checks_the_checksum(self):
        with tempfile.TemporaryDirectory() as d:
            for name in ("deno-cli-9.9.9-x86_64-unknown-linux-gnu.zip",
                         "deno-symbols-9.9.9-x86_64-unknown-linux-gnu.tar.gz"):
                with open(os.path.join(d, name), "wb") as f:
                    f.write(b"not what the checksum says")
            with open(os.path.join(d, "SHA256SUMS"), "w") as f:
                f.write("0" * 64 + "  deno-cli-9.9.9-x86_64-unknown-linux-gnu.zip\n")
            r = self.run_script("9.9.9", "x86_64-unknown-linux-gnu", "ojJ", "--from", d)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("SHA-256", r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
