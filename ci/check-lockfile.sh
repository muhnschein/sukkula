#!/bin/bash
# What spec §6 says about the lockfile, checked: it is committed and current,
# every git dependency is pinned to a commit, and nothing resolves outside
# this repository or crates.io.
#
#     ci/check-lockfile.sh [lockfile...]
#
# Defaults to Cargo.lock, plus fuzz/Cargo.lock when the fuzz crate commits
# one. cargo-deny's `sources` check already refuses unknown registries and
# git hosts; this adds what it does not look at:
#
#   - `?rev=` on every git source. A `branch =` or bare git dependency is
#     pinned only by whatever commit the lockfile happened to record, and
#     the next `cargo update` moves it -- the build is then something
#     nobody reviewed. `rev` makes the manifest itself say which commit.
#   - that commit and the one the lockfile resolved are the same.
#   - no path dependency that leaves the repository: it would build here
#     and nowhere else, and never in the offline SDK route.
#   - that the lockfile is current (`cargo metadata --locked`), so the
#     committed one is what builds.
#
# The Sailfish SDK's cargo never reads this file (the engine is built by
# the pinned toolchain, docs/BUILDING.md), so unlike piirit's and vuo's
# checks nothing here holds it to an older lockfile format.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
status=0
bad() { echo "check-lockfile: FAIL $*" >&2; status=1; return 0; }

locks=("$@")
if [[ ${#locks[@]} -eq 0 ]]; then
    locks=("$root/Cargo.lock")
    [[ -f "$root/fuzz/Cargo.lock" ]] && locks+=("$root/fuzz/Cargo.lock")
fi

for lock in "${locks[@]}"; do
    rel=${lock#"$root"/}
    if [[ ! -f "$lock" ]]; then
        bad "$rel is missing; the lockfile is committed (spec §6)"
        continue
    fi

    version=$(sed -n 's/^version = \([0-9]\{1,\}\)$/\1/p' "$lock" | head -1)
    [[ -n "$version" ]] || bad "$rel has no lockfile format version"

    gits=0
    while IFS= read -r source; do
        [[ -n "$source" ]] || continue
        gits=$((gits + 1))
        url=${source#git+}
        resolved=${url##*#}
        query=""
        [[ "$url" == *\?* ]] && query=${url#*\?} && query=${query%%#*}
        case "$query" in
            rev=*)
                rev=${query#rev=}
                if [[ ! $rev =~ ^[0-9a-f]{40}$ ]]; then
                    bad "$rel: $url is pinned to '$rev'; write the full 40-hex commit id"
                elif [[ "$rev" != "$resolved" ]]; then
                    bad "$rel: $url resolves to $resolved, not the pinned $rev"
                fi
                ;;
            *)
                bad "$rel: $url is not pinned by rev (${query:-no ref at all}); spec §6 pins every git dependency to a commit"
                ;;
        esac
    done < <(sed -n 's/^source = "\(git+[^"]*\)"$/\1/p' "$lock" | sort -u)

    while IFS= read -r source; do
        [[ -n "$source" ]] || continue
        case "$source" in
            git+*|"registry+https://github.com/rust-lang/crates.io-index") ;;
            *) bad "$rel: source '$source' is neither crates.io nor a pinned git repository" ;;
        esac
    done < <(sed -n 's/^source = "\(.*\)"$/\1/p' "$lock" | sort -u)

    [[ "$status" -eq 0 ]] && echo "check-lockfile: $rel -- format v$version, $gits git source(s), all pinned"
done

# Current, and nothing outside the tree. Needs cargo, and the registry
# index when the local copy of it is cold.
if command -v cargo >/dev/null 2>&1; then
    if ! meta=$(cd "$root" && cargo metadata --locked --format-version 1 2>&1); then
        echo "$meta" | tail -5 >&2
        bad "Cargo.lock is not current: a manifest changed without it (cargo metadata --locked)"
    elif command -v python3 >/dev/null 2>&1; then
        outside=$(python3 -c '
import json, os, sys
root = os.path.realpath(sys.argv[1])
meta = json.loads(sys.stdin.read())
for p in meta["packages"]:
    if p.get("source") is None:
        path = os.path.realpath(p["manifest_path"])
        if os.path.commonpath([root, path]) != root:
            print(p["name"], path)
' "$root" <<< "$meta")
        if [[ -n "$outside" ]]; then
            while IFS= read -r line; do
                bad "path dependency outside the repository: $line"
            done <<< "$outside"
        else
            echo "check-lockfile: Cargo.lock is current; every path dependency is inside the tree"
        fi
    else
        bad "python3 is needed to read cargo metadata"
    fi
else
    bad "cargo not found; the lockfile's currency cannot be checked"
fi

[[ "$status" -eq 0 ]] && echo "check-lockfile: ok"
exit "$status"
