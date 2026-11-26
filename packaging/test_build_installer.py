"""Shell-level tests of packaging/windows/build-installer.sh on its ISCC route
(the Windows CI route), with a fake ISCC that records what it is given.
Run: python3 -m unittest discover -s packaging"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
SCRIPT = os.path.join(HERE, "windows", "build-installer.sh")
TREE = os.path.join(REPO, "dist", "Milionar-windows-x64")

# Records argv and the MSYS settings, then writes the setup named by /O and /F.
FAKE_ISCC = r"""#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
rec = {"args": args, "env": {k: os.environ.get(k) for k in ("MSYS_NO_PATHCONV", "MSYS2_ARG_CONV_EXCL")}}
with open(os.environ["FAKE_ISCC_LOG"], "w") as f:
    json.dump(rec, f)
out = next(a[2:] for a in args if a.startswith("/O"))
name = next(a[2:] for a in args if a.startswith("/F"))
with open(os.path.join(out, name + ".exe"), "wb") as f:
    f.write(b"MZ fake setup")
"""


class StaticTests(unittest.TestCase):
    def test_msys_settings_only_on_the_iscc_call(self):
        with open(SCRIPT, encoding="utf-8") as f:
            lines = [ln for ln in f if "MSYS_NO_PATHCONV" in ln or "MSYS2_ARG_CONV_EXCL" in ln]
        code = [ln for ln in lines if not ln.lstrip().startswith("#")]
        self.assertEqual(len(code), 1, code)
        self.assertRegex(code[0], r"run_iscc\(\) \{ MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='\*' \"\$ISCC\"")
        self.assertNotIn("export", code[0])


@unittest.skipUnless(os.path.isfile(os.path.join(TREE, "milionar.exe")),
                     "needs the tree from packaging/windows/build-tree.sh")
class IsccRouteTests(unittest.TestCase):
    def test_fake_iscc_gets_switches_and_msys_settings_only_there(self):
        with tempfile.TemporaryDirectory() as d:
            dist = os.path.join(d, "dist")
            shutil.copytree(TREE, os.path.join(dist, "Milionar-windows-x64"))
            iscc = os.path.join(d, "iscc")
            with open(iscc, "w") as f:
                f.write(FAKE_ISCC)
            os.chmod(iscc, 0o755)
            log = os.path.join(d, "iscc.json")
            env = dict(os.environ, ISCC=iscc, DIST=dist, FAKE_ISCC_LOG=log)
            for k in ("MSYS_NO_PATHCONV", "MSYS2_ARG_CONV_EXCL"):
                env.pop(k, None)
            # A python3 shim placed first in PATH records the MSYS settings every
            # later python3 call (guard check, guard scan) sees.
            shim_dir = os.path.join(d, "bin")
            os.makedirs(shim_dir)
            seen = os.path.join(d, "python-env.txt")
            with open(os.path.join(shim_dir, "python3"), "w") as f:
                f.write('#!/bin/sh\necho "${MSYS_NO_PATHCONV-unset} ${MSYS2_ARG_CONV_EXCL-unset} $*" >> "%s"\n'
                        'exec "%s" "$@"\n' % (seen, sys.executable))
            os.chmod(os.path.join(shim_dir, "python3"), 0o755)
            env["PATH"] = shim_dir + os.pathsep + env["PATH"]
            r = subprocess.run(["bash", SCRIPT], env=env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            with open(log) as f:
                rec = json.load(f)
            self.assertEqual(rec["env"], {"MSYS_NO_PATHCONV": "1", "MSYS2_ARG_CONV_EXCL": "*"})
            args = rec["args"]
            self.assertEqual(args[0], "/Qp")
            self.assertTrue(any(re.fullmatch(r"/DAppVersion=\d+\.\d+\.\d+", a) for a in args), args)
            for prefix in ("/DSourceDir=", "/DIconFile=", "/O", "/F"):
                self.assertTrue(any(a.startswith(prefix) for a in args), (prefix, args))
            self.assertTrue(args[-1].endswith("milionar.iss"), args)
            with open(seen) as f:
                # The fake ISCC is itself a python3 script; its own call is checked above.
                calls = [c for c in f.read().splitlines() if iscc not in c]
            scans = [c for c in calls if "guard.py scan" in c]
            self.assertEqual(len(scans), 1, calls)
            for c in calls:
                self.assertTrue(c.startswith("unset unset "), c)
            sha = [n for n in os.listdir(dist) if n.endswith("-setup.exe.sha256")]
            self.assertEqual(len(sha), 1, os.listdir(dist))


if __name__ == "__main__":
    unittest.main()
