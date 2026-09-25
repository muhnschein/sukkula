#!/usr/bin/env bash
# Install Ubuntu packages on a CI runner, without letting somebody else's
# apt repository take the build down with it.
#
# `apt-get update` exits 100 when ANY configured repository fails, and the
# runner image ships several this project never installs from -- so one of
# them serving a bad index kills every job before a single test runs, with
# nothing in this repository having changed.
#
# So the third-party lists go before the update. Ubuntu's own are kept
# wherever the image puts them -- /etc/apt/sources.list, or a .list or
# .sources file under sources.list.d -- because a list is dropped only when
# nothing in it names an ubuntu.com host. That is what stops this from
# deleting the archive it is about to install from, on an image that moves
# the archive into sources.list.d, which Ubuntu's own installer now does.
#
# Usage:  ci/apt-install.sh <package>...
#         ci/apt-install.sh --prune-only     (drop the lists, install nothing)
#
# APT_SOURCES_DIR overrides which directory is pruned. It exists so
# ci/apt-install-selftest.sh can prove the rule on a directory of its own
# instead of on the machine's.
set -euo pipefail

sources_dir=$(printenv APT_SOURCES_DIR || true)
if [[ -z "$sources_dir" ]]; then
    sources_dir=/etc/apt/sources.list.d
fi

prune_only=0
if [[ "$#" -ge 1 ]] && [[ "$1" = "--prune-only" ]]; then
    prune_only=1
    shift
fi

# rm as the owner where that works, and through sudo where it does not.
# The selftest runs this against a directory of its own, and asking it for
# a password would be a poor way to run a test.
drop() {
    local list=$1
    if [[ -w "$(dirname -- "$list")" ]]; then
        rm -f "$list"
    else
        sudo rm -f "$list"
    fi
}

if [[ -d "$sources_dir" ]]; then
    for list in "$sources_dir"/*; do
        [[ -f "$list" ]] || continue
        case "$list" in
            *.list|*.sources) ;;
            *) continue ;;
        esac
        if grep -qE '://[^[:space:]]*\.ubuntu\.com' "$list"; then
            echo "apt-install: keeping $(basename -- "$list")"
            continue
        fi
        echo "apt-install: dropping $(basename -- "$list")"
        drop "$list"
    done
fi

if [[ "$prune_only" -eq 1 ]]; then
    exit 0
fi

if [[ "$#" -eq 0 ]]; then
    echo "apt-install: no packages named" >&2
    exit 1
fi

sudo apt-get update
sudo apt-get install -y "$@"
