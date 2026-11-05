#!/usr/bin/env python3
"""tools/ai-docs-refresh.sh against a throwaway repository: the readings, and the loop it caused.

Every case here is built from one small history in a temp directory — a trunk, a working branch, a
fork-added file, a contested file and a reference branch — so the expected counts are fixed numbers
rather than whatever the real repository happens to hold. No network, no cargo, no device.

The regressions this pins:

* `update` writes the COMMITTED reading. It used to write "commit count + 1", which is why running
  it by hand on a clean tree produced docs that `check` then rejected, and why every commit needed
  a matching docs commit (10 of 40 consecutive commits were nothing else).
* `update --pending` is the hook's reading and differs by file counts only — a file the commit adds.
* Touching a fork-ADDED file must not count it as "edited" (17822681 wrote 226 against a real 225).
* `check` names the line that disagrees instead of only saying "run update".
* No count that increments on every commit is stored at all.
"""
from __future__ import annotations

import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools" / "ai-docs-refresh.sh"

UPSTREAM_TEMPLATE = """# Upstream

<!-- BEGIN docs-fresh:base-stats -->
<!-- END docs-fresh:base-stats -->

<!-- BEGIN docs-fresh:cost-stats -->
<!-- END docs-fresh:cost-stats -->

The <!-- docs-fresh:edited -->0<!-- /docs-fresh:edited --> edited files are the seam.
"""


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=check
    )


def write(repo: Path, rel: str, text: str) -> None:
    path = repo / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def build_repo(root: Path) -> Path:
    """One trunk, one working branch, one reference branch. Returns the repo path."""
    repo = root / "repo"
    repo.mkdir()
    git(repo, "init", "-q", "-b", "nuvio-native")
    git(repo, "config", "user.email", "test@example.invalid")
    git(repo, "config", "user.name", "docs-refresh test")
    git(repo, "config", "commit.gpgsign", "false")

    (repo / "tools").mkdir()
    shutil.copy2(SCRIPT, repo / "tools" / "ai-docs-refresh.sh")
    write(repo, "AGENTS.md", "# Rules\n\nNo generated numbers in this file.\n")
    write(repo, "docs/nuvio/UPSTREAM.md", UPSTREAM_TEMPLATE)
    write(repo, "rust-modules/stremio/src/existing.rs", "// ours, but not new\n")
    write(repo, "src/main.c", "/* upstream */\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "base")
    base = git(repo, "rev-parse", "HEAD").stdout.strip()

    # The working line: one fork-ADDED file, then an edit to a file both sides touch.
    git(repo, "checkout", "-q", "-b", "forward-port/v0.9-hq", base)
    write(repo, "rust-modules/stremio/src/sbengine.rs", "// added by the fork\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "add a fork-owned module")
    write(repo, "rust-modules/stremio/src/existing.rs", "// ours, but not new\n// and edited\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "edit a shared file")

    # The reference branch, on the same base, contesting existing.rs and src/main.c.
    git(repo, "checkout", "-q", "-b", "upstream-line", base)
    write(repo, "src/main.c", "/* upstream */\n/* changed upstream */\n")
    write(repo, "rust-modules/stremio/src/existing.rs", "// ours, but not new\n// upstream too\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "upstream work")
    git(repo, "update-ref", "refs/remotes/upstream/feature/v0.9-hq", "upstream-line")

    git(repo, "checkout", "-q", "forward-port/v0.9-hq")
    return repo


def run(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", "tools/ai-docs-refresh.sh", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )


ROWS = {
    # table row label -> the count the generated table puts there
    "files_ours": r"^\| files this fork changed \| (\d+) \|$",
    "contested": r"^\| \*\*contested\*\* — changed on \*\*both\*\* sides \| (\d+) \|$",
    "added": r"^\| upstream-owned source \*\*added\*\* \(the fork's own crates\) \| (\d+) \|$",
}


def number(repo: Path, name: str) -> str:
    """An inline marker's value, or the count in the row that carries it."""
    text = (repo / "docs/nuvio/UPSTREAM.md").read_text(encoding="utf-8")
    m = re.search(rf"<!-- docs-fresh:{re.escape(name)} -->(.*?)<!-- /docs-fresh:", text, re.S)
    if m:
        return m.group(1)
    if name in ROWS:
        m = re.search(ROWS[name], text, re.M)
        if m:
            return m.group(1)
    raise AssertionError(f"no {name!r} in the generated docs:\n{text}")


class DocsRefresh(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = build_repo(Path(self.tmp.name))

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_update_writes_the_committed_reading_and_does_not_drift(self) -> None:
        first = run(self.repo, "update")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn("updated", first.stdout)
        self.assertEqual(run(self.repo, "check").returncode, 0)

        # Running it again must change nothing: the old pending reading added one commit per run.
        second = run(self.repo, "update")
        self.assertIn("nothing to update", second.stdout)
        self.assertEqual(run(self.repo, "check").returncode, 0)

    def test_the_counts_are_the_shape_of_the_fork(self) -> None:
        run(self.repo, "update")
        self.assertEqual(number(self.repo, "files_ours"), "2")  # sbengine.rs + existing.rs
        self.assertEqual(number(self.repo, "contested"), "1")  # existing.rs
        self.assertEqual(number(self.repo, "edited"), "1")  # existing.rs is M against the base
        self.assertEqual(number(self.repo, "added"), "1")  # sbengine.rs is A

    def test_no_counter_that_moves_on_every_commit_is_stored(self) -> None:
        run(self.repo, "update")
        text = (self.repo / "docs/nuvio/UPSTREAM.md").read_text(encoding="utf-8")
        self.assertNotIn("commits on our side", text)
        self.assertNotIn("docs-fresh:ahead", (self.repo / "AGENTS.md").read_text(encoding="utf-8"))

    def test_pending_counts_only_the_file_a_commit_adds(self) -> None:
        run(self.repo, "update")
        before = number(self.repo, "files_ours")

        write(self.repo, "rust-modules/stremio/src/brand_new.rs", "// new\n")
        git(self.repo, "add", "rust-modules/stremio/src/brand_new.rs")
        self.assertEqual(run(self.repo, "update", "--pending").returncode, 0)
        self.assertEqual(number(self.repo, "files_ours"), str(int(before) + 1))
        self.assertEqual(number(self.repo, "added"), "2")

        # The committed reading does not see the staged file, so `check` still passes at HEAD.
        self.assertEqual(run(self.repo, "update").returncode, 0)
        self.assertEqual(number(self.repo, "files_ours"), before)
        self.assertEqual(run(self.repo, "check").returncode, 0)

    def test_touching_a_fork_added_file_does_not_inflate_edited(self) -> None:
        run(self.repo, "update")
        self.assertEqual(number(self.repo, "edited"), "1")

        write(
            self.repo,
            "rust-modules/stremio/src/sbengine.rs",
            "// added by the fork\n// and edited by a later commit\n",
        )
        git(self.repo, "add", "rust-modules/stremio/src/sbengine.rs")
        self.assertEqual(run(self.repo, "update", "--pending").returncode, 0)
        # `git diff --cached` alone calls that file M (index against HEAD); against the base it is A.
        self.assertEqual(number(self.repo, "edited"), "1")
        self.assertEqual(number(self.repo, "added"), "1")

    def test_check_names_the_line_that_disagrees(self) -> None:
        run(self.repo, "update")
        path = self.repo / "docs/nuvio/UPSTREAM.md"
        text = path.read_text(encoding="utf-8")
        path.write_text(
            text.replace(
                "<!-- docs-fresh:edited -->1<!-- /docs-fresh:edited -->",
                "<!-- docs-fresh:edited -->99<!-- /docs-fresh:edited -->",
            ),
            encoding="utf-8",
        )
        result = run(self.repo, "check")
        self.assertEqual(result.returncode, 1)
        self.assertIn("stale derived numbers", result.stdout)
        self.assertIn("99", result.stdout)  # what the file says
        self.assertIn("1", result.stdout)   # what git says
        self.assertIn("docs/nuvio/UPSTREAM.md", result.stdout)


if __name__ == "__main__":
    unittest.main()
