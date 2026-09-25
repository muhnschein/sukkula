#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Saves the committed work of every agent worktree as patches under
# docs/handoff/wip/<area>/, so work in flight survives the container being
# reclaimed. Worktrees and their branches are local; only what is pushed on
# the integration branch lasts. docs/HANDOFF.md says how to replay them.
#
# Run from the repository root, then commit docs/handoff/wip and push.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
base_branch=claude/busy-babbage-7y67po

# Worktree branch -> area, for the fix round of 2026-09-25 (docs/HANDOFF.md).
area_of() {
    case "$1" in
        *a602b544075aa35fa) echo quickshare-mdns ;;
        *a506480e435f5cd36) echo localsend ;;
        *abdff3a097115f0b3) echo engine-core ;;
        *aa31243a61787cb91) echo wormhole-bluetooth-merged ;;
        *a1100ec55d3d67d57) echo qml-ui ;;
        *af1887251db015b9e) echo ci-harbour ;;
        *) basename "$1" ;;
    esac
}

found=0
while read -r path branch; do
    [[ "$branch" == worktree-agent-* ]] || continue
    found=1
    area=$(area_of "$branch")
    out="docs/handoff/wip/$area"
    rm -rf "$out"
    mkdir -p "$out"
    since=$(git merge-base "$base_branch" "$branch")
    count=$(git rev-list --count "$since..$branch")
    {
        echo "branch=$branch"
        echo "base=$since"
        echo "commits=$count"
        echo "uncommitted=$(git -C "$path" status --porcelain | wc -l)"
    } > "$out/STATE"
    if [ "$count" -gt 0 ]; then
        git format-patch --quiet -o "$out" "$since..$branch"
    fi
    echo "handoff: $area: $count commit(s) from $branch"
done < <(git worktree list --porcelain | awk '/^worktree /{p=$2} /^branch /{sub("refs/heads/","",$2); print p, $2}')

[ "$found" -eq 1 ] || echo "handoff: no agent worktrees"
