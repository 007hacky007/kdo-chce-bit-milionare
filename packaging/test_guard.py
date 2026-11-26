"""Tests of packaging/guard.py. Run: python3 -m unittest discover -s packaging"""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import guard  # noqa: E402

MANIFEST = """
# comment
Contents/Info.plist            @plist
Contents/MacOS/app             @binary exec
Contents/_CodeSignature/CodeResources?  @codesign
Contents/Resources/LICENSE     LICENSE
"""


def _in_git_checkout():
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    r = subprocess.run(["git", "-C", repo, "rev-parse", "--show-toplevel"],
                       capture_output=True, text=True)
    return r.returncode == 0 and os.path.samefile(r.stdout.strip(), repo)


# A plain public export (tools/release/export-public.sh without --init-git) is no
# git checkout; the .gitignore checks then have nothing to ask.
IN_GIT = _in_git_checkout()


def write(path, data=b"x"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


class TreeCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.app = os.path.join(self.tmp.name, "T.app")
        self.entries = guard.parse_manifest(MANIFEST)
        for rel in ("Contents/Info.plist", "Contents/MacOS/app", "Contents/Resources/LICENSE"):
            write(os.path.join(self.app, rel))

    def tearDown(self):
        self.tmp.cleanup()

    def check(self, **kw):
        kw.setdefault("unsigned", True)
        return guard.check_tree(self.app, self.entries, **kw)


class ManifestTests(unittest.TestCase):
    def test_parses_entries(self):
        e = guard.parse_manifest(MANIFEST)
        self.assertEqual([x.dest for x in e], [
            "Contents/Info.plist", "Contents/MacOS/app",
            "Contents/_CodeSignature/CodeResources", "Contents/Resources/LICENSE"])
        self.assertTrue(e[1].exe and e[1].generated)
        self.assertTrue(e[2].optional)
        self.assertFalse(e[3].generated)

    def test_rejects_forbidden_entries(self):
        for line in ("a/b.smk x", "a/x.DAT x", "a/GAME.ICO x", "a/Milionar.exe x",
                     "a/ok y/sound.wav", "a/smackw32.dll x", "a/x.iso x", "a/s.sav x", "a/p.upk x"):
            with self.subTest(line=line), self.assertRaises(guard.GuardError):
                guard.parse_manifest(line)

    def test_rejects_bad_paths_and_duplicates(self):
        for text in ("/abs x", "a/../b x", "a x\na y", "a", "a b c", "a ../b"):
            with self.subTest(text=text), self.assertRaises(guard.GuardError):
                guard.parse_manifest(text)

    def test_shipped_manifests_parse(self):
        here = os.path.dirname(os.path.abspath(__file__))
        repo = os.path.dirname(here)
        pins = guard.load_pins(repo)
        for plat in ("macos", "windows"):
            with self.subTest(plat=plat):
                entries = guard.load_manifest(os.path.join(here, plat, "stage.txt"))
                for e in entries:
                    guard.pin_for(e, pins)  # every tools/upscalers source is pinned
                    if not e.generated:
                        self.assertTrue(os.path.isfile(os.path.join(repo, e.source))
                                        or e.source.startswith(guard.PINNED_PREFIX), e.source)

    def test_both_manifests_ship_sdl_and_crate_licences(self):
        here = os.path.dirname(os.path.abspath(__file__))
        for plat, prefix in (("macos", "Contents/Resources/licenses/"), ("windows", "licenses/")):
            with self.subTest(plat=plat):
                got = {e.dest: e.source
                       for e in guard.load_manifest(os.path.join(here, plat, "stage.txt"))}
                for name in ("SDL3-zlib.txt", "rust-crates.html"):
                    self.assertEqual(got.get(prefix + name), "packaging/licenses/" + name)

    def test_windows_manifest_ships_only_the_listed_runner_files(self):
        here = os.path.dirname(os.path.abspath(__file__))
        dests = {e.dest for e in guard.load_manifest(os.path.join(here, "windows", "stage.txt"))}
        self.assertIn("milionar.exe", dests)
        self.assertIn("upscaler/vcomp140.dll", dests)
        self.assertEqual(sorted(d for d in dests if d.startswith("upscaler/")), [
            "upscaler/models/realesr-animevideov3-x3.bin",
            "upscaler/models/realesr-animevideov3-x3.param",
            "upscaler/realesrgan-ncnn-vulkan.exe", "upscaler/vcomp140.dll"])

    def test_own_exe_only_as_built_binary_at_the_root(self):
        guard.parse_manifest("milionar.exe @binary exec\n")
        for text in ("milionar.exe LICENSE", "Milionar.exe @binary", "bin/milionar.exe @binary",
                     "MILIONAR.EXE @binary", "x tools/Milionar.exe"):
            with self.subTest(text=text), self.assertRaises(guard.GuardError):
                guard.parse_manifest(text)


class CheckTests(TreeCase):
    def test_clean_tree_passes(self):
        self.assertEqual(self.check(), [])

    def test_signature_required_when_signed(self):
        self.assertEqual(self.check(unsigned=False),
                         ["Contents/_CodeSignature/CodeResources: missing"])
        write(os.path.join(self.app, "Contents/_CodeSignature/CodeResources"))
        self.assertEqual(self.check(unsigned=False), [])

    def test_forbidden_names(self):
        for rel in ("Contents/Resources/intro.SMK", "Contents/Resources/x.dat",
                    "Contents/Resources/a.wav", "Contents/Resources/Milionar.exe",
                    "Contents/Resources/SMACKW32.DLL", "Contents/Resources/game.ico",
                    "Contents/Resources/d.iso", "Contents/Resources/s.sav",
                    "Contents/Resources/p.upk"):
            with self.subTest(rel=rel):
                p = os.path.join(self.app, rel)
                write(p)
                errs = self.check()
                os.remove(p)
                self.assertTrue(any(rel in e and "forbidden" in e for e in errs), errs)

    def test_symlink(self):
        os.symlink("LICENSE", os.path.join(self.app, "Contents/Resources/link"))
        errs = self.check()
        self.assertIn("Contents/Resources/link: symlink", errs)

    def test_symlink_replacing_listed_file(self):
        p = os.path.join(self.app, "Contents/Resources/LICENSE")
        os.remove(p)
        os.symlink("/etc/hosts", p)
        errs = self.check()
        self.assertIn("Contents/Resources/LICENSE: symlink", errs)
        self.assertIn("Contents/Resources/LICENSE: missing", errs)

    def test_symlinked_dir(self):
        os.symlink(self.tmp.name, os.path.join(self.app, "Contents/Resources/up"))
        self.assertIn("Contents/Resources/up: symlink", self.check())

    def test_extra_file_and_dir(self):
        write(os.path.join(self.app, "Contents/Resources/extra.txt"))
        os.makedirs(os.path.join(self.app, "Contents/Empty"))
        errs = self.check()
        self.assertIn("Contents/Resources/extra.txt: not in the stage manifest", errs)
        self.assertIn("Contents/Empty: directory not in the stage manifest", errs)

    def test_missing_file(self):
        os.remove(os.path.join(self.app, "Contents/MacOS/app"))
        self.assertEqual(self.check(), ["Contents/MacOS/app: missing"])

    def test_build_path_in_file(self):
        write(os.path.join(self.app, "Contents/MacOS/app"), b"\0" * (3 << 20) + b"/home/ada-example/x")
        errs = self.check(needles=[b"/home/ada-example"])
        self.assertEqual(errs, ["Contents/MacOS/app: contains the forbidden string /home/ada-example"])

    def test_build_path_across_chunk_boundary(self):
        data = b"\0" * (guard.CHUNK - 4) + b"/home/ada-example"
        write(os.path.join(self.app, "Contents/MacOS/app"), data)
        self.assertEqual(len(self.check(needles=[b"/home/ada-example"])), 1)


    def test_hardlink(self):
        os.link(os.path.join(self.app, "Contents/Resources/LICENSE"),
                os.path.join(self.tmp.name, "outside"))
        self.assertEqual(self.check(), ["Contents/Resources/LICENSE: hardlink (2 links)"])

    @unittest.skipUnless(sys.platform == "darwin", "uses the macOS xattr tool")
    def test_extended_attributes(self):
        lic = os.path.join(self.app, "Contents/Resources/LICENSE")
        self.assertEqual(self.check(), [])
        subprocess.run(["xattr", "-w", "com.apple.metadata:kMDItemWhereFroms", "x", lic], check=True)
        subprocess.run(["xattr", "-w", "com.apple.quarantine", "q", os.path.join(self.app, "Contents")],
                       check=True)
        self.assertEqual(self.check(), [
            "Contents: extended attribute com.apple.quarantine",
            "Contents/Resources/LICENSE: extended attribute com.apple.metadata:kMDItemWhereFroms"])
        subprocess.run(["xattr", "-cr", self.app], check=True)
        self.assertEqual(self.check(), [])


def pe(machine):
    """A minimal PE header with the given COFF machine."""
    head = bytearray(64)
    head[:2] = b"MZ"
    head[0x3C:0x40] = (64).to_bytes(4, "little")
    return bytes(head) + b"PE\0\0" + machine.to_bytes(2, "little") + b"\0" * 32


class OwnExeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.tree = os.path.join(self.tmp.name, "tree")
        self.entries = guard.parse_manifest("milionar.exe @binary exec\nLICENSE LICENSE\n")
        write(os.path.join(self.tree, "LICENSE"))

    def tearDown(self):
        self.tmp.cleanup()

    def test_x86_64_build_passes(self):
        write(os.path.join(self.tree, "milionar.exe"), pe(guard.PE_MACHINE_AMD64))
        self.assertEqual(guard.check_tree(self.tree, self.entries), [])

    def test_original_i386_exe_is_rejected(self):
        write(os.path.join(self.tree, "milionar.exe"), pe(0x14C))
        self.assertIn("milionar.exe: forbidden file name", guard.check_tree(self.tree, self.entries))

    def test_not_a_pe_is_rejected(self):
        write(os.path.join(self.tree, "milionar.exe"), b"MZ not really")
        self.assertIn("milionar.exe: forbidden file name", guard.check_tree(self.tree, self.entries))

    def test_capitalised_name_is_rejected(self):
        # The original's spelling, even with an x86-64 PE inside: only the exact
        # lower-case name at the root is our build.
        write(os.path.join(self.tree, "Milionar.exe"), pe(guard.PE_MACHINE_AMD64))
        errs = guard.check_tree(self.tree, self.entries)
        self.assertIn("Milionar.exe: forbidden file name", errs)
        self.assertIn("milionar.exe: missing", errs)

    def test_own_name_elsewhere_is_rejected(self):
        write(os.path.join(self.tree, "milionar.exe"), pe(guard.PE_MACHINE_AMD64))
        write(os.path.join(self.tree, "upscaler/milionar.exe"), pe(guard.PE_MACHINE_AMD64))
        self.assertIn("upscaler/milionar.exe: forbidden file name",
                      guard.check_tree(self.tree, self.entries))

    def test_own_exe_not_from_the_build_is_rejected(self):
        entries = [guard.Entry("milionar.exe", "LICENSE")]
        write(os.path.join(self.tree, "milionar.exe"), pe(guard.PE_MACHINE_AMD64))
        self.assertIn("milionar.exe: forbidden file name", guard.check_tree(self.tree, entries))


class NeedleTests(unittest.TestCase):
    # Made-up values only: no real names, addresses or user names in tests.
    def test_needles_from_skips_short_kept_and_empty(self):
        got = guard.needles_from(
            ["/home/someone/", "abc", "", None, "Ada Example", "ada@example.invalid",
             "pseudonym", "/"], keep=["pseudonym", None])
        self.assertEqual(got, [b"/home/someone", b"Ada Example", b"ada@example.invalid"])

    def test_extra_needles_from_env_and_local_file(self):
        with tempfile.TemporaryDirectory() as repo:
            write(os.path.join(repo, guard.FORBID_FILE),
                  b"# comment\nSecret Codename  # trailing\n\n/Volumes/Scratch\n")
            env = {guard.FORBID_ENV: " first one , ,second"}
            self.assertEqual(guard.extra_needles(env, repo),
                             ["first one", "second", "Secret Codename", "/Volumes/Scratch"])
            self.assertEqual(guard.extra_needles({}, None), [])

    def test_allowed_words_are_plain_words_only(self):
        env = {guard.ALLOW_ENV: " runner , /home/ada-example,C:\\Users\\ada-example,, ci-bot,"
                                "ada@example.invalid,Ada Example,a\tb"}
        self.assertEqual(guard.allowed_words(env), ["runner", "ci-bot"])
        self.assertEqual(guard.allowed_words({}), [])

    def probe_needles(self, env_extra, names):
        """default_needles() in a child with USER, HOME and env_extra set:
        whether each of `names` is a needle."""
        here = os.path.dirname(os.path.abspath(__file__))
        probe = ("import os, sys; sys.path.insert(0, sys.argv[1]); import guard; "
                 "n = guard.default_needles(os.path.dirname(sys.argv[1])); "
                 "print(*[x.encode() in n for x in sys.argv[2:]])")
        env = dict(os.environ)
        for k in (guard.FORBID_ENV, guard.ALLOW_ENV):
            env.pop(k, None)
        env.update(USER="ada-example", HOME="/home/ada-example", **env_extra)
        r = subprocess.run([sys.executable, "-B", "-c", probe, here, *names],
                           capture_output=True, text=True, env=env)
        self.assertEqual(r.returncode, 0, r.stderr)
        return [x == "True" for x in r.stdout.split()]

    def test_allow_drops_only_the_login_name(self):
        got = self.probe_needles({guard.ALLOW_ENV: "ada-example"},
                                 ["ada-example", "/home/ada-example"])
        self.assertEqual(got, [False, True])

    def test_allow_never_drops_forbidden_strings(self):
        # Listed in both: the explicit forbid list wins, also for a plain word.
        got = self.probe_needles(
            {guard.FORBID_ENV: "Ada Example,ada@example.invalid,ada-example",
             guard.ALLOW_ENV: "Ada Example,ada@example.invalid,ada-example"},
            ["Ada Example", "ada@example.invalid", "ada-example"])
        self.assertEqual(got, [True, True, True])

    @unittest.skipUnless(IN_GIT, "not a git checkout")
    def test_forbid_file_is_gitignored(self):
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        r = subprocess.run(["git", "-C", repo, "check-ignore", "-q", guard.FORBID_FILE])
        self.assertEqual(r.returncode, 0)

    def test_extra_string_is_caught(self):
        with tempfile.TemporaryDirectory() as d:
            app = os.path.join(d, "T.app")
            write(os.path.join(app, "Contents/MacOS/app"), b"..Ada Example..")
            entries = guard.parse_manifest("Contents/MacOS/app @binary exec\n")
            errs = guard.check_tree(app, entries, needles=guard.needles_from(["Ada Example"]))
            self.assertEqual(errs, ["Contents/MacOS/app: contains the forbidden string Ada Example"])


class PinTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = self.tmp.name
        self.runner = b"runner bytes"
        lock = {
            "runners": {"macos": {
                "dir": "tools/upscalers/r",
                "runner": {"path": "run", "sha256": hashlib.sha256(self.runner).hexdigest()},
                "models": {"m.bin": hashlib.sha256(b"m").hexdigest()},
            }},
            "licenses": {"files": {"tools/upscalers/licenses/a.txt": hashlib.sha256(b"a").hexdigest()}},
        }
        write(os.path.join(self.repo, guard.LOCK), json.dumps(lock).encode())
        write(os.path.join(self.repo, "tools/upscalers/r/run"), self.runner)
        write(os.path.join(self.repo, "tools/upscalers/r/models/m.bin"), b"m")
        write(os.path.join(self.repo, "tools/upscalers/r/models/other.bin"), b"o")
        write(os.path.join(self.repo, "LICENSE"), b"gpl")
        self.pins = guard.load_pins(self.repo)
        self.app = os.path.join(self.repo, "out/T.app")

    def tearDown(self):
        self.tmp.cleanup()

    def test_stage_copies_and_sets_modes(self):
        entries = guard.parse_manifest(
            "Contents/R/run tools/upscalers/r/run exec\n"
            "Contents/R/m.bin tools/upscalers/r/models/m.bin\n"
            "Contents/L LICENSE\nContents/MacOS/x @binary exec\n")
        guard.stage(entries, self.repo, self.app, self.pins)
        self.assertEqual(os.stat(os.path.join(self.app, "Contents/R/run")).st_mode & 0o777, 0o755)
        self.assertEqual(os.stat(os.path.join(self.app, "Contents/L")).st_mode & 0o777, 0o644)
        write(os.path.join(self.app, "Contents/MacOS/x"))
        self.assertEqual(guard.check_tree(self.app, entries, self.pins), [])
        write(os.path.join(self.app, "Contents/R/run"), b"changed")
        self.assertEqual(len(guard.check_tree(self.app, entries, self.pins)), 1)

    def test_pinned_file_is_not_scanned_for_build_strings(self):
        # CI: the build machine's home equals the home of the machine that
        # built the upstream runner, so the pinned bytes contain it.
        home = "/Users/ada-example"
        self.runner = b"upstream " + home.encode() + b"/work/runner/src/x.c"
        lock_path = os.path.join(self.repo, guard.LOCK)
        with open(lock_path, encoding="utf-8") as f:
            lock = json.load(f)
        lock["runners"]["macos"]["runner"]["sha256"] = hashlib.sha256(self.runner).hexdigest()
        write(lock_path, json.dumps(lock).encode())
        write(os.path.join(self.repo, "tools/upscalers/r/run"), self.runner)
        pins = guard.load_pins(self.repo)
        entries = guard.parse_manifest(
            "Contents/R/run tools/upscalers/r/run exec\nContents/L LICENSE\n")
        guard.stage(entries, self.repo, self.app, pins)
        needles = guard.needles_from([home])
        self.assertEqual(guard.check_tree(self.app, entries, pins, needles=needles), [])
        # The same bytes in a file that is not pinned are rejected.
        write(os.path.join(self.app, "Contents/L"), self.runner)
        self.assertEqual(guard.check_tree(self.app, entries, pins, needles=needles),
                         ["Contents/L: contains the forbidden string " + home])
        # A pinned path whose bytes changed is scanned again.
        write(os.path.join(self.app, "Contents/L"), b"gpl")
        write(os.path.join(self.app, "Contents/R/run"), self.runner + b"!")
        errs = guard.check_tree(self.app, entries, pins, needles=needles)
        self.assertEqual(len(errs), 2, errs)
        self.assertIn("Contents/R/run: contains the forbidden string " + home, errs)
        # Without pins (no lock given) nothing is exempt.
        write(os.path.join(self.app, "Contents/R/run"), self.runner)
        self.assertEqual(guard.check_tree(self.app, entries, None, needles=needles),
                         ["Contents/R/run: contains the forbidden string " + home])

    def test_pinned_file_keeps_the_other_checks(self):
        entries = guard.parse_manifest("Contents/R/run tools/upscalers/r/run exec\n")
        guard.stage(entries, self.repo, self.app, self.pins)
        run = os.path.join(self.app, "Contents/R/run")
        os.link(run, os.path.join(self.repo, "outside"))
        self.assertEqual(guard.check_tree(self.app, entries, self.pins,
                                          needles=[b"runner bytes"]),
                         ["Contents/R/run: hardlink (2 links)"])

    def test_stage_rejects_hash_mismatch(self):
        write(os.path.join(self.repo, "tools/upscalers/r/run"), b"tampered")
        entries = guard.parse_manifest("Contents/R/run tools/upscalers/r/run exec\n")
        with self.assertRaises(guard.GuardError):
            guard.stage(entries, self.repo, self.app, self.pins)

    def test_stage_rejects_unpinned_upscaler_file(self):
        entries = guard.parse_manifest("Contents/R/o tools/upscalers/r/models/other.bin\n")
        with self.assertRaises(guard.GuardError):
            guard.stage(entries, self.repo, self.app, self.pins)

    def test_stage_rejects_symlink_source(self):
        os.symlink("LICENSE", os.path.join(self.repo, "LINK"))
        entries = guard.parse_manifest("Contents/L LINK\n")
        with self.assertRaises(guard.GuardError):
            guard.stage(entries, self.repo, self.app, self.pins)


WINDOWS_PROBE = r"""
import os, sys, tempfile
sys.modules["pwd"] = None          # Windows Python has no pwd module
sys.path.insert(0, sys.argv[1])
import guard
assert guard.pwd is None
os.name = "nt"                     # take the Windows branches
os.environ["USERNAME"] = "ada-example"
os.environ["USERPROFILE"] = "C:\\Users\\ada-example"
needles = guard.default_needles(os.path.dirname(sys.argv[1]))
for n in (b"ada-example", b"C:\\Users\\ada-example", b"C:/Users/ada-example"):
    assert n in needles, (n, needles)
with tempfile.TemporaryDirectory() as d:
    os.makedirs(os.path.join(d, "t"))
    with open(os.path.join(d, "t", "LICENSE"), "wb") as f:
        f.write(b"..C:/Users/ada-example/x..")
    assert guard.list_xattrs(os.path.join(d, "t", "LICENSE")) == []
    entries = guard.parse_manifest("LICENSE LICENSE\n")
    errs = guard.check_tree(os.path.join(d, "t"), entries, needles=needles)
    assert errs == ["LICENSE: contains the forbidden string C:/Users/ada-example",
                    "LICENSE: contains the forbidden string ada-example"], errs
print("ok")
"""


class WindowsHostTests(unittest.TestCase):
    def test_guard_runs_without_pwd_on_windows(self):
        here = os.path.dirname(os.path.abspath(__file__))
        r = subprocess.run([sys.executable, "-B", "-c", WINDOWS_PROBE, here],
                           capture_output=True, text=True)
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "ok"), r.stderr)


class LockTests(unittest.TestCase):
    def test_windows_pins_include_runner_dll_and_models(self):
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        pins = guard.load_pins(repo)
        base = "tools/upscalers/realesrgan-20220424-windows/"
        for name in ("realesrgan-ncnn-vulkan.exe", "vcomp140.dll",
                     "models/realesr-animevideov3-x3.param", "models/realesr-animevideov3-x3.bin"):
            self.assertIn(base + name, pins)
        self.assertNotIn(base + "vcomp140d.dll", pins)

    @unittest.skipUnless(IN_GIT, "not a git checkout")
    def test_innosetup_lock(self):
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        with open(os.path.join(repo, "tools/upscalers/innosetup.lock.json"), encoding="utf-8") as f:
            inno = json.load(f)["innosetup"]
        self.assertTrue(inno["version"].startswith("6."))
        self.assertRegex(inno["sha256"], "^[0-9a-f]{64}$")
        self.assertTrue(inno["url"].startswith("https://github.com/jrsoftware/issrc/releases/download/"))
        r = subprocess.run(["git", "-C", repo, "check-ignore", "-q", "tools/upscalers/innosetup.lock.json"])
        self.assertEqual(r.returncode, 1)  # tracked, not ignored


class HashTests(unittest.TestCase):
    def test_hashes_file_has_lf_only(self):
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "t/b/x"), b"1")
            write(os.path.join(d, "t/a"), b"2")
            out = os.path.join(d, "list.sha256")
            self.assertEqual(guard.main(["hashes", "--root", os.path.join(d, "t"), "--out", out]), 0)
            with open(out, "rb") as f:
                data = f.read()
            self.assertNotIn(b"\r", data)
            self.assertEqual(data.count(b"\n"), 2)

    def test_tree_hashes_sorted(self):
        with tempfile.TemporaryDirectory() as d:
            write(os.path.join(d, "b/x"), b"1")
            write(os.path.join(d, "a"), b"2")
            text = guard.tree_hashes(d)
            self.assertEqual(text, "%s  a\n%s  b/x\n" % (
                hashlib.sha256(b"2").hexdigest(), hashlib.sha256(b"1").hexdigest()))


if __name__ == "__main__":
    unittest.main()
