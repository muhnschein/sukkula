#!/bin/bash
# The vendored Quick Share library's own tests (third_party/rqs_lib: its
# unit tests, including the receiver's checks in inbound_tests.rs, and
# tests/loopback.rs), on exactly the dependency versions Sukkula ships.
#
#     ci/rqs-lib-tests.sh [cargo test arguments...]
#
# rqs_lib is a workspace of its own (the root workspace excludes
# third_party/), so `cargo test --manifest-path third_party/rqs_lib/...`
# would resolve a lockfile of its own -- whatever crates.io has today, not
# what the engine is built with -- and leave that Cargo.lock and a target/
# inside the vendored tree, where ci/vendor-check.sh then finds files
# upstream does not have and fails. So the crate is tested from a copy:
#
#   1. third_party/rqs_lib is copied to a temporary directory, without any
#      target/ or Cargo.lock a local run may have left there, and beside it
#      the vendored crates it depends on by path (CONTRACT: mdns-sd, added
#      by the Quick Share fix round; `../mdns-sd` from rqs_lib);
#   2. the root Cargo.lock goes next to it, and cargo prunes it to
#      rqs_lib's graph. Every package that remains must be in the root
#      lock at the same version and checksum -- the resolution may drop
#      packages, never add or move one -- so the tests build what ships;
#   3. `cargo test --locked` runs there, with a target directory of its own
#      (RQS_LIB_TARGET_DIR, default target/rqs-lib-tests: under the
#      gitignored target/, so CI's cache keeps it and nothing lands in
#      third_party/).
#
# cargo runs from the repository root, so rust-toolchain.toml pins the
# toolchain as for everything else. Afterwards third_party/ is exactly as
# it was; the script checks that too.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
src="$root/third_party/rqs_lib"
# CONTRACT: the vendored crates rqs_lib names by path, copied alongside.
siblings=(mdns-sd)
target_dir=${RQS_LIB_TARGET_DIR:-$root/target/rqs-lib-tests}

fail() { echo "rqs-lib-tests: FAIL $*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || fail "cargo not found"
[[ -f "$src/Cargo.toml" ]] || fail "third_party/rqs_lib/Cargo.toml is missing"
[[ -f "$root/Cargo.lock" ]] || fail "Cargo.lock is missing"

# What third_party/ holds before, to prove it is untouched after.
listing() { (cd "$root/third_party" && find . -print | LC_ALL=C sort); }
before=$(listing)

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
crate="$work/rqs_lib"
mkdir "$crate"
(cd "$src" && tar -cf - --exclude=./target --exclude=./Cargo.lock .) | (cd "$crate" && tar -xf -)
for sibling in "${siblings[@]}"; do
    [[ -f "$root/third_party/$sibling/Cargo.toml" ]] || fail "third_party/$sibling/Cargo.toml is missing"
    mkdir "$work/$sibling"
    (cd "$root/third_party/$sibling" && tar -cf - --exclude=./target --exclude=./Cargo.lock .) |
        (cd "$work/$sibling" && tar -xf -)
done
cp "$root/Cargo.lock" "$crate/Cargo.lock"

cd "$root"
# Prunes the copied lock to rqs_lib's own graph. Without --locked: dropping
# the engine's packages is a change to the file, and the point.
cargo metadata --format-version 1 --manifest-path "$crate/Cargo.toml" >/dev/null ||
    fail "cargo could not resolve rqs_lib against the root Cargo.lock"

# name, version, source and checksum of every package in a lockfile.
packages() {
    local lockfile=$1
    awk '
        /^\[\[package\]\]/ { if (n != "") print n, v, s, c; n = v = s = c = ""; next }
        /^name = /     { n = $3 }
        /^version = /  { v = $3 }
        /^source = /   { s = $3 }
        /^checksum = / { c = $3 }
        END { if (n != "") print n, v, s, c }
    ' "$lockfile" | LC_ALL=C sort
}
extra=$(LC_ALL=C comm -23 <(packages "$crate/Cargo.lock") <(packages "$root/Cargo.lock"))
if [[ -n "$extra" ]]; then
    echo "$extra" >&2
    fail "rqs_lib resolves packages that Cargo.lock does not ship (above); update Cargo.lock"
fi
echo "rqs-lib-tests: $(packages "$crate/Cargo.lock" | wc -l) packages, all as Cargo.lock ships them"

CARGO_TARGET_DIR="$target_dir" cargo test --locked --manifest-path "$crate/Cargo.toml" "$@"

[[ "$(listing)" == "$before" ]] || fail "third_party/ changed while rqs_lib's tests ran"
echo "rqs-lib-tests: ok"
