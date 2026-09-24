#!/bin/bash
# Prove that ci/vendor-check.sh still refuses a vendored crate that is not
# exactly upstream plus its patches -- offline, against an upstream this
# test makes: a throwaway git repository with a crate in a subdirectory,
# the shape of open-quickshare's core_lib.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
status=0
cases=0

# The upstream: a repository whose core_lib/ is the crate.
up="$work/upstream"
mkdir -p "$up/core_lib/src" "$up/core_lib/examples" "$up/core_lib/bindings"
printf '[package]\nname = "rqs_lib"\nversion = "0.11.5"\n' > "$up/core_lib/Cargo.toml"
printf 'pub fn name(n: &str) -> &str {\n    n\n}\n' > "$up/core_lib/src/lib.rs"
printf 'pub fn size(s: i64) -> i64 {\n    s\n}\n' > "$up/core_lib/src/size.rs"
printf 'fn main() {}\n' > "$up/core_lib/examples/demo.rs"
printf '{}\n' > "$up/core_lib/bindings/package.json"
printf 'top-level readme\n' > "$up/README.md"
git -C "$up" init -q
git -C "$up" add -A
git -C "$up" -c user.name=t -c user.email=t@example.invalid commit -qm upstream
commit=$(git -C "$up" rev-parse HEAD)

# The patches, made the way a vendoring would make them.
patched="$work/patched"
cp -a "$up/core_lib" "$patched"
sed -i 's/    n$/    n.rsplit(\x27\/\x27).next().unwrap_or("received-file")/' "$patched/src/lib.rs"
first=$(cd "$work" && diff -u upstream/core_lib/src/lib.rs patched/src/lib.rs |
    sed -e 's|^--- upstream/core_lib/|--- a/|' -e 's|^+++ patched/|+++ b/|')
cp -a "$patched" "$work/patched2"
sed -i 's/    s$/    s.max(0)/' "$work/patched2/src/size.rs"
second=$(cd "$work" && diff -u patched/src/size.rs patched2/src/size.rs |
    sed -e 's|^--- patched/|--- a/|' -e 's|^+++ patched2/|+++ b/|')

stage() {
    local dir="$work/tree"
    rm -rf "$dir"
    mkdir -p "$dir/ci" "$dir/third_party/patches/rqs_lib"
    cp "$root/ci/vendor-check.sh" "$dir/ci/"
    printf 'rqs_lib  file://%s  %s  core_lib  third_party/rqs_lib\n' "$up" "$commit" > "$dir/ci/vendor.conf"
    cp -a "$work/patched2" "$dir/third_party/rqs_lib"
    # Not vendored: outside the crate's build.
    rm -rf "$dir/third_party/rqs_lib/examples" "$dir/third_party/rqs_lib/bindings"
    printf '%s\n' "$first" > "$dir/third_party/patches/rqs_lib/0001-q1-names.patch"
    printf '%s\n' "$second" > "$dir/third_party/patches/rqs_lib/0002-q3-sizes.patch"
    tree=$dir
    return 0
}

expect() {
    local want=$1 what=$2 change=$3 got out
    cases=$((cases + 1))
    stage
    ( cd "$tree" && eval "$change" ) || { echo "selftest: FAIL could not apply '$what'" >&2; status=1; return 0; }
    if out=$(VENDOR_UPSTREAM_DIR='' "$tree/ci/vendor-check.sh" 2>&1); then got=pass; else got=fail; fi
    if [[ "$got" = "$want" ]]; then
        echo "selftest: ok   $what -> $want"
    else
        echo "selftest: FAIL $what should $want, got $got" >&2
        echo "$out" >&2
        status=1
    fi
    return 0
}

# shellcheck disable=SC2016 # the case scripts are expanded by eval, later
{
expect pass "upstream plus its patches" ':'
expect fail "a stray edit in the vendored copy" \
    'printf "// tweak\n" >> third_party/rqs_lib/src/lib.rs'
expect fail "a file added without a patch" \
    'printf "fn x() {}\n" > third_party/rqs_lib/src/extra.rs'
expect fail "a source file dropped without a patch" \
    'rm third_party/rqs_lib/src/size.rs'
expect fail "the manifest dropped" \
    'rm third_party/rqs_lib/Cargo.toml'
expect pass "upstream's examples kept after all" \
    'cp -a "$up/core_lib/examples" third_party/rqs_lib/'
expect fail "a patch that no longer applies" \
    'sed -i "s/unwrap_or/unwrap_or_else/" third_party/patches/rqs_lib/0001-q1-names.patch'
expect fail "no patches at all" \
    'rm -rf third_party/patches'
expect fail "patches in two places" \
    'mkdir -p third_party/rqs_lib-patches && cp third_party/patches/rqs_lib/*.patch third_party/rqs_lib-patches/'
expect pass "a series file in the right order" \
    'printf "0001-q1-names.patch\n0002-q3-sizes.patch\n" > third_party/patches/rqs_lib/series'
expect fail "a series file naming a missing patch" \
    'printf "0001-q1-names.patch\n0003-gone.patch\n" > third_party/patches/rqs_lib/series'
expect pass "patches made against upstream's repository root" \
    'sed -i "s|^--- a/|--- a/core_lib/|; s|^+++ b/|+++ b/core_lib/|" third_party/patches/rqs_lib/*.patch'
expect pass "one patch file, piirit-style" \
    'cat third_party/patches/rqs_lib/*.patch > third_party/rqs_lib.patch && rm -rf third_party/patches'
expect fail "a commit that is not a full id" \
    'sed -i "s/  $commit  / ${commit:0:7} /" ci/vendor.conf'
expect fail "a vendored crate that is not there" \
    'rm -rf third_party/rqs_lib'
}

echo
if [[ "$status" -eq 0 ]]; then
    echo "selftest: ok ($cases cases)"
else
    echo "selftest: FAILED" >&2
fi
exit "$status"
