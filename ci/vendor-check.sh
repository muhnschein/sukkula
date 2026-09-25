#!/bin/bash
# Prove each vendored crate is its upstream plus its patches, and nothing
# else (spec §5: "the patched copy lives in third_party/ with the patches
# kept separate").
#
#     ci/vendor-check.sh
#
# Carrying someone else's crate in-tree is only safe while everyone can see
# exactly how it differs from upstream -- and rqs_lib's differences are
# security fixes (Q1-Q7), so a stray edit or a patch that silently stopped
# applying is a hole. For every line of ci/vendor.conf: fetch upstream at
# the pinned commit, apply the patches, and require the vendored tree to
# match. Piirit's ci/vendor-check.sh is the model.
#
# The rules:
#   - a file in both trees must be byte-identical;
#   - a file only in the vendored tree was added without a patch: refused;
#   - a file only upstream was dropped. That is allowed for what is not
#     part of the crate's build -- upstream's Node bindings, examples, dist
#     artefacts, its own Cargo.lock -- and listed; a dropped Cargo.toml,
#     build.rs, or anything under src/ or proto/ changes the code, and has
#     to be a patch.
#
#   VENDOR_UPSTREAM_DIR  a local clone of the upstream to read the commit
#                        from instead of fetching it (one crate at a time;
#                        git lines only)
#
# CONTRACT: a line may name a crates.io release instead of a git commit
# (mdns-sd, vendored by the Quick Share fix round): the URL of its .crate
# file and that file's 64-hex sha256 in place of the commit -- the checksum
# Cargo.lock recorded for it -- and, as the subdirectory, the directory the
# archive unpacks to (`<name>-<version>`). The archive is taken from cargo's
# download cache ($CARGO_HOME/registry/cache/*/) when a file of that name is
# there, else fetched with curl, and used only if its sha256 is the pinned
# one: the published crate, byte for byte, is the upstream.
#
# Needs git and patch, the network unless VENDOR_UPSTREAM_DIR is set (and,
# for a crate line, unless cargo has the archive), and sha256sum for crate
# lines. Missing inputs fail: a vendored crate that is not there is not a
# pass.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
conf="$root/ci/vendor.conf"
status=0
bad() { echo "vendor-check: FAIL $*" >&2; status=1; return 0; }

for tool in git patch diff tar; do
    command -v "$tool" >/dev/null 2>&1 || { echo "vendor-check: FAIL $tool not found" >&2; exit 1; }
done
[[ -f "$conf" ]] || { echo "vendor-check: FAIL $conf is missing" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

checked=0
while read -r name url commit subdir vendored _; do
    [[ -n "${name:-}" && "$name" != '#'* ]] || continue
    checked=$((checked + 1))
    if [[ $commit =~ ^[0-9a-f]{64}$ ]]; then
        kind=crate
    elif [[ $commit =~ ^[0-9a-f]{40}$ ]]; then
        kind=git
    else
        bad "$name: '$commit' is not a full commit id (or a crate's sha256)"
        continue
    fi
    vdir="$root/$vendored"
    if [[ ! -d "$vdir" ]]; then
        bad "$name: $vendored is not there; vendor it, or drop its line from ci/vendor.conf"
        continue
    fi

    # The patches, wherever the vendoring put them.
    candidates=("$root/third_party/$name.patch" "$root/third_party/patches/$name"
                "$root/third_party/$name-patches" "$root/third_party/$name.patches")
    found=()
    for c in "${candidates[@]}"; do
        [[ -e "$c" ]] && found+=("$c")
    done
    if [[ ${#found[@]} -eq 0 ]]; then
        bad "$name: no patches at any of ${candidates[*]#"$root"/}; spec §5 requires Q1-Q7 patched"
        continue
    elif [[ ${#found[@]} -gt 1 ]]; then
        bad "$name: patches in more than one place (${found[*]#"$root"/}); keep one"
        continue
    fi
    patches=()
    if [[ -f "${found[0]}" ]]; then
        patches=("${found[0]}")
    elif [[ -f "${found[0]}/series" ]]; then
        while read -r p _; do
            [[ -n "$p" && "$p" != '#'* ]] || continue
            [[ -f "${found[0]}/$p" ]] || { bad "$name: series names $p, which is not there"; continue 2; }
            patches+=("${found[0]}/$p")
        done < "${found[0]}/series"
    else
        mapfile -t patches < <(find "${found[0]}" -maxdepth 1 -name '*.patch' | sort)
    fi
    if [[ ${#patches[@]} -eq 0 ]]; then
        bad "$name: ${found[0]#"$root"/} holds no patches"
        continue
    fi

    # Upstream at the pinned commit, only the subdirectory, exactly as git
    # has it: `git archive` rather than a checkout, so nothing of the
    # working tree or its line-ending settings leaks in.
    up="$work/$name-upstream"
    mkdir -p "$up"
    if [[ "$kind" = crate ]]; then
        # CONTRACT: a crates.io release, pinned by its sha256 (see the top).
        if [[ $subdir == */* || $subdir == .* ]]; then
            bad "$name: '$subdir' must be the one directory the crate unpacks to"
            continue
        fi
        command -v sha256sum >/dev/null 2>&1 || { bad "$name: sha256sum not found"; continue; }
        archive="$work/$name.crate"
        cached=""
        for c in "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/"${url##*/}"; do
            [[ -f "$c" ]] && cached=$c && break
        done
        if [[ -n "$cached" ]]; then
            cp "$cached" "$archive"
        elif ! command -v curl >/dev/null 2>&1; then
            bad "$name: curl not found, and cargo has no ${url##*/}"
            continue
        elif ! curl -fsSL --proto '=https,file' -o "$archive" "$url" >&2; then
            bad "$name: could not fetch $url"
            continue
        fi
        got=$(sha256sum "$archive" | cut -d' ' -f1)
        if [[ "$got" != "$commit" ]]; then
            bad "$name: ${url##*/} has sha256 $got, not the pinned $commit"
            continue
        fi
        if ! tar -xzf "$archive" -C "$up" || [[ ! -d "$up/$subdir" ]]; then
            bad "$name: ${url##*/} does not unpack to $subdir"
            continue
        fi
    else
        if [[ -n "${VENDOR_UPSTREAM_DIR:-}" ]]; then
            src_repo=$VENDOR_UPSTREAM_DIR
        else
            src_repo="$work/$name.git"
            if ! { git init -q --bare "$src_repo" &&
                   git -C "$src_repo" fetch -q --depth 1 "$url" "$commit"; } >&2; then
                bad "$name: could not fetch $url at $commit"
                continue
            fi
        fi
        if ! git -C "$src_repo" archive --format=tar "$commit" "$subdir" | tar -x -C "$up"; then
            bad "$name: $commit has no $subdir"
            continue
        fi
    fi
    base="$up/$subdir"

    # Each patch at whatever strip level makes it apply: they may be made
    # against upstream's repository root (a/core_lib/src/...), the crate
    # (a/src/...), or this tree (a/third_party/rqs_lib/src/...).
    for p in "${patches[@]}"; do
        applied=0
        for level in 1 2 3 0 4; do
            if patch -s -f --dry-run -p"$level" -d "$base" < "$p" >/dev/null 2>&1; then
                patch -s -f --no-backup-if-mismatch -p"$level" -d "$base" < "$p" >/dev/null
                applied=1
                break
            fi
        done
        [[ "$applied" = 1 ]] || { bad "$name: ${p#"$root"/} does not apply to upstream $commit"; continue 2; }
        echo "vendor-check: $name: applied ${p#"$root"/}"
    done

    # The patch directory, if it is inside the vendored tree, is not part
    # of what is compared.
    mapfile -t only_vendored < <(diff -rq "$base" "$vdir" 2>/dev/null |
        sed -n "s|^Only in $vdir\(/*\)\([^:]*\): \(.*\)|\2/\3|p" | sed 's|^/||')
    mapfile -t only_upstream < <(diff -rq "$base" "$vdir" 2>/dev/null |
        sed -n "s|^Only in $base\(/*\)\([^:]*\): \(.*\)|\2/\3|p" | sed 's|^/||')
    mapfile -t differ < <(diff -rq "$base" "$vdir" 2>/dev/null |
        sed -n "s|^Files $base/\(.*\) and $vdir/.* differ$|\1|p")

    for f in "${differ[@]}"; do
        [[ -n "$f" ]] && bad "$name: $f differs from upstream plus patches; fold the edit into a patch"
    done
    for f in "${only_vendored[@]}"; do
        [[ -n "$f" ]] && bad "$name: $f is in $vendored but not upstream plus patches; add it by a patch"
    done
    for f in "${only_upstream[@]}"; do
        [[ -n "$f" ]] || continue
        case "$f" in
            Cargo.toml|build.rs|src|src/*|proto|proto/*)
                bad "$name: $f was dropped from the vendored copy; that changes the crate, so it has to be a patch" ;;
            *)
                echo "vendor-check: $name: not vendored (outside the crate's build): $f" ;;
        esac
    done
    if [[ ${#differ[@]} -eq 0 && ${#only_vendored[@]} -eq 0 ]]; then
        echo "vendor-check: $name: $vendored is upstream $commit plus ${#patches[@]} patch(es)"
    fi
done < "$conf"

[[ "$checked" -gt 0 ]] || bad "ci/vendor.conf names no crate"
[[ "$status" -eq 0 ]] && echo "vendor-check: ok"
exit "$status"
