"""Tests of tools/release/export-public.sh and hygiene.py.
Run: python3 -B -m unittest discover -s tools/release

Each test builds a throwaway git repository holding the export scripts and the
guard, plants a file and exports it. Made-up values only; the planted strings are
assembled at run time so this file itself passes the scan.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
SCRIPTS = ["tools/release/export-public.sh", "tools/release/hygiene.py", "packaging/guard.py"]
FAKE_NEEDLE = "Ada Q. Example"


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode("utf-8"))


class ExportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.src = os.path.join(self.tmp.name, "src")
        self.out = os.path.join(self.tmp.name, "out")
        # No global or system git configuration; a fixed test identity.
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        MILIONAR_GUARD_FORBID=FAKE_NEEDLE)
        self.env.pop("MILIONAR_GUARD_ALLOW", None)
        for rel in SCRIPTS:
            dst = os.path.join(self.src, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(os.path.join(REPO, rel), dst)
        write(os.path.join(self.src, "apps/milionar/Cargo.toml"),
              '[package]\nname = "milionar"\nversion = "0.1.0"\n')
        write(os.path.join(self.src, "README.md"), "Plain ASCII - text.\n")
        # An internal plan, with the header marker hygiene.py looks for.
        write(os.path.join(self.src, "docs/internal/notes.md"),
              "# Plan\n\n> **For agentic" + " workers:** internal\n")
        # The maintainer's local list of internal paths (gitignored in the real repo).
        write(os.path.join(self.src, "tools/release/export-exclude.local"),
              "# internal\n/docs/internal/\n")
        write(os.path.join(self.src, "analysis/findings.md"), "notes\n")
        self.git("init", "-q", "-b", "master")
        self.git("config", "user.name", "Test Example")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.commit()

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args, cwd=None):
        return subprocess.run(["git", "-C", cwd or self.src, *args], env=self.env,
                              check=True, capture_output=True, text=True).stdout

    def commit(self):
        self.git("add", "-A", "-f")
        self.git("commit", "-q", "--allow-empty", "-m", "test")

    def plant(self, rel, data):
        write(os.path.join(self.src, rel), data)
        self.commit()

    def export(self, *args):
        return subprocess.run(
            [os.path.join(self.src, "tools/release/export-public.sh"), self.out, *args],
            env=self.env, capture_output=True, text=True)

    def assert_fails(self, needle_in_stderr):
        r = self.export()
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn(needle_in_stderr, r.stderr)
        self.assertFalse(os.path.exists(self.out), "a failed export must leave nothing")

    def test_clean_export(self):
        r = self.export()
        self.assertEqual(r.returncode, 0, r.stderr)
        head = self.git("rev-parse", "HEAD").strip()
        with open(os.path.join(self.out, "ENGINE_REVISION")) as f:
            self.assertEqual(f.read(), head + "\n")
        self.assertTrue(os.path.isfile(os.path.join(self.out, "README.md")))
        self.assertFalse(os.path.exists(os.path.join(self.out, "docs/internal")))
        self.assertFalse(os.path.exists(os.path.join(self.out, "analysis")))
        self.assertFalse(os.path.exists(os.path.join(self.out, ".git")))

    def test_with_analysis(self):
        r = self.export("--with-analysis")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertTrue(os.path.isfile(os.path.join(self.out, "analysis/findings.md")))
        self.assertFalse(os.path.exists(os.path.join(self.out, "docs/internal")))

    def test_uncommitted_files_are_not_exported(self):
        write(os.path.join(self.src, "loose.smk"), b"x")
        r = self.export()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.out, "loose.smk")))

    def test_init_git_uses_the_pseudonym(self):
        r = self.export("--init-git")
        self.assertEqual(r.returncode, 0, r.stderr)
        log = self.git("log", "--format=%an|%ae|%cn|%ce|%G?", cwd=self.out).splitlines()
        self.assertEqual(log, ["007hacky007|007hacky007@users.noreply.github.com|"
                               "007hacky007|007hacky007@users.noreply.github.com|N"])
        self.assertEqual(self.git("branch", "--show-current", cwd=self.out).strip(), "master")
        self.assertEqual(self.git("config", "--local", "commit.gpgsign", cwd=self.out).strip(),
                         "false")
        self.assertEqual(self.git("status", "--porcelain", cwd=self.out), "")

    def test_refuses_non_empty_out_dir(self):
        write(os.path.join(self.out, "keep.txt"), "x")
        r = self.export()
        self.assertNotEqual(r.returncode, 0)
        self.assertTrue(os.path.isfile(os.path.join(self.out, "keep.txt")))

    def test_missing_local_exclude_list_fails(self):
        os.remove(os.path.join(self.src, "tools/release/export-exclude.local"))
        r = self.export()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("export-exclude.local is missing", r.stderr)
        self.assertFalse(os.path.exists(self.out))

    def test_empty_local_exclude_list_fails(self):
        write(os.path.join(self.src, "tools/release/export-exclude.local"), "# nothing\n\n")
        r = self.export()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("lists no path", r.stderr)
        self.assertFalse(os.path.exists(self.out))

    def test_internal_doc_caught_when_not_excluded(self):
        # The list exists but misses the internal folder: the marker scan stops it.
        write(os.path.join(self.src, "tools/release/export-exclude.local"), "docs/other\n")
        self.assert_fails("internal planning document")

    def test_unignored_untracked_listed_path_warns(self):
        write(os.path.join(self.src, "tools/release/export-exclude.local"),
              "docs/internal\nscratch-notes\n")
        write(os.path.join(self.src, "scratch-notes/a.md"), "x\n")  # untracked, not ignored
        r = self.export()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("warning: scratch-notes exists and is not ignored", r.stderr)

    def test_linux_and_windows_home_paths_fail(self):
        for text in ("cd /ho" + "me/bob/src\n", "C:\\Us" + "ers\\Bob\\x\n",
                     "\"c:\\\\us" + "ers\\\\bob\"\n"):
            with self.subTest(text=text):
                self.plant("docs/p.md", text)
                self.assert_fails("home path")

    def test_allow_list_does_not_silence_forbidden_strings(self):
        self.env["MILIONAR_GUARD_ALLOW"] = FAKE_NEEDLE + ",ada@example.invalid"
        self.env["MILIONAR_GUARD_FORBID"] = FAKE_NEEDLE + ",ada@example.invalid"
        self.plant("docs/credits.md", "Mail ada@example.invalid\n")
        self.assert_fails("forbidden string ada@example.invalid")

    def test_home_path_fails(self):
        self.plant("src/a.rs", "let p = \"/Us" + "ers/bob/Library/x\";\n")
        self.assert_fails("home path")

    def test_placeholder_home_path_passes(self):
        self.plant("src/a.rs", "let p = \"/Us" + "ers/someone/Library/x\";\n")
        r = self.export()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_unicode_dash_fails(self):
        self.plant("docs/a.md", "one \u2014 two\n")
        self.assert_fails("U+2014")

    def test_game_data_fails(self):
        self.plant("assets/intro.smk", b"SMK4")
        self.assert_fails("forbidden extension .smk")

    def test_original_exe_name_fails(self):
        self.plant("bin/MILIONAR.EXE", b"MZ")
        self.assert_fails("forbidden file name")

    def test_needle_from_env_fails_in_any_case(self):
        self.plant("docs/credits.md", "Thanks to " + FAKE_NEEDLE.upper() + ".\n")
        self.assert_fails("forbidden string " + FAKE_NEEDLE)

    def test_hygiene_scans_this_repository_head(self):
        # The real tree (with the real needles) is checked by running the export;
        # here only the scanner's own files must be clean.
        sys.path.insert(0, HERE)
        import hygiene
        for rel in ("tools/release/hygiene.py", "tools/release/test_export.py",
                    "tools/release/export-public.sh"):
            with open(os.path.join(REPO, rel), "rb") as f:
                self.assertEqual(hygiene.scan_bytes(rel, f.read(), []), [])


if __name__ == "__main__":
    unittest.main()
