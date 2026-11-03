#!/usr/bin/env python3
"""Pins the shape of CI changes whose failure is silent (everything stays green).

* `cache-cleanup.yml` runs on `pull_request_target`, which is safe only while it never checks out
  or runs pull request code and holds nothing but `actions: write`;
* `cache-prune.yml` stays manual and dry by default;
* ci.yml's `paths` filter (`CiPathFilter`): evaluated with GitHub's rules, a docs-only or
  repository-metadata-only change (issue forms, a PR template, prose, images) starts no run, while
  every file a gate reads (PRIVACY.md, the credits pair, an `include_str!` target, the workflows)
  still does, and no other workflow carries a catch-all `'**'` filter.

This fork retired the upstream release/site/simulator workflows (release, rc, nightly, build-package,
site-video, pages, build-bench, ci-metrics, simulators); releases are built by `build-ipk.yml`.

Text-level on purpose, like ci/test_ci_timeouts.py (no YAML library on a stock runner).
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github/workflows"
sys.path.insert(0, str(ROOT / "ci"))
import ci_paths  # noqa: E402


def text(name):
    return (WORKFLOWS / name).read_text()


def code(name):
    """The workflow without comment lines, so a comment cannot satisfy or trip an assertion."""
    return "\n".join(l for l in text(name).splitlines() if not l.lstrip().startswith("#"))


def job_body(name, job):
    lines = code(name).splitlines()
    start = next(i for i, l in enumerate(lines) if l == f"  {job}:")
    end = next((i for i in range(start + 1, len(lines)) if re.match(r"^  [A-Za-z0-9_-]+:\s*$", lines[i])), len(lines))
    return "\n".join(lines[start:end])


class CacheCleanup(unittest.TestCase):
    def test_privileged_trigger_never_runs_pull_request_code(self):
        body = code("cache-cleanup.yml")
        self.assertRegex(body, r"(?m)^  pull_request_target:\n    types: \[closed\]")
        self.assertNotIn("checkout", body)
        self.assertNotIn("actions/", body.replace("actions: write", ""))
        # Only the number is read from the event, and through the environment, not the script.
        self.assertEqual(sorted(set(re.findall(r"github\.event\.[A-Za-z_.]+", body))),
                         ["github.event.pull_request.number"])
        self.assertNotRegex(body, r"run:[^\n]*\$\{\{")
        self.assertNotIn("\n      - uses:", body)

    def test_token_holds_actions_write_and_nothing_else(self):
        body = code("cache-cleanup.yml")
        block = re.search(r"(?m)^permissions:\n((?:  .*\n)+)", body + "\n").group(1)
        self.assertEqual(block.split(), ["actions:", "write"])
        self.assertNotIn("permissions:", job_body("cache-cleanup.yml", "delete-pull-request-caches"))

    def test_prune_is_manual_and_dry_by_default(self):
        body = code("cache-prune.yml")
        self.assertRegex(body, r"(?m)^on:\n  workflow_dispatch:")
        self.assertNotRegex(body, r"(?m)^  (push|pull_request|pull_request_target|schedule):")
        self.assertRegex(body, r"dry_run:[\s\S]*?default: true")
        self.assertIn("--delete", body)


class CiPathFilter(unittest.TestCase):
    """ci.yml's `paths` list (the `&ci_paths` anchor, shared by `push` and `pull_request`), run through
    GitHub's matching rules, because a wrong line here fails silently in BOTH directions: a gate-read
    file excluded means a run that should have caught a break never starts, and a prose file left
    in means ~20 runner-minutes per documentation edit (#261, #513).

    The rules (docs.github.com, "Workflow syntax", filter pattern cheat sheet): patterns are
    evaluated in order; a `!` pattern removes what an earlier pattern matched, a later positive
    pattern puts it back; `*` matches any characters but `/`; `**` matches any characters including
    `/`, and `**/` also matches no directory at all; `?` matches one character but `/`."""

    @staticmethod
    def anchor():
        return ci_paths.patterns(text("ci.yml"))

    @staticmethod
    def triggers(path, patterns=None):
        return ci_paths.triggers(path, patterns if patterns is not None else CiPathFilter.anchor())

    def test_only_push_filters_by_path_and_pull_request_starts_on_every_pr(self):
        # A PR the filter skips starts no run, so the required checks never report. The `changes` job
        # applies the same list to the PR's files instead.
        self.assertRegex(code("ci.yml"), r"(?m)^  pull_request:\n  workflow_dispatch:")
        self.assertEqual(len(re.findall(r"(?m)^\s+paths:", code("ci.yml"))), 1)
        self.assertEqual(len(re.findall(r"(?m)^\s+paths: &ci_paths$", text("ci.yml"))), 1)
        self.assertNotIn("*ci_paths", code("ci.yml"))
        self.assertIn("paths: &ci_paths", code("ci.yml").split("  pull_request:")[0])

    def test_the_changes_jobs_evaluator_is_this_evaluator(self):
        # ci/ci_paths.py is what the job runs, on the list it reads from ci.yml: same code, same cases.
        def run(files):
            return subprocess.run(
                [sys.executable, str(ROOT / "ci/ci_paths.py")], input="\n".join(files), text=True,
                capture_output=True, check=True).stdout.strip()
        self.assertEqual(run(list(self.MUST_NOT_TRIGGER)), "false")
        self.assertEqual(run(["README.md", "docs/x.md", "Makefile"]), "true")
        for path in self.MUST_TRIGGER:
            self.assertEqual(run([path]), "true", path)
        for path in self.MUST_NOT_TRIGGER:
            self.assertEqual(run([path]), "false", path)
        self.assertEqual(run(["tools/requirements-docs.txt"]), "false")
        self.assertEqual(run([]), "true", "no files learned: fail closed")

    def test_the_matcher_follows_githubs_rules(self):
        t = self.triggers
        self.assertTrue(t("a/b/c.rs", ["**"]))
        self.assertTrue(t("x", ["**"]))
        self.assertFalse(t("a/b.md", ["**", "!a/**"]))
        self.assertTrue(t("a/b.md", ["**", "!a/**", "a/b.md"]), "a later positive pattern re-includes")
        self.assertFalse(t("a/b.md", ["**", "a/b.md", "!a/**"]), "a later negative pattern excludes again")
        self.assertTrue(t("a/b/c.md", ["**", "!a/*.md"]), "`*` does not cross `/`")
        self.assertFalse(t("a/b/c.md", ["**", "!a/**/*.md"]))
        self.assertFalse(t("a/c.md", ["**", "!a/**/*.md"]), "`**/` also matches no directory")
        self.assertFalse(t("README.md", ["**", "!README.md"]))
        self.assertTrue(t("docs/README.md", ["**", "!README.md"]), "a bare name is anchored at the root")

    # Every path here is read by a gate or ships, so skipping the run for it would hide a break.
    MUST_TRIGGER = (
        "rust-modules/ui/src/widgets.rs", "rust-modules/Cargo.lock", "rust-modules/build.rs", "src/main.c",
        "Makefile", ".gitignore", "ci/check-package.py", "ci/check-python-steps.txt",
        "tests/test_harness.py", "tests/README.md", "tools/check-parallel.py", "locales/en/settings.json",
        "pkg/appinfo.json", "assets/icons/play.svg",
        ".github/workflows/ci.yml", ".github/workflows/build-ipk.yml",
        ".github/actions/host-rust/action.yml",
        ".claude/hooks/outbound-guard.py", ".claude/hooks/outbound-guard-test.py",
        ".agents/skills/wake-tv/wake-tv.sh",
        "PRIVACY.md", "LICENSE", "LICENSING.md", "TRADEMARKS.md", "THIRD-PARTY-NOTICES.md",
        "docs/screenshots/CREDITS.md", "site/credits.html", "site/index.html", "site/ci/index.html",
        "docs/measurements/p1b-logs/pipe_abr_pin_320.log",
    )
    # Prose, images and repository metadata: no gate reads them (the evidence is in ci.yml).
    MUST_NOT_TRIGGER = (
        ".github/ISSUE_TEMPLATE/bug_report.yml", ".github/ISSUE_TEMPLATE/config.yml",
        ".github/FUNDING.yml", ".github/PULL_REQUEST_TEMPLATE.md", ".github/pull_request_template.md",
        ".github/CODEOWNERS", ".github/dependabot.yml",
        "README.md", "CONTRIBUTING.md", "SECURITY.md", "AGENTS.md", "CLAUDE.md",
        "docs/agent-reference.md", "docs/troubleshooting.md", "docs/release-notes/v0.8.0.md",
        "docs/plex-openapi.json", "docs/screenshots/home.jpg", "docs/assets/sentry-wordmark-dark.svg",
        "docs/webosbrew-package.yml", "docs/measurements/m2-verbose-rerun.txt",
        "site/404.html", "site/media/closeup-glass.jpg", "tools/render-doc-page.py",
        "rust-modules/media/src/player/CLAUDE.md", ".agents/skills/ui-sim/SKILL.md",
        ".claude/settings.json", ".claude/workflows/swarm-gate.js", ".claude/agents/doc-claim-auditor.md",
        ".codex/agents/doc-claim-auditor.toml",
    )

    def test_what_a_gate_reads_triggers(self):
        patterns = self.anchor()
        for path in self.MUST_TRIGGER:
            with self.subTest(path):
                self.assertTrue(self.triggers(path, patterns), f"{path} is read by a gate but starts no run")
                self.assertTrue((ROOT / path).exists(), f"{path} is gone: update MUST_TRIGGER")

    def test_prose_images_and_repository_metadata_do_not(self):
        patterns = self.anchor()
        for path in self.MUST_NOT_TRIGGER:
            with self.subTest(path):
                self.assertFalse(self.triggers(path, patterns), f"{path} starts the whole CI for nothing")

    def test_every_compile_time_include_triggers(self):
        # `include_str!`/`include_bytes!` pull a file into the build, so a change to it changes what the
        # crate tests (PRIVACY.md, LICENSE, the locale and fixture files...) compile against.
        patterns, seen = self.anchor(), 0
        include = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"')
        for folder, dirs, files in os.walk(ROOT / "rust-modules"):
            dirs[:] = [d for d in dirs if not d.startswith("target")]
            for name in files:
                if not name.endswith(".rs"):
                    continue
                source = Path(folder, name)
                for target in include.findall(source.read_text(errors="replace")):
                    resolved = (source.parent / target).resolve()
                    try:
                        relative = resolved.relative_to(ROOT).as_posix()
                    except ValueError:
                        continue
                    seen += 1
                    with self.subTest(f"{source.relative_to(ROOT).as_posix()} -> {relative}"):
                        self.assertTrue(self.triggers(relative, patterns), f"{relative} is include_str!ed")
        self.assertGreater(seen, 50)

    def test_only_ci_has_a_catch_all_filter(self):
        # Every other workflow lists the files it builds from, so a documentation push cannot start it.
        for wf in sorted(WORKFLOWS.glob("*.yml")):
            catch_all = re.findall(r"(?m)^\s+- ['\"]?\*\*['\"]?\s*$", code(wf.name))
            self.assertEqual(len(catch_all), 1 if wf.name == "ci.yml" else 0, wf.name)



if __name__ == "__main__":
    unittest.main()
