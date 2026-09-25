#!/usr/bin/env bash
# Build the device RPM the way every Sukkula package is built (spec §3):
# the Rust engine cross-compiled by the pinned toolchain with the SDK's own
# aarch64 GCC and Sailfish OS 5.2 sysroot, then the Qt shell built, linked
# and packaged by mb2 inside the SDK.
#
#     scripts/build-rpm.sh image                 print the SDK image this tree wants
#     scripts/build-rpm.sh pull <image>          pull it by the digest ci/sdk-image.digests
#                                                pins; fails when none is pinned or served
#     scripts/build-rpm.sh lift <image> <dir>    copy the cross toolchain and the
#                                                target sysroot out of the image
#     scripts/build-rpm.sh engine <dir>          cross-build the engine against them
#     scripts/build-rpm.sh package <image>       mb2-build the RPM, engine linked
#     scripts/build-rpm.sh check <rpm> <dir>     the binary rules (ci/check-elf.sh)
#                                                and Jolla's validator on the result
#     scripts/build-rpm.sh all [<dir>]           all of it, in order (`make rpm`)
#
# .github/workflows/rpm.yml runs the same steps one by one, for their logs
# and its caches; `make rpm` runs `all`. Needs Docker, and sudo for the two
# things the SDK image insists on: files owned by its build user, and the
# cross toolchain at the absolute path /opt/cross. docs/BUILDING.md has the
# why of each step.
#
#   SFOS          SDK version, default 5.2.0.15 (the Jolla Phone 2026's)
#   SDK_REGISTRY  where the derived image lives, default ghcr.io/<owner>
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SFOS="${SFOS:-5.2.0.15}"
ARCH=aarch64
TARGET="SailfishOS-$SFOS-$ARCH"
NAME=harbour-sukkula
SPEC="rpm/$NAME.spec"
DIGESTS="ci/sdk-image.digests"

fail() { echo "build-rpm: FAIL $*" >&2; exit 1; }
[[ $SFOS =~ ^[0-9]+(\.[0-9]+){3}$ ]] || fail "SFOS '$SFOS' is not a dotted SDK version"

# The derived image's name carries a hash of everything that goes into it
# -- the script that derives it and the spec's BuildRequires -- so a
# change to either wants an image of its own. The name is only a name: a
# tag can be repointed by anyone who can push to the registry, so nothing
# here pulls by it (pull, below).
image_name() {
    local registry=${SDK_REGISTRY:-}
    if [[ -z "$registry" ]]; then
        local owner=${GITHUB_REPOSITORY_OWNER:-}
        if [[ -z "$owner" ]]; then
            owner=$(git -C "$ROOT" remote get-url origin 2>/dev/null |
                sed -n 's#.*github.com[:/]\([^/]*\)/.*#\1#p')
        fi
        registry="ghcr.io/$(tr '[:upper:]' '[:lower:]' <<< "${owner:-local}")"
    fi
    local inputs
    [[ -f ci/build-sdk-image.sh && -f "$SPEC" ]] || fail "ci/build-sdk-image.sh or $SPEC is missing"
    inputs=$( { cat ci/build-sdk-image.sh; grep '^BuildRequires:' "$SPEC" | sort; } | sha256sum | cut -c1-12)
    echo "$registry/sukkula-sdk:$SFOS-$ARCH-$inputs"
}

# The image, by the digest the reviewed tree pins for its tag in
# ci/sdk-image.digests -- the derived image's twin of the upstream digest
# ci/build-sdk-image.sh pins. `docker pull repo@digest` refuses content
# that does not hash to the digest, so what is pulled is what was pinned,
# whatever the tag points at today; it is then tagged with its name on
# this machine only. Fails, for the caller to derive the image instead,
# when the tag has no pin or the registry does not serve that digest (a
# fork's registry, say): deriving trusts no registry at all.
pull() {
    local image=$1 tag repo digest
    tag=${image##*:}
    repo=${image%:*}
    [[ -f "$DIGESTS" ]] || fail "$DIGESTS is missing"
    digest=$(awk -v t="$tag" '$1 == t { print $2 }' "$DIGESTS" | tail -1)
    if [[ -z "$digest" ]]; then
        echo "build-rpm: no digest pinned for $tag in ci/sdk-image.digests" >&2
        return 1
    fi
    [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || fail "ci/sdk-image.digests pins '$digest' for $tag, which is not a sha256 digest"
    if ! docker pull -q "$repo@$digest"; then
        echo "build-rpm: $repo does not serve $digest" >&2
        return 1
    fi
    docker tag "$repo@$digest" "$image"
    # Belt and braces: the digest the image is known by is the pinned one.
    docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$image" |
        grep -qxF "$repo@$digest" || fail "$image is not $repo@$digest after the pull"
    echo "using $image, pinned at $digest"
}

# What the engine needs from the image, and nothing else:
#
#   /opt/cross               the SDK's aarch64-meego-linux-gnu GCC 10, which
#                            must match the phone's glibc and libstdc++
#   the target sysroot       glibc, libdbus-1 and their headers; the
#                            ".default" snapshot where the image has one,
#                            since that is where it baked the BuildRequires
#   cc1's own libraries      libmpc, libmpfr, libgmp: the compilers are
#                            32-bit programs linking libraries that exist only
#                            inside the SDK
#
# Copied out with `docker cp` rather than exporting the whole image, as
# vuo's rpm.yml does.
lift() {
    local image=$1 dir=$2
    mkdir -p "$dir"
    dir=$(cd "$dir" && pwd)
    local targets sysroot
    targets=$(docker run --rm "$image" sb2-config -l 2>/dev/null || true)
    echo "targets in $image:"; echo "$targets"
    if grep -qx -- "$TARGET.default" <<< "$targets"; then
        sysroot="$TARGET.default"
    elif grep -qx -- "$TARGET" <<< "$targets"; then
        sysroot="$TARGET"
    else
        fail "no $TARGET in $image; see the list above"
    fi

    local c
    # The image declares no default command, and `docker create` refuses
    # without one; the container never starts, so any command will do.
    c=$(docker create "$image" true)
    mkdir -p "$dir/opt" "$dir/sysroot" "$dir/usr/lib"
    # `docker cp` out of a container extracts in the client, which as a
    # normal user cannot restore root ownership; sudo, then chown back.
    sudo docker cp "$c:/opt/cross" "$dir/opt/cross"
    sudo docker cp "$c:/srv/mer/targets/$sysroot/." "$dir/sysroot/"
    docker rm "$c" >/dev/null

    # The libraries the cross compiler's programs load, as the image's own
    # ldd lists them, dereferenced -- those under /usr/lib only, as vuo's
    # rpm.yml takes them. /lib is the SDK's glibc and its siblings: the
    # host's i386 libc serves, and a foreign one on LD_LIBRARY_PATH would
    # break every 32-bit program the compiler starts.
    docker run --rm --user root -v "$dir/usr/lib:/out" "$image" sh -c '
        set -e
        libexec=/opt/cross/libexec/gcc/aarch64-meego-linux-gnu
        for b in $libexec/*/cc1 $libexec/*/cc1plus $libexec/*/collect2 $libexec/*/lto1 \
                 $libexec/*/lto-wrapper /opt/cross/bin/aarch64-meego-linux-gnu-*; do
            [ -x "$b" ] || continue
            ldd "$b" 2>/dev/null | awk "/=> \\//{print \$3}"
        done | sort -u | while read -r lib; do
            case "$lib" in
                /usr/lib/*) cp -L "$lib" /out/ ;;
            esac
        done
        ls /out/libmpc.so* /out/libmpfr.so* /out/libgmp.so* >/dev/null'
    sudo chown -R "$(id -u):$(id -g)" "$dir"

    # GCC resolves cc1 and its specs against its absolute install prefix.
    if [[ "$(readlink -f /opt/cross 2>/dev/null)" != "$dir/opt/cross" ]]; then
        sudo ln -sfn "$dir/opt/cross" /opt/cross
    fi
    echo "lifted $sysroot and /opt/cross into $dir"
    du -sh "$dir"/opt/cross "$dir"/sysroot "$dir"/usr/lib
}

engine() {
    local dir
    dir=$(cd "$1" && pwd)
    SFOS_CROSS=/opt/cross SFOS_CROSS_LIBS="$dir/usr/lib" \
        "$ROOT/scripts/cross-build-rust.sh" --sdk "$dir/sysroot"
}

# mb2 inside the image, as its own build user: sdk-manage refuses root
# ("Cannot determine Mer SDK user"), so the tree is handed to that user and
# taken back afterwards. Mounted inside the user's home, because rpm runs
# under scratchbox2, which redirects absolute paths it does not recognise
# into the target rootfs: with the tree anywhere else mb2 writes .mb2/spec
# outside and rpm looks for it inside (piirit's docs/BUILDING.md). The
# directory keeps the package's name, which is how mb2 finds the spec.
#
# `-X` (--no-fix-version) because without it build-init asks git describe
# for a version, finds no tags, and stops before writing the spec.
package() {
    local image=$1 uid gid home builddir
    [[ -f "target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.so" ]] ||
        fail "no engine; run '$0 engine <dir>' first"
    uid=$(docker run --rm "$image" id -u)
    gid=$(docker run --rm "$image" id -g)
    home=$(docker run --rm "$image" sh -c 'echo $HOME')
    builddir="$home/$NAME"
    echo "the SDK builds as uid=$uid gid=$gid in $builddir"
    sudo chown -R "$uid:$gid" "$ROOT"
    local rc=0
    # The script is single-quoted, so nothing from the image is spliced
    # into shell text; TARGET goes in through the environment.
    docker run --rm --privileged -e TARGET="$TARGET" \
        -v "$ROOT:$builddir" -w "$builddir" "$image" \
        /bin/bash -c 'set -e
            mb2 -t "$TARGET" -X build-init
            mb2 -t "$TARGET" -X build-requires ||
                echo "::warning::mb2 build-requires failed; the SDK image may be behind the spec"
            mb2 -t "$TARGET" -X build --no-check' || rc=$?
    sudo chown -R "$(id -u):$(id -g)" "$ROOT"
    [[ "$rc" -eq 0 ]] || { ls -la .mb2 2>/dev/null || true; fail "mb2 build failed ($rc)"; }
    find RPMS -name "$NAME-*.rpm" -print
}

# The package, judged: the binary rules first (they say *why* a link is
# wrong), then Jolla's validator, which decides.
check() {
    local rpm=$1 dir=$2 unpack ceiling="" abs
    [[ -f "$rpm" ]] || fail "no such RPM: $rpm"
    # Made absolute here, not in the subshell below: after its cd, a
    # relative path such as rpm.yml's RPMS/... no longer names the file.
    abs="$(cd "$(dirname "$rpm")" && pwd)/$(basename "$rpm")"
    unpack=$(mktemp -d)
    (cd "$unpack" && rpm2cpio "$abs" | cpio -idm --quiet)
    if [[ -f "$dir/sysroot/usr/include/features.h" ]]; then
        local major minor
        major=$(sed -n 's/^#define[[:space:]]\{1,\}__GLIBC__[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' "$dir/sysroot/usr/include/features.h")
        minor=$(sed -n 's/^#define[[:space:]]\{1,\}__GLIBC_MINOR__[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p' "$dir/sysroot/usr/include/features.h")
        ceiling="--glibc-ceiling $major.$minor"
    fi
    local status=0
    # The binary, which finds the engine's library through its one RPATH,
    # and the library, which exports the C ABI and nothing else.
    # shellcheck disable=SC2086 # $ceiling is one option and its value, or nothing
    "$ROOT/ci/check-elf.sh" --main-export --only-main --stripped --libc-start-main 2.34 $ceiling \
        --rpath "/usr/share/$NAME/lib" "$unpack/usr/bin/$NAME" || status=1
    # shellcheck disable=SC2086
    "$ROOT/ci/check-elf.sh" --library --stripped $ceiling \
        --exports-only "sukkula_start sukkula_command sukkula_stop sukkula_version" \
        "$unpack/usr/share/$NAME/lib/libsukkula_ffi.so" || status=1
    rm -rf "$unpack"
    "$ROOT/ci/harbour-validate-rpm.sh" "$rpm" || status=1
    return "$status"
}

case "${1:-}" in
    image) image_name ;;
    pull) [[ $# -eq 2 ]] || fail "usage: $0 pull <image>"; pull "$2" ;;
    lift) [[ $# -eq 3 ]] || fail "usage: $0 lift <image> <dir>"; lift "$2" "$3" ;;
    engine) [[ $# -eq 2 ]] || fail "usage: $0 engine <dir>"; engine "$2" ;;
    package) [[ $# -eq 2 ]] || fail "usage: $0 package <image>"; package "$2" ;;
    check) [[ $# -eq 3 ]] || fail "usage: $0 check <rpm> <dir>"; check "$2" "$3" ;;
    all)
        dir=${2:-$HOME/.cache/sukkula-sdk/$TARGET}
        image=$(image_name)
        # One already on this machine is one this machine derived or
        # pulled by digest; otherwise the pinned one, or derive it.
        if ! docker image inspect "$image" >/dev/null 2>&1 && ! pull "$image"; then
            echo "no pinned $image to pull; deriving it (about as long as a build)"
            "$ROOT/ci/build-sdk-image.sh" "$SFOS" "$ARCH" "$image"
        fi
        [[ -x "$dir/opt/cross/bin/aarch64-meego-linux-gnu-gcc" ]] || lift "$image" "$dir"
        engine "$dir"
        package "$image"
        rpm=$(find RPMS -name "$NAME-*.$ARCH.rpm" ! -name '*debug*' | sort | tail -1)
        check "$rpm" "$dir"
        ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
