#!/bin/bash
# Rebuild third_party/rqs_lib from upstream and the patches here, and
# compare it with what is committed.
#
#     third_party/rqs_lib.patches/check.sh [UPSTREAM_CLONE]
#
# Reads repo, commit and subdir from third_party/rqs_lib.UPSTREAM, takes
# that subdirectory at that commit -- from UPSTREAM_CLONE (or
# $VENDOR_UPSTREAM_DIR, as ci/vendor-check.sh takes it) when given, else
# from a fresh shallow fetch -- applies every NNNN-*.patch in this
# directory in lexical order with `git apply -p1` from the subdirectory's
# root, and diffs the result against third_party/rqs_lib. Any difference,
# a patch that does not apply, or a missing input exits non-zero.
#
# What is compared on the vendored side is what git would commit: tracked
# files and untracked ones the vendored .gitignore does not exclude. The
# crate's own test build leaves target/ and Cargo.lock there (both
# ignored); they are not part of the tree. An edit that is not in a patch,
# committed or not, is.
#
# ci/vendor-check.sh checks the same thing with `patch` in CI; this is the
# one to run before committing a change to the vendored copy or its
# patches. To change the vendored code: edit it, then turn the edit into a
# patch (or fold it into the patch of its concern) until this passes.
set -euo pipefail

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "$here/../.." && pwd)
vendored="$root/third_party/rqs_lib"
pin="$root/third_party/rqs_lib.UPSTREAM"

fail() {
    echo "rqs_lib check: FAIL $*" >&2
    exit 1
}

for tool in git tar diff; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool not found"
done
[[ -f "$pin" ]] || fail "${pin#"$root"/} is missing"
[[ -d "$vendored" ]] || fail "third_party/rqs_lib is missing"

# Exactly three lines: repo=, commit=, subdir=.
[[ $(wc -l < "$pin") -eq 3 ]] || fail "${pin#"$root"/} must have exactly three lines"
repo=$(sed -n 's/^repo=//p' "$pin")
commit=$(sed -n 's/^commit=//p' "$pin")
subdir=$(sed -n 's/^subdir=//p' "$pin")
[[ -n "$repo" && -n "$subdir" ]] || fail "${pin#"$root"/} needs repo= and subdir="
[[ $commit =~ ^[0-9a-f]{40}$ ]] || fail "commit= must be a full 40-hex commit id"
[[ $subdir != */* && $subdir != .* ]] || fail "subdir= must be one plain directory name"

shopt -s nullglob
patches=("$here"/[0-9][0-9][0-9][0-9]-*.patch)
shopt -u nullglob
[[ ${#patches[@]} -gt 0 ]] || fail "no NNNN-*.patch files in ${here#"$root"/}"
# Lexical order, independent of the locale.
mapfile -t patches < <(printf '%s\n' "${patches[@]}" | LC_ALL=C sort)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# git apply below runs outside any repository; keep git from finding one
# above the scratch directory (a TMPDIR inside a checkout, say).
export GIT_CEILING_DIRECTORIES="$work"

source_repo=${1:-${VENDOR_UPSTREAM_DIR:-}}
if [[ -n "$source_repo" ]]; then
    git -C "$source_repo" cat-file -e "$commit^{commit}" 2>/dev/null ||
        fail "$source_repo does not have $commit"
else
    source_repo="$work/upstream.git"
    git init -q --bare "$source_repo"
    git -C "$source_repo" fetch -q --depth 1 "$repo" "$commit" ||
        fail "could not fetch $commit from $repo"
fi

mkdir -p "$work/rebuilt"
git -C "$source_repo" archive --format=tar "$commit" "$subdir" | tar -x -C "$work/rebuilt" ||
    fail "$commit has no $subdir"
base="$work/rebuilt/$subdir"

for p in "${patches[@]}"; do
    (cd "$base" && git apply -p1 --whitespace=nowarn "$p") ||
        fail "${p#"$root"/} does not apply"
    echo "rqs_lib check: applied ${p#"$root"/}"
done

# The vendored tree as git sees it (see the header).
mkdir -p "$work/vendored"
(
    cd "$root"
    git ls-files -z --cached --others --exclude-standard -- third_party/rqs_lib |
        while IFS= read -r -d '' f; do
            # A tracked file deleted from the working tree is a difference.
            [[ -e "$f" || -L "$f" ]] || continue
            dest="$work/vendored/${f#third_party/rqs_lib/}"
            mkdir -p "$(dirname -- "$dest")"
            cp -P -- "$f" "$dest"
        done
)

if ! diff -r "$base" "$work/vendored"; then
    fail "third_party/rqs_lib is not upstream $commit plus ${#patches[@]} patches"
fi
echo "rqs_lib check: ok: third_party/rqs_lib is upstream ${commit:0:7} plus ${#patches[@]} patches"
