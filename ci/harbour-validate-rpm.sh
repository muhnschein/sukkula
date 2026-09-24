#!/bin/bash
# Run Jolla's own validator against a built RPM, and fail on any finding.
#
# This is the authority ci/harbour-check.sh stands in for, and what
# `sfdk check -s harbour` runs: only a built package shows the Requires
# and Provides rpm generated, the stripped binary's symbols and the real
# file modes. It needs an RPM, so it runs in .github/workflows/rpm.yml
# rather than on every pull request.
#
# Every ERROR and every WARNING fails it. Harbour itself rejects on errors
# only, but each warning the validator can raise about this package -- an
# unstripped binary, a missing icon size, a Compatibility permission, a
# deprecated library -- is either a defect or QA scrutiny at intake, and
# Sukkula has no reason to carry one. ci/harbour/waivers.conf could excuse
# a known one with its reason; it is empty (docs/HARBOUR.md).
#
#     ci/harbour-validate-rpm.sh <rpm>
#     ci/harbour-validate-rpm.sh --log <saved validation log>
#
# The validator is upstream's, fetched at the commit ci/harbour/UPSTREAM
# names -- over raw.githubusercontent.com, which is reachable from networks
# that stop at github.com's pages -- and refused unless its two unvendored
# files hash to what UPSTREAM records and every vendored .conf matches the
# fetched one byte for byte. $HARBOUR_VALIDATOR points at an existing copy
# (which is then held to the same checks).
set -u
shopt -s extglob

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
upstream_file="$root/ci/harbour/UPSTREAM"
waivers="$root/ci/harbour/waivers.conf"
validator=${HARBOUR_VALIDATOR:-}
repo_path=sailfishos/sdk-harbour-rpmvalidator

usage() {
    echo "usage: $0 <rpm> | $0 --log <validation log>" >&2
    exit 2
}

# Run a command with SIGPIPE handled the way a terminal would handle it.
#
# GitHub starts a job step from a Node process, which ignores SIGPIPE, and
# an ignored disposition survives exec -- so inside the validator every
# write into a pipe whose reader has already gone returns EPIPE instead of
# killing the writer, and bash reports it: piirit's runs logged thousands of
# "echo: write error: Broken pipe" lines, enough to push the verdict past
# what the logs API returns. bash cannot undo an inherited ignore, so the
# reset happens in a process that execs the validator afterwards.
sigpipe_default() {
    if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import os, signal, sys
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])' "$@"
    elif command -v perl >/dev/null 2>&1; then
        perl -e '$SIG{PIPE} = "DEFAULT"; exec { $ARGV[0] } @ARGV or die "$!\n"' "$@"
    else
        echo "harbour-rpm: note: no python3 or perl to reset SIGPIPE with;" \
             "the validator's log may carry broken-pipe noise" >&2
        "$@"
    fi
    return
}

log=""
rpm=""
case "${1:-}" in
    --log) [[ $# -eq 2 ]] || usage; log=$2 ;;
    "" | -*) usage ;;
    *) [[ $# -eq 1 ]] || usage; rpm=$1 ;;
esac

if [[ -n "$rpm" ]]; then
    [[ -f "$rpm" ]] || { echo "harbour-rpm: FAIL no such RPM: $rpm" >&2; exit 1; }
    for tool in rpm rpm2cpio cpio file objdump readelf c++filt strings; do
        command -v "$tool" >/dev/null 2>&1 ||
            { echo "harbour-rpm: FAIL the validator needs $tool (install rpm, cpio, file, binutils)" >&2; exit 1; }
    done

    commit=$(sed -n 's/^at commit \([0-9a-f]\{40\}\).*/\1/p' "$upstream_file" | head -1)
    if [[ -z "$commit" ]]; then
        echo "harbour-rpm: FAIL ci/harbour/UPSTREAM names no commit to pin the validator to" >&2
        exit 1
    fi

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    if [[ -z "$validator" ]]; then
        # The commit ci/harbour/UPSTREAM names, so the rules vendored here
        # and the code that reads them are the same Harbour -- and so CI is
        # not executing whatever that repository's HEAD is today. A raw URL
        # at a commit id is immutable; the hashes below make sure of it.
        validator="$tmp/validator"
        mkdir -p "$validator"
        fetch_list="rpmvalidation.sh rpmvalidation.conf"
        for conf in "$root"/ci/harbour/*.conf; do
            [[ "$(basename "$conf")" = waivers.conf ]] && continue
            fetch_list="$fetch_list $(basename "$conf")"
        done
        for f in $fetch_list; do
            # -L follows redirects; the two --proto flags keep every hop on
            # HTTPS. Retried, since this is the one network step in the job.
            if ! curl -sSfL --proto '=https' --proto-redir '=https' --retry 3 \
                    -o "$validator/$f" \
                    "https://raw.githubusercontent.com/$repo_path/$commit/$f"; then
                echo "harbour-rpm: FAIL could not fetch $f of the validator at $commit" >&2
                exit 1
            fi
        done
        chmod +x "$validator/rpmvalidation.sh"
    fi
    [[ -f "$validator/rpmvalidation.sh" ]] ||
        { echo "harbour-rpm: FAIL no rpmvalidation.sh in $validator" >&2; exit 1; }

    # The two files that are run rather than vendored, against the hashes
    # UPSTREAM records for them.
    for f in rpmvalidation.sh rpmvalidation.conf; do
        want=$(sed -n "s/^[[:space:]]*sha256 \([0-9a-f]\{64\}\)[[:space:]]\{1,\}$f\$/\1/p" "$upstream_file")
        got=$(sha256sum "$validator/$f" 2>/dev/null | cut -d' ' -f1)
        if [[ -z "$want" ]] || [[ "$want" != "$got" ]]; then
            echo "harbour-rpm: FAIL $f hashes to ${got:-nothing}, but ci/harbour/UPSTREAM says ${want:-nothing};" \
                 "run scripts/update-harbour-rules.sh" >&2
            exit 1
        fi
    done

    # The vendored rules and the validator's own must be the same Harbour,
    # or the two checks disagree about what is allowed. Same commit, so a
    # difference is a vendored file edited by hand.
    for conf in "$root"/ci/harbour/*.conf; do
        name=$(basename "$conf")
        [[ "$name" = waivers.conf ]] && continue
        if ! cmp -s "$conf" "$validator/$name"; then
            echo "harbour-rpm: FAIL ci/harbour/$name is not the validator's own at $commit;" \
                 "run scripts/update-harbour-rules.sh" >&2
            exit 1
        fi
    done
    echo "harbour-rpm: validator at $commit, rules and hashes verified"

    log="$tmp/validation.log"
    # BATCHERBATCHERBATCHER makes it emit `KIND|subject|message` without
    # colour and bracket the run with !BEGIN! and !END!<verdict>!. It exits
    # non-zero for its own reasons too, so the markers decide, not the
    # status.
    BATCHERBATCHERBATCHER=1 sigpipe_default "$validator/rpmvalidation.sh" \
        -g "$validator" "$rpm" > "$log" 2>&1 || true
fi

[[ -f "$log" ]] || { echo "harbour-rpm: FAIL no validation log: $log" >&2; exit 1; }

# Shown without any broken-pipe noise the reset did not catch, and counted
# rather than dropped silently, so a reset that stops working says so.
noise=$(grep -c 'write error: Broken pipe' "$log" || true)
if [[ "$noise" -gt 0 ]]; then
    echo "harbour-rpm: note: hid $noise broken-pipe line(s) from the" \
         "validator; SIGPIPE was ignored by whatever started this"
fi
grep -v 'write error: Broken pipe' "$log" || true

verdict=$(sed -n 's/^!END!\([A-Z]*\)!.*/\1/p' "$log" | tail -1)
if [[ -z "$verdict" ]]; then
    echo "harbour-rpm: FAIL the validator produced no verdict" >&2
    exit 1
fi

# A finding is waived when one waiver's subject glob matches the
# validator's subject field *and* its message glob matches the message.
# Both, deliberately: the subject alone would waive every finding the
# validator could ever raise about a path, when only the one named is
# known and accepted.
waived_line() {
    local subject=$1 message=$2 entry wid wsubject wmessage
    [[ -f "$waivers" ]] || return 1
    while IFS= read -r entry; do
        entry=${entry%%#*}
        read -r wid wsubject wmessage <<< "$entry"
        { [[ -n "${wid:-}" ]] && [[ -n "${wsubject:-}" ]] && [[ -n "${wmessage:-}" ]]; } || continue
        # shellcheck disable=SC2053 # unquoted on purpose: they are globs.
        [[ $subject == $wsubject ]] && [[ $message == $wmessage ]] && return 0
    done < "$waivers"
    return 1
}

findings=0
waived=0
while IFS= read -r line; do
    kind=$(cut -d'|' -f1 <<< "$line")
    subject=$(cut -d'|' -f2 <<< "$line")
    message=$(cut -d'|' -f3- <<< "$line")
    if waived_line "$subject" "$message"; then
        echo "harbour-rpm: WAIVED $kind $subject -- $message"
        waived=$((waived + 1))
    else
        echo "harbour-rpm: FAIL $kind $subject -- $message" >&2
        findings=$((findings + 1))
    fi
done < <(grep -E '^(ERROR|WARNING)\|' "$log" || true)

echo
if [[ "$findings" -gt 0 ]]; then
    echo "harbour-rpm: FAILED -- $findings finding(s); Sukkula ships with none (docs/HARBOUR.md)" >&2
    exit 1
fi
# A FAIL verdict with nothing parsed is a validator whose output this
# wrapper no longer understands, which is not a pass.
if [[ "$verdict" != PASS ]] && [[ "$waived" -eq 0 ]]; then
    echo "harbour-rpm: FAILED -- the validator says $verdict and no finding explains it" >&2
    exit 1
fi
if [[ "$waived" -gt 0 ]]; then
    echo "harbour-rpm: ok -- nothing new; $waived waived finding(s) (ci/harbour/waivers.conf)"
else
    echo "harbour-rpm: ok -- the validator accepts this package with no findings"
fi
