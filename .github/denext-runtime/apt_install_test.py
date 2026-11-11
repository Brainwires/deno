#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for apt_install.sh against stand-in dpkg-query / dpkg / apt-get /
sudo / timeout / sleep commands: nothing is fetched when every package is
installed, apt runs with retries and timeouts, a failed try is repeated, and
the third try moves the Ubuntu sources to the fallback mirror.

Usage: python3 -I .github/denext-runtime/apt_install_test.py
"""

import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "apt_install.sh")

STUBS = {
    # Installed packages are listed in $INSTALLED (space separated).
    "dpkg-query": r"""#!/usr/bin/env bash
pkg="${@: -1}"
for p in $INSTALLED; do [ "$p" = "$pkg" ] && { printf installed; exit 0; }; done
exit 1
""",
    "dpkg": "#!/usr/bin/env bash\necho amd64\n",
    # Logs every call; `update` fails while $FAIL_UPDATES (a counter file) > 0.
    "apt-get": r"""#!/usr/bin/env bash
echo "apt-get $*" >>"$LOG"
for a in "$@"; do
  if [ "$a" = update ]; then
    n=$(cat "$FAIL_UPDATES")
    if [ "$n" -gt 0 ]; then echo $((n - 1)) >"$FAIL_UPDATES"; exit 100; fi
  fi
done
exit 0
""",
    "sudo": '#!/usr/bin/env bash\n"$@"\n',
    "timeout": '#!/usr/bin/env bash\necho "timeout $1" >>"$LOG"\nshift\n"$@"\n',
    "sleep": "#!/usr/bin/env bash\nexit 0\n",
}

SOURCES = (
    "Types: deb\n"
    "URIs: http://azure.archive.ubuntu.com/ubuntu/\n"
    "Suites: noble noble-updates\n\n"
    "Types: deb\n"
    "URIs: http://security.ubuntu.com/ubuntu/\n"
    "Suites: noble-security\n"
)


class AptInstall(unittest.TestCase):
    def run_script(self, args, installed="", fail_updates=0, tries=None):
        tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(tmp, "bin")
        os.makedirs(bin_dir)
        for name, body in STUBS.items():
            path = os.path.join(bin_dir, name)
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        etc = os.path.join(tmp, "etc")
        os.makedirs(os.path.join(etc, "sources.list.d"))
        sources = os.path.join(etc, "sources.list.d", "ubuntu.sources")
        with open(sources, "w") as f:
            f.write(SOURCES)
        log = os.path.join(tmp, "log")
        open(log, "w").close()
        counter = os.path.join(tmp, "fail")
        with open(counter, "w") as f:
            f.write(str(fail_updates))
        env = {
            "PATH": bin_dir + os.pathsep + os.environ.get("PATH", ""),
            "INSTALLED": installed,
            "LOG": log,
            "FAIL_UPDATES": counter,
            "APT_ETC": etc,
            "APT_INSTALL_DELAY": "1",
        }
        if tries:
            env["APT_INSTALL_TRIES"] = str(tries)
        result = subprocess.run(
            ["bash", SCRIPT, *args], env=env, capture_output=True, text=True
        )
        with open(log) as f:
            calls = f.read().splitlines()
        with open(sources) as f:
            return result, calls, f.read()

    def test_everything_installed_fetches_nothing(self):
        result, calls, _ = self.run_script(["cmake", "libclang-dev"], "cmake libclang-dev")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [])
        self.assertIn("nothing to fetch", result.stdout)

    def test_installs_only_what_is_missing_with_retries_and_timeouts(self):
        result, calls, sources = self.run_script(["cmake", "ninja-build"], "cmake")
        self.assertEqual(result.returncode, 0, result.stderr)
        install = [c for c in calls if " install " in c and "remove" not in c]
        self.assertEqual(len(install), 1)
        self.assertTrue(install[0].endswith("install -y --no-install-recommends ninja-build"))
        self.assertIn("-o Acquire::Retries=5", install[0])
        self.assertIn("-o Acquire::http::Timeout=30", install[0])
        self.assertIn("timeout 600", calls)
        self.assertIn("timeout 1200", calls)
        self.assertIn("azure.archive.ubuntu.com", sources)

    def test_with_recommends(self):
        result, calls, _ = self.run_script(["--with-recommends", "cmake"])
        self.assertEqual(result.returncode, 0, result.stderr)
        install = [c for c in calls if c.endswith("install -y cmake")]
        self.assertEqual(len(install), 1, calls)

    def test_a_failed_update_is_retried(self):
        result, calls, sources = self.run_script(["cmake"], fail_updates=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(1 for c in calls if c.endswith("update -q")), 2)
        self.assertIn("try 1 of 4 failed", result.stdout)
        # Two tries on the image's own mirror: the sources are untouched.
        self.assertIn("azure.archive.ubuntu.com", sources)

    def test_the_third_try_uses_the_fallback_mirror(self):
        result, _, sources = self.run_script(["cmake"], fail_updates=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("apt sources now use http://archive.ubuntu.com/ubuntu", result.stdout)
        self.assertNotIn("azure.", sources)
        self.assertNotIn("security.ubuntu.com", sources)
        self.assertIn("URIs: http://archive.ubuntu.com/ubuntu/\n", sources)

    def test_the_fourth_try_uses_the_next_mirror(self):
        result, _, sources = self.run_script(["cmake"], fail_updates=3)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("URIs: http://mirrors.edge.kernel.org/ubuntu/\n", sources)

    def test_gives_up_after_the_last_try(self):
        result, calls, _ = self.run_script(["cmake"], fail_updates=9, tries=3)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(sum(1 for c in calls if c.endswith("update -q")), 3)
        self.assertIn("::error::apt_install.sh: could not install cmake after 3 tries", result.stderr)

    def test_no_packages_is_an_error(self):
        result, _, _ = self.run_script([])
        self.assertEqual(result.returncode, 1)
        self.assertIn("no packages named", result.stderr)


if __name__ == "__main__":
    unittest.main()
