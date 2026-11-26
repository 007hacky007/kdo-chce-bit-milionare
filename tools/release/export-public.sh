#!/usr/bin/env bash
# Writes a public source tree of one commit, without the private git history.
#
#   tools/release/export-public.sh <out_dir> [--commit <rev>] [--with-analysis] [--init-git]
#
# - <out_dir> must not exist or be empty.
# - The files of <rev> (default HEAD; uncommitted changes are never exported) come
#   from `git archive`, minus the paths listed in the gitignored
#   tools/release/export-exclude.local (one git pathspec per line, # comments; the
#   file must exist and list at least one path: it names the maintainer's internal
#   notes without naming them here) and, unless --with-analysis is given, analysis/
#   (reverse-engineering notes and tables of the original game: whether they go
#   public is the owner's decision). Gitignored and untracked files are never in a
#   commit, so they never reach the export.
# - ENGINE_REVISION in <out_dir> holds the full source commit hash.
# - tools/release/hygiene.py then scans the tree (home paths, Unicode dashes, the
#   original game's file names and extensions, strings of this machine and user,
#   $MILIONAR_GUARD_FORBID and packaging/.guard-forbid.local); on any hit the
#   export fails and <out_dir> is removed.
# - --init-git makes <out_dir> a new git repository (branch master) with one
#   commit authored by the public pseudonym, signing off and global/system git
#   configuration ignored. Nothing is pushed anywhere.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
pseudonym="007hacky007"
pseudonym_email="007hacky007@users.noreply.github.com"

usage() { echo "usage: $0 <out_dir> [--commit <rev>] [--with-analysis] [--init-git]" >&2; exit 2; }

out=""
rev=HEAD
with_analysis=0
init_git=0
while [ $# -gt 0 ]; do
    case "$1" in
        --commit) [ $# -ge 2 ] || usage; rev="$2"; shift 2 ;;
        --with-analysis) with_analysis=1; shift ;;
        --init-git) init_git=1; shift ;;
        -*) usage ;;
        *) [ -z "$out" ] || usage; out="$1"; shift ;;
    esac
done
[ -n "$out" ] || usage

exclude_file="$root/tools/release/export-exclude.local"
[ -f "$exclude_file" ] || {
    echo "error: $exclude_file is missing (one pathspec per line, at least one)" >&2
    exit 1
}

commit="$(git -C "$root" rev-parse --verify --quiet "$rev^{commit}")" \
    || { echo "error: $rev is not a commit" >&2; exit 1; }

if [ -e "$out" ]; then
    [ -d "$out" ] && [ -z "$(ls -A "$out")" ] \
        || { echo "error: $out exists and is not an empty directory" >&2; exit 1; }
fi
mkdir -p "$out"
out="$(cd "$out" && pwd -P)"
case "$out/" in
    "$(cd "$root" && pwd -P)/"*) echo "error: $out is inside the repository" >&2; rmdir "$out"; exit 1 ;;
esac

ok=0
cleanup() { if [ "$ok" != 1 ]; then rm -rf "$out"; fi; }
trap cleanup EXIT

excludes=()
skip=()
while IFS= read -r line || [ -n "$line" ]; do
    line="${line%%#*}"
    line="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    line="${line#/}"
    line="${line%/}"
    [ -n "$line" ] || continue
    skip+=("$line")
done < "$exclude_file"
if [ ${#skip[@]} -eq 0 ]; then
    echo "error: $exclude_file lists no path; refusing to export without the internal exclusions" >&2
    exit 1
fi
# A listed path that exists untracked in the checkout should also be ignored by git
# (the ignore lines live in the local .git/info/exclude, not in .gitignore).
for p in "${skip[@]}"; do
    if [ -e "$root/$p" ] && [ -z "$(git -C "$root" ls-files -- "$p" | head -1)" ] \
        && ! git -C "$root" check-ignore -q -- "$p"; then
        echo "warning: $p exists and is not ignored by git; add /$p/ to .git/info/exclude" >&2
    fi
done
if [ "$with_analysis" != 1 ]; then
    skip+=(analysis)
fi
for p in ${skip[@]+"${skip[@]}"}; do
    excludes+=(":(exclude)$p")
done
git -C "$root" archive --format=tar "$commit" -- . ${excludes[@]+"${excludes[@]}"} | tar -xf - -C "$out"
for p in ${skip[@]+"${skip[@]}"}; do
    if [ -e "$out/$p" ]; then
        echo "error: excluded path $p is in the export" >&2
        exit 1
    fi
done
printf '%s\n' "$commit" > "$out/ENGINE_REVISION"

python3 -B "$root/tools/release/hygiene.py" --repo "$root" "$out"

if [ "$init_git" = 1 ]; then
    version="$(python3 -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["package"]["version"])' "$out/apps/milionar/Cargo.toml")"
    # No global or system configuration (hooks, templates, signing, identity).
    g() {
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
            GIT_AUTHOR_NAME="$pseudonym" GIT_AUTHOR_EMAIL="$pseudonym_email" \
            GIT_COMMITTER_NAME="$pseudonym" GIT_COMMITTER_EMAIL="$pseudonym_email" \
            git -C "$out" "$@"
    }
    g init -q -b master
    g config user.name "$pseudonym"
    g config user.email "$pseudonym_email"
    g config commit.gpgsign false
    g add -A
    g commit -q -m "Kdo chce bit milionare port $version"
    echo "git: $(g log --format='%h %an <%ae> %s' -1)"
fi

ok=1
echo "exported $commit to $out"
