#!/usr/bin/env python3
"""Stage manifest and release guard for the packaged app (stdlib only).

A stage manifest (packaging/macos/stage.txt for Milionar.app,
packaging/windows/stage.txt for the Windows install tree) is the single list of
what may ship. Each line is

    <dest inside the bundle or tree>  <source>  [exec]

where <source> is a path relative to the repository root, or a word starting
with "@" for a file the build script makes itself (binary, Info.plist, icon,
code signature). A source under tools/upscalers/ must be pinned in
tools/upscalers/upscaler.lock.json and is copied only when its SHA-256 matches.
A dest ending in "?" is made by signing; `check --unsigned` lets it be absent.

Commands (paths relative to the repository root or absolute):

    guard.py stage  --manifest M --app APP      copy the repository sources
    guard.py check  --manifest M --app APP [--unsigned]
    guard.py hashes --root DIR --out FILE       sha256sum-style list of a tree
    guard.py scan FILE...                       forbidden strings in single files
                                                (an installer, a disk image)

check rejects forbidden files (game data and the original's binaries, see
FORBIDDEN_* and OWN_EXE), symlinks, special files, anything not in the manifest, missing
entries, hardlinks, extended attributes other than com.apple.provenance,
pinned files whose hash changed, and files that contain strings of the build
machine or its user (pinned files whose hash matches are not scanned: their
bytes are upstream's, fixed by the lock; see default_needles; extra ones in $MILIONAR_GUARD_FORBID
or the gitignored packaging/.guard-forbid.local; $MILIONAR_GUARD_ALLOW drops
plain login names, for CI runners whose user name is a common word).
"""

import argparse
import ctypes
import getpass
import hashlib
import json
import os
try:
    import pwd  # Unix only
except ImportError:
    pwd = None
import shutil
import stat
import subprocess
import sys

FORBIDDEN_EXTS = {".smk", ".dat", ".wav", ".iso", ".sav", ".upk"}
FORBIDDEN_NAMES = {"milionar.exe", "smackw32.dll", "game.ico"}
# The original game's executable is Milionar.exe, a 32-bit i386 PE. Our Windows
# binary has the same name in lower case. The name stays forbidden everywhere
# (any case) except for exactly one file: dest OWN_EXE (tree root, this exact
# lower-case spelling) whose manifest source is @binary (built by the build
# script) and which, in the built tree, is a PE image for x86-64 (machine
# 0x8664). The original exe can never pass that: wrong machine, and a
# Milionar.exe spelled any other way or anywhere else is rejected by name.
OWN_EXE = "milionar.exe"
PE_MACHINE_AMD64 = 0x8664
PINNED_PREFIX = "tools/upscalers/"
LOCK = "tools/upscalers/upscaler.lock.json"
CHUNK = 1 << 20


class GuardError(Exception):
    pass


def forbidden_reason(rel, own_exe=False):
    """Why the relative path `rel` may never ship, or None. `own_exe` says that
    `rel` is our own built Windows binary (see OWN_EXE); it lifts the name ban
    only for the exact path OWN_EXE."""
    name = rel.replace("\\", "/").rsplit("/", 1)[-1].lower()
    if name in FORBIDDEN_NAMES and not (own_exe and rel == OWN_EXE):
        return "forbidden file name"
    ext = os.path.splitext(name)[1]
    if ext in FORBIDDEN_EXTS:
        return "forbidden extension " + ext
    return None


def _check_rel(rel, what):
    if not rel or rel.startswith("/") or "\\" in rel:
        raise GuardError("%s %r must be a relative path with /" % (what, rel))
    if any(p in ("", ".", "..") for p in rel.split("/")):
        raise GuardError("%s %r has an empty, . or .. component" % (what, rel))


class Entry:
    def __init__(self, dest, source, exe=False, optional=False):
        self.dest = dest
        self.source = source
        self.exe = exe
        self.optional = optional

    @property
    def generated(self):
        return self.source.startswith("@")


def parse_manifest(text):
    entries = []
    seen = set()
    for n, line in enumerate(text.splitlines(), 1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) not in (2, 3) or (len(parts) == 3 and parts[2] != "exec"):
            raise GuardError("manifest line %d: expected '<dest> <source> [exec]'" % n)
        dest, source = parts[0], parts[1]
        optional = dest.endswith("?")
        dest = dest.rstrip("?")
        _check_rel(dest, "line %d: dest" % n)
        if not source.startswith("@"):
            _check_rel(source, "line %d: source" % n)
        for p, own in ((dest, source == "@binary"), (source, False)):
            why = forbidden_reason(p, own)
            if why:
                raise GuardError("manifest line %d: %s: %s" % (n, p, why))
        if dest in seen:
            raise GuardError("manifest line %d: duplicate dest %s" % (n, dest))
        seen.add(dest)
        entries.append(Entry(dest, source, len(parts) == 3, optional))
    if not entries:
        raise GuardError("empty manifest")
    return entries


def load_manifest(path):
    with open(path, encoding="utf-8") as f:
        return parse_manifest(f.read())


def lock_pins(lock):
    """Repository path -> pinned SHA-256 of every runner file (macOS and
    Windows: runner, extra files such as vcomp140.dll, models) and licence."""
    pins = {}
    for runner in lock["runners"].values():
        base = runner["dir"].rstrip("/")
        pins[base + "/" + runner["runner"]["path"]] = runner["runner"]["sha256"]
        for name, sha in runner.get("extra_files", {}).items():
            pins[base + "/" + name] = sha
        for name, sha in runner["models"].items():
            pins[base + "/models/" + name] = sha
    for path, sha in lock["licenses"]["files"].items():
        pins[path] = sha
    return pins


def load_pins(repo):
    with open(os.path.join(repo, LOCK), encoding="utf-8") as f:
        return lock_pins(json.load(f))


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                return h.hexdigest()
            h.update(b)


def pin_for(entry, pins):
    if entry.generated or not entry.source.startswith(PINNED_PREFIX):
        return None
    if entry.source not in pins:
        raise GuardError("%s is under %s but not pinned in %s" % (entry.source, PINNED_PREFIX, LOCK))
    return pins[entry.source]


def stage(entries, repo, app, pins):
    """Copies every repository-sourced entry into `app`, checking pins."""
    for e in entries:
        if e.generated:
            continue
        src = os.path.join(repo, e.source)
        st = os.lstat(src)
        if not stat.S_ISREG(st.st_mode):
            raise GuardError("source %s is not a regular file" % e.source)
        want = pin_for(e, pins)
        if want is not None:
            got = sha256_file(src)
            if got != want:
                raise GuardError("%s has sha256 %s, pinned %s" % (e.source, got, want))
        dst = os.path.join(app, e.dest)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copyfile(src, dst)
        os.chmod(dst, 0o755 if e.exe else 0o644)


def pe_machine(path):
    """The COFF machine field of a PE image, or None when `path` is not one."""
    with open(path, "rb") as f:
        head = f.read(64)
        if len(head) < 64 or head[:2] != b"MZ":
            return None
        f.seek(int.from_bytes(head[0x3C:0x40], "little"))
        sig = f.read(6)
    if len(sig) < 6 or sig[:4] != b"PE\0\0":
        return None
    return int.from_bytes(sig[4:6], "little")


def _contains(path, needles):
    """The needles (bytes) found in the file."""
    if not needles:
        return []
    keep = max(len(n) for n in needles) - 1
    found = set()
    tail = b""
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            buf = tail + b
            for n in needles:
                if n in buf:
                    found.add(n)
            tail = buf[-keep:] if keep else b""
    return sorted(found)


FORBID_ENV = "MILIONAR_GUARD_FORBID"
FORBID_FILE = "packaging/.guard-forbid.local"
# Comma-separated login names that are not needles (CI: the GitHub macOS
# runner's user is "runner", a word our own notices use). Only login names are
# dropped, never paths, $MILIONAR_GUARD_FORBID / .guard-forbid.local entries or
# the git identity; entries with / \ @ or whitespace are ignored.
ALLOW_ENV = "MILIONAR_GUARD_ALLOW"
# Extended attributes allowed on shipped files. com.apple.provenance is set by
# the system on files written by a provenance-tracked process and cannot be
# removed by the user; it is an opaque id, not a path or a name. codesign
# writes no xattrs inside a bundle (the signature is embedded in the Mach-O
# and in _CodeSignature/CodeResources).
ALLOWED_XATTRS = {"com.apple.provenance"}
MIN_NEEDLE = 4


def _git_global(key):
    try:
        out = subprocess.run(["git", "config", "--global", "--get", key],
                             capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() or None


def _git_local(repo, key):
    try:
        out = subprocess.run(["git", "-C", repo, "config", "--local", "--get", key],
                             capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() or None


def extra_needles(env=None, repo=None):
    """Forbidden strings from $MILIONAR_GUARD_FORBID (comma-separated) and the
    gitignored packaging/.guard-forbid.local (one per line, # comments)."""
    env = os.environ if env is None else env
    out = [x.strip() for x in env.get(FORBID_ENV, "").split(",")]
    if repo is not None:
        try:
            with open(os.path.join(repo, FORBID_FILE), encoding="utf-8") as f:
                out += [ln.split("#", 1)[0].strip() for ln in f]
        except FileNotFoundError:
            pass
    return [x for x in out if x]


def needles_from(strings, keep=()):
    """Byte needles of the strings long enough to search for, minus `keep`."""
    keep = {k for k in keep if k}
    out = set()
    for s in strings:
        if not s or s in keep:
            continue
        s = s.rstrip("/") or s
        if len(s) >= MIN_NEEDLE and s not in ("/", "~"):
            out.add(s.encode())
    return sorted(out)


def default_needles(repo):
    """Strings of the build machine and its user that must not end up in an
    artifact, all read at run time: home, checkout, Cargo and rustup homes, the
    login name, the global git name and e-mail (unless they are the identity
    configured for this repository) and the extra strings of extra_needles.
    On Windows also USERNAME, USERPROFILE and HOME (Git Bash), each path in
    its \\ and / spelling."""
    paths = [os.path.expanduser("~"), os.path.realpath(repo),
             os.environ.get("CARGO_HOME"), os.environ.get("RUSTUP_HOME")]
    logins = [os.environ.get("USER")]
    if pwd is not None and hasattr(os, "getuid"):
        try:
            logins.append(pwd.getpwuid(os.getuid()).pw_name)
        except KeyError:
            pass
    if os.name == "nt":
        logins.append(os.environ.get("USERNAME"))
        paths += [os.environ.get("USERPROFILE"), os.environ.get("HOME")]
        try:
            logins.append(getpass.getuser())
        except (OSError, KeyError, ImportError):
            pass
        paths += [x.replace("\\", "/") for x in paths if x and "\\" in x]
    # $MILIONAR_GUARD_ALLOW drops login names only: never a path, an explicit
    # forbidden string or the git identity.
    allow = set(allowed_words())
    logins = [x for x in logins if x not in allow]
    keep = [_git_local(repo, "user.name"), _git_local(repo, "user.email")]
    git = needles_from([_git_global("user.name"), _git_global("user.email")], keep)
    return sorted(set(needles_from(paths + logins + extra_needles(repo=repo))) | set(git))


def allowed_words(env=None):
    """The entries of $MILIONAR_GUARD_ALLOW: plain login-like words only. An
    entry with a path separator, an @ or whitespace is ignored."""
    env = os.environ if env is None else env
    out = [x.strip() for x in env.get(ALLOW_ENV, "").split(",")]
    return [x for x in out
            if x and not any(c in x for c in "/\\@") and not any(c.isspace() for c in x)]


def list_xattrs(path):
    """Names of the extended attributes of `path` (symlinks not followed).
    Always empty on Windows: NTFS has no xattrs this build could set."""
    if os.name == "nt":
        return []
    if hasattr(os, "listxattr"):  # Linux
        return os.listxattr(path, follow_symlinks=False)
    if sys.platform != "darwin":
        return []
    libc = ctypes.CDLL(None, use_errno=True)
    libc.listxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_int]
    libc.listxattr.restype = ctypes.c_ssize_t
    nofollow = 0x0001  # XATTR_NOFOLLOW
    raw = os.fsencode(path)
    size = libc.listxattr(raw, None, 0, nofollow)
    if size < 0:
        e = ctypes.get_errno()
        raise OSError(e, os.strerror(e), path)
    if size == 0:
        return []
    buf = ctypes.create_string_buffer(size)
    size = libc.listxattr(raw, buf, size, nofollow)
    if size < 0:
        e = ctypes.get_errno()
        raise OSError(e, os.strerror(e), path)
    return [n.decode("utf-8", "replace") for n in buf.raw[:size].split(b"\0") if n]


def _xattrs(path):
    """Extended attributes of `path` outside ALLOWED_XATTRS."""
    return sorted(n for n in list_xattrs(path) if n not in ALLOWED_XATTRS)


def check_tree(app, entries, pins=None, repo=None, unsigned=False, needles=()):
    """Every problem with the staged tree, as messages (empty when it is fine)."""
    errors = []
    wanted = {e.dest: e for e in entries}
    allowed_dirs = set()
    for d in wanted:
        parts = d.split("/")[:-1]
        for i in range(1, len(parts) + 1):
            allowed_dirs.add("/".join(parts[:i]))
    found = set()
    if not os.path.isdir(app) or os.path.islink(app):
        return ["%s is not a directory" % app]
    for x in _xattrs(app):
        errors.append("(app root): extended attribute %s" % x)

    def walk_error(e):
        errors.append("%s: cannot read (%s)" % (e.filename, e.strerror))

    for dirpath, dirnames, filenames in os.walk(app, followlinks=False, onerror=walk_error):
        reldir = os.path.relpath(dirpath, app)
        reldir = "" if reldir == "." else reldir.replace(os.sep, "/")
        for name in list(dirnames) + filenames:
            rel = (reldir + "/" + name) if reldir else name
            full = os.path.join(dirpath, name)
            st = os.lstat(full)
            own = (rel == OWN_EXE and rel in wanted and wanted[rel].source == "@binary"
                   and stat.S_ISREG(st.st_mode) and pe_machine(full) == PE_MACHINE_AMD64)
            why = forbidden_reason(rel, own)
            if why:
                errors.append("%s: %s" % (rel, why))
            if stat.S_ISLNK(st.st_mode):
                errors.append("%s: symlink" % rel)
                continue
            for x in _xattrs(full):
                errors.append("%s: extended attribute %s" % (rel, x))
            if stat.S_ISDIR(st.st_mode):
                if rel not in allowed_dirs:
                    errors.append("%s: directory not in the stage manifest" % rel)
                continue
            if not stat.S_ISREG(st.st_mode):
                errors.append("%s: not a regular file" % rel)
                continue
            if st.st_nlink > 1:
                errors.append("%s: hardlink (%d links)" % (rel, st.st_nlink))
            if rel not in wanted:
                errors.append("%s: not in the stage manifest" % rel)
                continue
            found.add(rel)
            e = wanted.get(rel)
            pinned_ok = False
            if e is not None and pins is not None:
                want = pin_for(e, pins)
                if want is not None:
                    if sha256_file(full) != want:
                        errors.append("%s: sha256 differs from the pin %s" % (rel, want))
                    else:
                        pinned_ok = True
            if pinned_ok:
                # Upstream bytes fixed by the lock (the macOS runner was built
                # on a CI machine whose home is the same as the GitHub macOS
                # runner's): nothing of this build machine can be in them, so
                # the needle scan is skipped. Every other check still applies.
                continue
            for n in _contains(full, list(needles)):
                errors.append("%s: contains the forbidden string %s" % (rel, n.decode(errors="replace")))
        dirnames.sort()
    for d, e in sorted(wanted.items()):
        if d not in found and not (unsigned and e.optional):
            errors.append("%s: missing" % d)
    return errors


def tree_hashes(root):
    """sha256sum-style lines for every regular file under root, sorted by path."""
    lines = []
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        for name in filenames:
            full = os.path.join(dirpath, name)
            if os.path.islink(full):
                raise GuardError("%s: symlink" % full)
            rel = os.path.relpath(full, root).replace(os.sep, "/")
            lines.append((rel, sha256_file(full)))
    lines.sort()
    return "".join("%s  %s\n" % (h, r) for r, h in lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("stage", "check"):
        p = sub.add_parser(name)
        p.add_argument("--manifest", required=True)
        p.add_argument("--app", required=True)
        if name == "check":
            p.add_argument("--unsigned", action="store_true",
                           help="the entries marked ? (code signature) are not there yet")
    p = sub.add_parser("scan")
    p.add_argument("files", nargs="+")
    p = sub.add_parser("hashes")
    p.add_argument("--root", required=True)
    p.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    try:
        if a.cmd == "hashes":
            text = tree_hashes(a.root)
            # LF on every OS: the list made on macOS is compared byte for byte
            # with one made on Windows.
            with open(a.out, "w", encoding="utf-8", newline="\n") as f:
                f.write(text)
            return 0
        if a.cmd == "scan":
            needles = default_needles(repo)
            bad = 0
            for path in a.files:
                for n in _contains(path, needles):
                    print("guard: %s: contains the forbidden string %s"
                          % (path, n.decode(errors="replace")), file=sys.stderr)
                    bad += 1
            if bad:
                return 1
            print("guard: no forbidden strings in %d file(s)" % len(a.files))
            return 0
        entries = load_manifest(a.manifest)
        pins = load_pins(repo)
        if a.cmd == "stage":
            stage(entries, repo, a.app, pins)
            return 0
        errors = check_tree(a.app, entries, pins, repo, a.unsigned, default_needles(repo))
    except GuardError as e:
        print("guard: " + str(e), file=sys.stderr)
        return 1
    for e in errors:
        print("guard: " + e, file=sys.stderr)
    if errors:
        print("guard: %d problem(s) in %s" % (len(errors), a.app), file=sys.stderr)
        return 1
    print("guard: %s matches the stage manifest (%d entries)" % (a.app, len(entries)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
