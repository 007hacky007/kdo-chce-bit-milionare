#!/usr/bin/env python3
"""Hygiene scan of a public export tree (stdlib only).

    hygiene.py --repo REPO DIR

Fails (exit 1) when any file or path under DIR has
  - a forbidden name or extension of the original game's files (game data,
    Milionar.exe, Smackw32.dll, game.ico; the same list as packaging/guard.py);
  - a home path: /Users/<name>, /home/<name> or <drive>:\\Users\\<name> (either
    slash, any case), other than the made-up names in PLACEHOLDER_USERS (tests
    use them for long-path layout);
  - a Unicode dash (only the ASCII hyphen-minus is allowed in text);
  - a marker of the maintainer's internal planning documents (INTERNAL_MARKERS:
    phrases from their fixed headers that public files never contain), a second
    line of defence behind the export's exclude list;
  - a string of the build machine or its user: the needles of
    packaging/guard.py default_needles(REPO) (home, checkout path, login name,
    global git name and e-mail unless they are this repository's identity,
    $MILIONAR_GUARD_FORBID and the gitignored packaging/.guard-forbid.local),
    matched without regard to case.

A .git directory under DIR is skipped. Nothing is printed of a needle except
the needle itself on a hit, so run it where that output stays private.
"""

import argparse
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(HERE)), "packaging"))
import guard  # noqa: E402

# Unicode dashes: hyphen, non-breaking hyphen, figure dash, en dash, em dash,
# horizontal bar, minus sign, small em dash, small hyphen-minus, fullwidth
# hyphen-minus, two-em and three-em dash.
DASHES = "\u2010\u2011\u2012\u2013\u2014\u2015\u2212\ufe58\ufe63\uff0d\u2e3a\u2e3b"
# Made-up user names of test fixtures.
PLACEHOLDER_USERS = {"someone", "n\u011bkdo", "ada-example"}
# /Users/<name>, /home/<name>, C:\Users\<name> (single or doubled backslashes,
# as in source code), any case. The name ends at a separator, quote, space or
# placeholder syntax ($HOME, <name>, {user}).
HOME_RE = re.compile(
    rb"(?:/(?:users|home)/|[a-z]:\\{1,2}users\\{1,2})"
    rb"([^/\\\s\"'`<>()\[\]{},;:$*]*)", re.IGNORECASE)

# Header phrases of the internal plans and design spec. Split so that this file
# (which is exported) does not match itself.
INTERNAL_MARKERS = [m.encode() for m in (
    "For agentic" + " workers", "REQUIRED SUB" + "-SKILL",
    "## User" + " decisions", "## Anti" + "-goals (failure",
)]


def scan_bytes(rel, data, needles):
    """Problems in one file's contents."""
    errors = []
    for m in HOME_RE.finditer(data):
        name = m.group(1).decode("utf-8", "replace")
        if name and name not in PLACEHOLDER_USERS:
            errors.append("%s: home path %s" % (rel, m.group(0).decode("utf-8", "replace")))
            break
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        text = None  # binary (font): no dash check
    if text is not None:
        for i, line in enumerate(text.splitlines(), 1):
            bad = sorted({c for c in line if c in DASHES})
            if bad:
                errors.append("%s:%d: Unicode dash %s" % (
                    rel, i, " ".join("U+%04X" % ord(c) for c in bad)))
    for m in INTERNAL_MARKERS:
        if m in data:
            errors.append("%s: internal planning document (marker %r)" % (rel, m.decode()))
            break
    low = data.lower()
    for n in needles:
        if n.lower() in low:
            errors.append("%s: contains the forbidden string %s"
                          % (rel, n.decode("utf-8", "replace")))
    return errors


def scan_tree(root, needles):
    """Every problem under root, as messages (empty when clean)."""
    errors = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames[:] = sorted(d for d in dirnames
                             if not (dirpath == root and d == ".git"))
        for name in sorted(filenames) + [d for d in dirnames]:
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, root).replace(os.sep, "/")
            why = guard.forbidden_reason(rel)
            if why:
                errors.append("%s: %s" % (rel, why))
            if any(c in DASHES for c in rel):
                errors.append("%s: Unicode dash in the path" % rel)
            if os.path.isdir(full) and not os.path.islink(full):
                continue
            if os.path.islink(full):
                target = os.readlink(full).encode()
                errors += scan_bytes(rel + " (symlink)", target, needles)
                continue
            with open(full, "rb") as f:
                errors += scan_bytes(rel, f.read(), needles)
    return errors


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--repo", required=True, help="source repository (for the needles)")
    ap.add_argument("dir")
    a = ap.parse_args(argv)
    needles = guard.default_needles(os.path.abspath(a.repo))
    errors = scan_tree(os.path.abspath(a.dir), needles)
    for e in errors:
        print("hygiene: " + e, file=sys.stderr)
    if errors:
        print("hygiene: %d problem(s) in %s" % (len(errors), a.dir), file=sys.stderr)
        return 1
    print("hygiene: %s is clean" % a.dir)
    return 0


if __name__ == "__main__":
    sys.exit(main())
