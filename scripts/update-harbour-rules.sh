#!/bin/sh
# Refresh ci/harbour/ from sailfishos/sdk-harbour-rpmvalidator.
#
#     scripts/update-harbour-rules.sh [<commit>]
#
# Those .conf files are the authoritative Harbour rules -- the prose docs
# lag behind them -- so ci/harbour-check.sh reads them rather than a
# transcription. They are vendored so CI stays deterministic and offline;
# this script is how they move, and the diff is the review. It also records
# the hashes of the two files ci/harbour-validate-rpm.sh fetches and runs
# without vendoring, so the checks on both sides move together.
#
# Without a commit it takes the repository's HEAD; with one, that commit.
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
dest="$root/ci/harbour"
repo=https://github.com/sailfishos/sdk-harbour-rpmvalidator.git
want=${1:-}

# rpmvalidation.conf is not copied: it names the other files and the
# installed paths, which ci/harbour-check.sh restates for a source tree.
files='allowed_libraries.conf allowed_permissions.conf allowed_qmlimports.conf
       allowed_requires.conf allowed_sailjailkeys.conf deprecated_libraries.conf
       deprecated_qmlimports.conf deprecated_requires.conf disallowed_orgnames.conf
       disallowed_qmlimport_patterns.conf dropped_libraries.conf dropped_qmlimports.conf
       dropped_requires.conf'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git init -q "$tmp/validator"
git -C "$tmp/validator" fetch -q --depth 1 "$repo" "${want:-HEAD}" >&2
git -C "$tmp/validator" checkout -q FETCH_HEAD
commit=$(git -C "$tmp/validator" rev-parse HEAD)
date=$(git -C "$tmp/validator" log -1 --format=%cs)

mkdir -p "$dest"
for f in $files; do
    cp "$tmp/validator/$f" "$dest/$f"
done
sh_hash=$(sha256sum "$tmp/validator/rpmvalidation.sh" | cut -d' ' -f1)
conf_hash=$(sha256sum "$tmp/validator/rpmvalidation.conf" | cut -d' ' -f1)

cat > "$dest/UPSTREAM" <<EOF
The .conf files in this directory, waivers.conf excepted, are copied
verbatim from

    $repo

at commit $commit ($date), by scripts/update-harbour-rules.sh.
ci/harbour-validate-rpm.sh runs the validator from that same commit, and
refuses the two files it fetches but does not vendor unless they hash to:

    sha256 $sh_hash  rpmvalidation.sh
    sha256 $conf_hash  rpmvalidation.conf

They carry that project's licence, GPL-2.0-or-later, which Sukkula's own
GPL-3.0-or-later terms accept. rpmvalidation.sh itself is not vendored:
ci/harbour-check.sh reimplements the checks that a source tree can answer,
and .github/workflows/rpm.yml runs the real script against a built RPM.
EOF

echo "ci/harbour: updated to $commit ($date)"
echo "ci/harbour: now run ci/harbour-check.sh and ci/harbour-check-selftest.sh;"
echo "            a rule that moved may need the check's logic to follow it"
