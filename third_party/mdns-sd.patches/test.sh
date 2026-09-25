#!/bin/bash
# The vendored mdns-sd's own unit tests: upstream's, which prove that the
# patches here did not break the daemon, and those the patches add (the
# cache caps and replacement rule, the TTL cap, the cache's deadlines, the
# local-link check, the reply limiter).
#
#     third_party/mdns-sd.patches/test.sh [cargo test arguments...]
#
# They run on a copy of third_party/mdns-sd, against the lockfile upstream
# published in the crate: the tests' own dependencies (env_logger,
# test-log, ...) are not in Sukkula's Cargo.lock, and have no business
# there. The archive is the one ci/vendor.conf pins, taken from cargo's
# download cache or crates.io and used only if its sha256 is the pinned
# one, as ci/vendor-check.sh does. Nothing is left in third_party/.
#
# Three of upstream's tests need IPv6 on the loopback interface; where the
# kernel has no IPv6 (no /proc/net/if_inet6) they are skipped, and said so.
# The daemon's end-to-end behaviour under Sukkula -- registration, S7, a
# flood -- is tested by third_party/rqs_lib/tests/mdns.rs
# (ci/rqs-lib-tests.sh) and crates/sukkula-engine/tests/quickshare_mdns.rs.
set -euo pipefail

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "$here/../.." && pwd)
fail() { echo "mdns-sd tests: FAIL $*" >&2; exit 1; }

for tool in cargo sha256sum tar awk; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool not found"
done
read -r _ url sha subdir vendored _ < <(awk '$1 == "mdns-sd"' "$root/ci/vendor.conf") ||
    fail "ci/vendor.conf has no mdns-sd line"
[[ $sha =~ ^[0-9a-f]{64}$ ]] || fail "ci/vendor.conf pins mdns-sd by '$sha', not a sha256"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
archive="$work/${url##*/}"
cached=""
for c in "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/"${url##*/}"; do
    [[ -f "$c" ]] && cached=$c && break
done
if [[ -n "$cached" ]]; then
    cp "$cached" "$archive"
else
    command -v curl >/dev/null 2>&1 || fail "curl not found, and cargo has no ${url##*/}"
    curl -fsSL --proto '=https,file' -o "$archive" "$url" || fail "could not fetch $url"
fi
[[ "$(sha256sum "$archive" | cut -d' ' -f1)" == "$sha" ]] || fail "${url##*/} is not the pinned archive"
tar -xzf "$archive" -C "$work" "$subdir/Cargo.lock" || fail "${url##*/} has no Cargo.lock"

listing() { (cd "$root/$vendored" && find . -print | LC_ALL=C sort); }
before=$(listing)
mkdir "$work/crate"
(cd "$root/$vendored" && tar -cf - --exclude=./target --exclude=./Cargo.lock .) | (cd "$work/crate" && tar -xf -)
cp "$work/$subdir/Cargo.lock" "$work/crate/Cargo.lock"

skip=()
if [[ ! -e /proc/net/if_inet6 ]]; then
    echo "mdns-sd tests: no IPv6 here; skipping upstream's three tests that need it"
    skip=(--skip test_excluded_address_preserves_announced_service --skip test_negative_hostname_answers)
fi

cd "$root"
CARGO_TARGET_DIR=${MDNS_SD_TARGET_DIR:-$root/target/mdns-sd-tests} \
    cargo test --locked --manifest-path "$work/crate/Cargo.toml" --lib "$@" -- "${skip[@]}"
[[ "$(listing)" == "$before" ]] || fail "$vendored changed while its tests ran"
echo "mdns-sd tests: ok"
