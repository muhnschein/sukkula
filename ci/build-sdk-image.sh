#!/bin/bash
# Derive the slim, pre-provisioned SDK image .github/workflows/rpm.yml
# builds with, from upstream's platform SDK.
#
#     ci/build-sdk-image.sh <sfos-version> aarch64 <output-image>
#
# Upstream's image carries three architectures' target rootfs -- about
# 5 GB to pull, of which a build uses a third -- and leaves the spec's
# BuildRequires to be zypper-installed into the target on every build.
# What comes out of here carries the one architecture Sukkula builds, with
# those packages already in its target: roughly 2 GB, and no package
# installs on the build's critical path. That target is also the sysroot
# scripts/cross-build-rust.sh compiles the engine against, which is why
# libdbus-1's headers (a BuildRequires) have to be baked into it. Adapted
# from piirit's, less the rustlib provisioning: the SDK's Rust is never
# used here (spec §3).
#
# Run by .github/workflows/sdk-image.yml, and by scripts/build-rpm.sh when
# the image it wants is not published yet. The image's tag carries a hash
# of this script and the spec's BuildRequires (scripts/build-rpm.sh image),
# so a change to either derives a new image rather than replacing one.
set -euo pipefail

usage() {
    echo "usage: $0 <sfos-version> aarch64 <output-image>" >&2
    exit 2
}

[[ $# -eq 3 ]] || usage
sfos=$1
arch=$2
output=$3

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
spec="$root/rpm/harbour-sukkula.spec"

[[ $sfos =~ ^[0-9]+(\.[0-9]+){3}$ ]] ||
    { echo "build-sdk-image: '$sfos' is not a dotted SDK version" >&2; exit 1; }
# The Jolla Phone 2026 and nothing else (spec §2).
[[ "$arch" = aarch64 ]] ||
    { echo "build-sdk-image: Sukkula builds aarch64, not '$arch'" >&2; exit 1; }

# By digest, not tag. The SDK image is a third party's, it is run
# privileged with a checkout mounted read-write, and a tag can be repointed
# at any content by whoever owns it. Only 5.2 and later: Harbour requires
# __libc_start_main@GLIBC_2.34, and 5.2 is the Jolla Phone 2026's baseline
# (docs/HARBOUR.md). To add a version: pull the tag once, read the digest
# with `docker inspect --format '{{index .RepoDigests 0}}'`, add a line.
case "$sfos" in
    5.2.0.15) digest=sha256:e7c4379393b17ee63d7f7443ce9930c315a61fdb2e732016346ccbe8c934435e ;;
    *) echo "build-sdk-image: no pinned digest for coderus/sailfishos-platform-sdk:$sfos;" \
            "add one to $0" >&2
       exit 1 ;;
esac

# The digest is what pins the content, so any registry serving that digest
# serves the same bytes -- which is what makes a mirror safe, and why one
# is settable at all: Docker Hub rate-limits anonymous pulls.
upstream="${SDK_UPSTREAM_REPO:-coderus/sailfishos-platform-sdk}@$digest"
target="SailfishOS-$sfos-$arch"

echo ">> deriving $output"
echo "   from $upstream"
echo "   target $target"

if docker image inspect "$upstream" >/dev/null 2>&1; then
    echo ">> $upstream is already here"
else
    docker pull "$upstream"
fi

cid=$(docker run -d --privileged "$upstream" sleep infinity)
cleanup() { docker rm -f "$cid" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Copied in rather than bind-mounted: a mount leaves its mount point behind
# in the exported filesystem, and the spec is the only thing from this tree
# the bake reads.
docker cp "$spec" "$cid:/tmp/harbour-sukkula.spec"

# The bake, in two halves, because the two need different users and this
# image grants no passwordless sudo. mb2 has to run as the image's own
# mersdk (sdk-manage refuses root); everything that writes outside that
# user's home has to be root, which `docker exec --user root` grants.
echo ">> installing what the spec needs, as the build user"
docker exec -e TARGET="$target" "$cid" bash -euxo pipefail -c '
    # A build directory named for the package: mb2 derives the package it
    # is building from the directory it runs in.
    mkdir -p ~/harbour-sukkula/rpm
    cp /tmp/harbour-sukkula.spec ~/harbour-sukkula/rpm/
    cd ~/harbour-sukkula

    # -X (--no-fix-version) for the reason rpm.yml passes it: without it
    # build-init asks git describe for a version, finds no tags, and stops
    # before writing .mb2/spec.
    mb2 -t "$TARGET" -X build-init
    mb2 -t "$TARGET" -X build-requires

    # The image ships with a default target this bake removes, and a
    # default naming a target that is not there fails every bare sb2 call.
    # Rewritten in place, as the build user, so the file stays theirs.
    sed -i "s|^DEFAULT_TARGET=.*|DEFAULT_TARGET=$TARGET|" "$HOME/.scratchbox2/config"
    grep "^DEFAULT_TARGET=$TARGET$" "$HOME/.scratchbox2/config"

    rm -rf ~/harbour-sukkula
'

echo ">> dropping every other architecture"
docker exec --user root -e TARGET="$target" "$cid" bash -euxo pipefail -c '
    # Each target comes in two rootfs: the pristine one, and the
    # "<target>.default" snapshot that mb2 actually builds in -- which is
    # where build-requires put everything above. Both of ours stay.
    for dir in /srv/mer/targets/*/; do
        name=$(basename "$dir")
        [[ "$name" = "$TARGET" || "$name" = "$TARGET.default" ]] && continue
        rm -rf "$dir" "/home/mersdk/.scratchbox2/$name"
    done
    rm -f /tmp/harbour-sukkula.spec
    rm -rf "/srv/mer/targets/$TARGET/var/cache/zypp"/* \
           "/srv/mer/targets/$TARGET.default/var/cache/zypp"/*
'

# `docker export` writes the container's filesystem as it stands, so the
# deletions above are deletions rather than whiteouts over layers that
# still weigh what they weighed. That is the whole reason for the
# export/import rather than a Dockerfile `RUN rm`. The config a Dockerfile
# would have carried is restored by hand; these are upstream's own.
echo ">> flattening"
docker stop "$cid" >/dev/null

# GHCR links a published package to a repository by this label, and a
# package it has not linked is one that repository's own token is not
# granted to pull back.
source_url=""
if [[ -n "${GITHUB_REPOSITORY:-}" ]]; then
    source_url="${GITHUB_SERVER_URL:-https://github.com}/$GITHUB_REPOSITORY"
elif origin=$(git -C "$root" remote get-url origin 2>/dev/null); then
    source_url=${origin%.git}
fi

changes=(
    --change 'ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin'
    --change 'USER mersdk'
    --change 'WORKDIR /home/mersdk'
    --change "LABEL org.opencontainers.image.base.digest=$digest"
)
if [[ -n "$source_url" ]]; then
    changes+=(--change "LABEL org.opencontainers.image.source=$source_url")
fi

docker export "$cid" |
    docker import "${changes[@]}" \
        --message "sukkula: $target, BuildRequires installed, one architecture" \
        - "$output" >/dev/null

# A gate, not a report: an image that cannot answer these is one that fails
# minutes into a build instead, with an error about something else.
echo ">> checking what came out"
buildrequires=$(sed -n 's/^BuildRequires:[[:space:]]*\([^[:space:]]*\).*/\1/p' "$spec" | tr '\n' ' ')
docker run --rm --privileged -e TARGET="$target" -e BUILDREQUIRES="$buildrequires" "$output" \
    bash -euo pipefail -c '
    # Read once into a variable and matched from there, rather than piped
    # into `grep -q`: grep stops at its first match, sb2-config dies of
    # SIGPIPE writing the rest, and under pipefail that reads as failure.
    targets=$(sb2-config -l)
    printf "%s\n" "$targets"
    [ "$(printf "%s\n" "$targets" | grep -c .)" = 2 ] ||
        { echo "the wrong number of targets survived" >&2; exit 1; }
    grep -qx "$TARGET" <<< "$targets"
    grep -qx "$TARGET.default" <<< "$targets"

    # Every BuildRequires, against the snapshot mb2 builds in -- the
    # pristine target beside it has none of them.
    for req in $BUILDREQUIRES; do
        sb2 -t "$TARGET.default" rpm -q --whatprovides "$req" >/dev/null ||
            { echo "the snapshot lacks $req" >&2; exit 1; }
        echo "has $req"
    done

    # What scripts/cross-build-rust.sh lifts out: the cross compiler, and
    # a sysroot that libdbus-sys can find libdbus-1 in.
    test -x /opt/cross/bin/aarch64-meego-linux-gnu-gcc
    /opt/cross/bin/aarch64-meego-linux-gnu-gcc --version | head -1
    test -f "/srv/mer/targets/$TARGET.default/usr/lib64/pkgconfig/dbus-1.pc"
    grep "^DEFAULT_TARGET=$TARGET$" "$HOME/.scratchbox2/config"

    # And that a build directory still initialises against the slimmed
    # target, which is the next thing rpm.yml does.
    mkdir -p ~/verify/rpm
    printf "Name: verify\nSummary: s\nVersion: 0\nRelease: 1\nLicense: GPL-3.0-or-later\n%%description\ns\n%%build\n%%install\n%%files\n" \
        > ~/verify/rpm/verify.spec
    cd ~/verify && mb2 -t "$TARGET" -X build-init
'

size=$(docker image inspect --format '{{.Size}}' "$output")
echo ">> $output is $((size / 1000 / 1000)) MB unpacked"
