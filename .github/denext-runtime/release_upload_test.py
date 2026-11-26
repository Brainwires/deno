#!/usr/bin/env python3
# Copyright 2018-2026 the Deno authors. MIT license.
"""Tests for release_upload.sh against a stand-in `gh` that keeps the
repository's releases in a JSON file: a new release is a draft until every
asset is on it, a failed upload (one GitHub stored anyway, or a half-stored
one) is retried with only what is missing, a re-run reuses the tag's draft or
finds the published release complete, and a published release is never
changed.

Usage: python3 -I .github/denext-runtime/release_upload_test.py
"""

import json
import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "release_upload.sh")

# The stand-in gh: the state is {"releases": [...], "next_id": n,
# "upload_faults": [...]}; each upload pops one fault: "ok", "fail" (stores
# nothing), "stored_then_fail" (stores every file, then fails, as on the
# denext.30 run) or "starter" (leaves a half-stored asset of the first file).
GH = r'''#!/usr/bin/env python3
import json, os, sys
state_path = os.environ["GH_STATE"]
with open(state_path) as f:
    state = json.load(f)
with open(os.environ["LOG"], "a") as f:
    f.write("gh " + " ".join(sys.argv[1:]) + "\n")
args = sys.argv[1:]

def save():
    with open(state_path, "w") as f:
        json.dump(state, f)

def new_id():
    state["next_id"] += 1
    return state["next_id"]

def release_by_tag(tag):
    hits = [r for r in state["releases"] if r["tag_name"] == tag]
    if not hits:
        sys.exit("release not found")
    return hits[0]

def flag(name):
    return args[args.index(name) + 1] if name in args else None

if args[0] == "api":
    rest = [a for a in args[1:] if a != "--paginate"]
    method = "GET"
    if rest[0] == "-X":
        method, rest = rest[1], rest[2:]
    path = rest[0].split("?")[0]
    parts = path.split("/")
    if method == "GET" and parts[-1] == "releases":
        for r in state["releases"]:
            print(f"{r['id']}\t{str(r['draft']).lower()}\t{r['tag_name']}")
    elif method == "GET" and parts[-1] == "assets":
        rid = int(parts[-2])
        for r in state["releases"]:
            if r["id"] == rid:
                for a in r["assets"]:
                    print(f"{a['name']}\t{a['size']}\t{a['state']}\t{a['id']}")
    elif method == "DELETE":
        aid = int(parts[-1])
        for r in state["releases"]:
            r["assets"] = [a for a in r["assets"] if a["id"] != aid]
    elif method == "PATCH":
        rid = int(parts[-1])
        fields = {}
        for i, a in enumerate(rest):
            if a in ("-F", "-f"):
                k, v = rest[i + 1].split("=", 1)
                if v.startswith("@"):
                    with open(v[1:]) as f:
                        v = f.read()
                fields[k] = v
        for r in state["releases"]:
            if r["id"] == rid:
                if not r["draft"]:
                    sys.exit("editing a published release")
                r["draft"] = fields["draft"] == "true"
                r["prerelease"] = fields["prerelease"] == "true"
                r["make_latest"] = fields["make_latest"]
                r["name"] = fields["name"]
                r["body"] = fields["body"]
                print(f"published https://example.invalid/{r['tag_name']}")
    else:
        sys.exit("unexpected api call")
elif args[:2] == ["release", "create"]:
    tag = args[2]
    if "--draft" not in args or "--verify-tag" not in args:
        sys.exit("not a draft on the existing tag")
    if [a for a in args[3:] if os.path.isfile(a) and a != flag("--notes-file")]:
        sys.exit("create was given assets")
    with open(flag("--notes-file")) as f:
        body = f.read()
    state["releases"].append({"id": new_id(), "tag_name": tag, "draft": True,
                              "assets": [], "body": body})
elif args[:2] == ["release", "upload"]:
    r = release_by_tag(args[2])
    if "--clobber" not in args:
        sys.exit("no --clobber")
    files = [a for a in args[3:] if os.path.isfile(a)]
    fault = state["upload_faults"].pop(0) if state["upload_faults"] else "ok"
    if fault == "fail":
        save(); sys.exit("HTTP 502")
    if fault == "starter":
        r["assets"].append({"id": new_id(), "name": os.path.basename(files[0]),
                            "size": 0, "state": "starter"})
        save(); sys.exit("HTTP 502")
    for path in files:
        name = os.path.basename(path)
        if any(a["name"] == name for a in r["assets"]):
            save(); sys.exit(f"HTTP 422: ReleaseAsset.name already exists: {name}")
        r["assets"].append({"id": new_id(), "name": name,
                            "size": os.path.getsize(path), "state": "uploaded"})
    if fault == "stored_then_fail":
        save(); sys.exit("HTTP 422: ReleaseAsset.name already exists")
else:
    sys.exit("unexpected gh call")
save()
'''

SLEEP = '#!/usr/bin/env bash\necho "sleep $1" >>"$LOG"\n'

TAG = "denext-runtime-v3.0.0-denext.31"


class ReleaseUpload(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        bin_dir = os.path.join(self.tmp, "bin")
        os.makedirs(bin_dir)
        for name, body in (("gh", GH), ("sleep", SLEEP)):
            path = os.path.join(bin_dir, name)
            with open(path, "w") as f:
                f.write(body)
            os.chmod(path, 0o755)
        self.path = bin_dir + os.pathsep + os.environ.get("PATH", "")
        self.files = []
        for i, name in enumerate(["a.tar.gz", "b.zip", "SHA256SUMS", "manifest.json", "s.tar.gz"]):
            path = os.path.join(self.tmp, name)
            with open(path, "w") as f:
                f.write("x" * (i + 1))
            self.files.append(path)
        self.notes = os.path.join(self.tmp, "notes.md")
        with open(self.notes, "w") as f:
            f.write("the notes\n")
        self.state_path = os.path.join(self.tmp, "state.json")
        self.log = os.path.join(self.tmp, "log")
        self.write_state({"releases": [], "next_id": 100, "upload_faults": []})

    def write_state(self, state):
        with open(self.state_path, "w") as f:
            json.dump(state, f)

    def state(self):
        with open(self.state_path) as f:
            return json.load(f)

    def run_script(self, faults=(), latest="true", prerelease="", files=None):
        state = self.state()
        state["upload_faults"] = list(faults)
        self.write_state(state)
        open(self.log, "w").close()
        env = {
            "PATH": self.path,
            "GH_STATE": self.state_path,
            "LOG": self.log,
            "REPO": "Brainwires/deno",
            "TAG": TAG,
            "LATEST": latest,
            "PRERELEASE": prerelease,
            "UPLOAD_BATCH": "2",
            "UPLOAD_ATTEMPTS": "3",
            "UPLOAD_BACKOFF": "5",
        }
        result = subprocess.run(
            ["bash", SCRIPT, self.notes, *(files or self.files)],
            env=env, capture_output=True, text=True,
        )
        with open(self.log) as f:
            return result, f.read().splitlines()

    def assets(self, release):
        return sorted((a["name"], a["size"], a["state"]) for a in release["assets"])

    def expected(self):
        return sorted((os.path.basename(p), os.path.getsize(p), "uploaded") for p in self.files)

    def only_release(self):
        releases = self.state()["releases"]
        self.assertEqual(len(releases), 1, releases)
        return releases[0]

    def test_a_new_release_is_a_draft_until_every_asset_is_on_it(self):
        result, calls = self.run_script(prerelease="--prerelease", latest="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        release = self.only_release()
        self.assertFalse(release["draft"])
        self.assertTrue(release["prerelease"])
        self.assertEqual(release["make_latest"], "false")
        self.assertEqual(release["name"], TAG)
        self.assertEqual(release["body"], "the notes\n")
        self.assertEqual(self.assets(release), self.expected())
        # Three batches of at most two, then the publish last.
        uploads = [c for c in calls if c.startswith("gh release upload")]
        self.assertEqual(len(uploads), 3)
        self.assertTrue(calls[-1].startswith("gh api -X PATCH"), calls)
        self.assertEqual([c for c in calls if c.startswith("sleep")], [])

    def test_latest_and_not_a_prerelease(self):
        result, _ = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        release = self.only_release()
        self.assertFalse(release["prerelease"])
        self.assertEqual(release["make_latest"], "true")

    def test_an_upload_github_stored_before_failing_is_not_sent_again(self):
        result, calls = self.run_script(faults=["stored_then_fail"])
        self.assertEqual(result.returncode, 0, result.stderr)
        release = self.only_release()
        self.assertFalse(release["draft"])
        self.assertEqual(self.assets(release), self.expected())
        uploads = [c for c in calls if c.startswith("gh release upload")]
        # The first batch's retry found both files stored: no second upload.
        self.assertEqual(len(uploads), 3, calls)
        self.assertIn("sleep 5", calls)

    def test_a_failed_batch_is_retried_with_backoff(self):
        result, calls = self.run_script(faults=["fail", "fail"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.assets(self.only_release()), self.expected())
        self.assertEqual([c for c in calls if c.startswith("sleep")], ["sleep 5", "sleep 10"])

    def test_a_half_stored_asset_is_deleted_before_the_retry(self):
        result, calls = self.run_script(faults=["starter"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.assets(self.only_release()), self.expected())
        self.assertTrue(any(c.startswith("gh api -X DELETE") for c in calls), calls)

    def test_a_batch_that_never_uploads_leaves_the_draft(self):
        result, calls = self.run_script(faults=["fail"] * 3)
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not upload", result.stderr)
        release = self.only_release()
        self.assertTrue(release["draft"])
        self.assertFalse(any(c.startswith("gh api -X PATCH") for c in calls))

    def test_a_re_run_reuses_the_draft(self):
        self.run_script(faults=["fail"] * 3)
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("reusing the draft release", result.stdout)
        self.assertFalse(any(c.startswith("gh release create") for c in calls))
        release = self.only_release()
        self.assertFalse(release["draft"])
        self.assertEqual(self.assets(release), self.expected())

    def test_a_re_run_after_publishing_changes_nothing(self):
        self.run_script()
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already published with every asset", result.stdout)
        self.assertFalse(any("release upload" in c or "-X " in c for c in calls), calls)

    def test_a_published_release_with_other_assets_is_an_error(self):
        self.run_script()
        with open(self.files[0], "a") as f:
            f.write("changed")
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("already published but its assets differ", result.stderr)
        self.assertIn("wrong: a.tar.gz", result.stderr)
        self.assertFalse(any("release upload" in c or "-X " in c for c in calls), calls)

    def test_an_unexpected_asset_on_the_draft_stops_the_publish(self):
        self.write_state({"releases": [
            {"id": 1, "tag_name": TAG, "draft": True, "assets": [
                {"id": 2, "name": "stray.txt", "size": 1, "state": "uploaded"},
            ]},
        ], "next_id": 100, "upload_faults": []})
        result, calls = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("unexpected: stray.txt", result.stderr)
        self.assertTrue(self.only_release()["draft"])
        self.assertFalse(any(c.startswith("gh api -X PATCH") for c in calls))

    def test_two_releases_of_the_tag_is_an_error(self):
        self.write_state({"releases": [
            {"id": 1, "tag_name": TAG, "draft": True, "assets": []},
            {"id": 2, "tag_name": TAG, "draft": True, "assets": []},
        ], "next_id": 100, "upload_faults": []})
        result, _ = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("2 releases have the tag", result.stderr)

    def test_files_with_the_same_name_are_refused(self):
        os.makedirs(os.path.join(self.tmp, "other"))
        twin = os.path.join(self.tmp, "other", "b.zip")
        with open(twin, "w") as f:
            f.write("y")
        result, calls = self.run_script(files=self.files + [twin])
        self.assertEqual(result.returncode, 1)
        self.assertIn("two release files are named b.zip", result.stderr)
        self.assertEqual(calls, [])

    def test_a_bad_latest_value_is_refused(self):
        result, calls = self.run_script(latest="yes")
        self.assertEqual(result.returncode, 1)
        self.assertIn("LATEST must be true or false", result.stderr)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
