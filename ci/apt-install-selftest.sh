#!/usr/bin/env bash
# Prove that ci/apt-install.sh drops the right apt lists and keeps the
# rest.
#
# The rule it applies has one way of being catastrophically wrong: delete
# the Ubuntu archive along with the third-party repositories, and every job
# fails to install anything, which is the failure it exists to prevent.
# Runner images differ in where they put the archive -- /etc/apt/sources.list
# on some, a .sources file under sources.list.d on others -- so the cases
# below cover both, and each one is a file this has to get right on some
# image somebody will run it on.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
script="$root/ci/apt-install.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [[ ! -x "$script" ]]; then
    echo "selftest: FAIL $script is missing or not executable" >&2
    exit 1
fi

status=0
cases=0

stage() {
    rm -rf "$work/sources"
    mkdir -p "$work/sources"

    # What GitHub's image ships beside Ubuntu's own, in the one-line format.
    cat >"$work/sources/google-chrome.list" <<'EOF'
deb [arch=amd64] https://dl.google.com/linux/chrome/deb/ stable main
EOF
    cat >"$work/sources/microsoft-prod.list" <<'EOF'
deb [arch=amd64] https://packages.microsoft.com/ubuntu/22.04/prod jammy main
EOF
    # Ubuntu's archive in the deb822 format, which is where a current
    # image puts it. Dropping this one is the mistake that matters.
    cat >"$work/sources/ubuntu.sources" <<'EOF'
Types: deb
URIs: http://azure.archive.ubuntu.com/ubuntu/
Suites: noble noble-updates noble-backports
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
    # And in the one-line format, which is where an older one puts it.
    cat >"$work/sources/archive.list" <<'EOF'
deb http://archive.ubuntu.com/ubuntu jammy main
EOF
    # Not a source list at all. apt ignores anything without the two
    # extensions, and so must this.
    cat >"$work/sources/google-chrome.list.save" <<'EOF'
deb [arch=amd64] https://dl.google.com/linux/chrome/deb/ stable main
EOF
    return 0
}

# survives <file> <yes|no> <why>
survives() {
    local name=$1 want=$2 why=$3
    cases=$((cases + 1))
    local there=no
    if [[ -f "$work/sources/$name" ]]; then
        there=yes
    fi
    if [[ "$there" = "$want" ]]; then
        echo "selftest: ok   $why"
    else
        echo "selftest: FAIL $why -- $name present=$there, wanted $want" >&2
        status=1
    fi
    return 0
}

stage
out=$(APT_SOURCES_DIR="$work/sources" "$script" --prune-only 2>&1)
rc=$?

cases=$((cases + 1))
if [[ "$rc" -eq 0 ]]; then
    echo "selftest: ok   pruning succeeds"
else
    echo "selftest: FAIL pruning should exit 0, got $rc" >&2
    echo "$out" >&2
    status=1
fi

survives google-chrome.list no "a third-party list is dropped"
survives microsoft-prod.list no "every third-party list is dropped, not just the first"
survives ubuntu.sources yes "the archive in deb822 form is kept"
survives archive.list yes "the archive in one-line form is kept"
survives google-chrome.list.save yes "a file apt would not read is left alone"

# The log is what a reader has when a job installs the wrong thing, so it
# has to name what happened to each file rather than only doing it.
cases=$((cases + 1))
if grep -q "dropping google-chrome.list" <<<"$out" &&
    grep -q "keeping ubuntu.sources" <<<"$out"; then
    echo "selftest: ok   the log names what was dropped and what was kept"
else
    echo "selftest: FAIL the log should name each list it acted on" >&2
    echo "$out" >&2
    status=1
fi

# A directory with nothing in it, and one that is not there at all: an
# image that keeps every source in /etc/apt/sources.list has both, and
# both are fine.
rm -rf "$work/sources"
mkdir -p "$work/sources"
cases=$((cases + 1))
if APT_SOURCES_DIR="$work/sources" "$script" --prune-only >/dev/null 2>&1; then
    echo "selftest: ok   an empty sources.list.d is accepted"
else
    echo "selftest: FAIL an empty sources.list.d was rejected" >&2
    status=1
fi

cases=$((cases + 1))
if APT_SOURCES_DIR="$work/nowhere" "$script" --prune-only >/dev/null 2>&1; then
    echo "selftest: ok   a missing sources.list.d is accepted"
else
    echo "selftest: FAIL a missing sources.list.d was rejected" >&2
    status=1
fi

# Called with nothing to install and no --prune-only, this is a workflow
# step that lost its package list. Refusing beats a silent no-op that lets
# the next step fail with "command not found".
stage
cases=$((cases + 1))
if APT_SOURCES_DIR="$work/sources" "$script" >/dev/null 2>&1; then
    echo "selftest: FAIL naming no packages should not exit 0" >&2
    status=1
else
    echo "selftest: ok   naming no packages is refused"
fi

echo "selftest: $cases case(s)"
if [[ "$status" -ne 0 ]]; then
    echo "selftest: FAILED" >&2
fi
exit "$status"
