#!/bin/bash
# The one reader of ci/harbour/waivers.conf, sourced by ci/harbour-check.sh
# and ci/harbour-validate-rpm.sh. Not run on its own.
#
# One parser for both checks, because two used to read the same line two
# ways (finding "Waiver scope differs between the two Harbour checks"): the
# RPM wrapper ignored the check id, so a waiver written for a source finding
# with the message `*` excused every validator finding about that path; and
# the source check failed every waiver it did not use itself as stale, so a
# waiver for a validator-only finding could not be written at all.
#
# A waiver says which check it is for, and each check matches every field
# it has:
#
#     <checker>  <id>  <subject glob>  <message glob>   # reason
#
#   source  ci/harbour-check.sh. <id> is a check ID of docs/HARBOUR.md
#           (1.8.3, P.2), matched exactly.
#   rpm     ci/harbour-validate-rpm.sh. <id> is the validator's severity,
#           ERROR or WARNING, matched exactly: rpmvalidation.sh names no
#           check, only the kind, the subject and the message.
#
# Subject and message are bash globs; the message is the rest of the line
# up to the `#`, spaces and all. The message glob has to name something --
# a glob of nothing but `*` and `?` is refused -- so a waiver excuses one
# known finding and never every finding about a subject. The reason after
# the `#` is required. Each check fails on a malformed line, and on a
# waiver of its own that matched nothing (a stale one); neither reads the
# other's.

# waiver_records <file>
#
# Prints one record per waiver, tab-separated:
#     <line number> <checker> <id> <subject glob> <message glob>
# and `harbour-waivers: FAIL ...` on stderr for every malformed line.
# Returns 1 if there was one. A missing file is no waivers.
waiver_records() {
    local file=$1 raw line reason checker id subject message why n=0 bad=0
    [[ -f "$file" ]] || return 0
    while IFS= read -r raw || [[ -n "$raw" ]]; do
        n=$((n + 1))
        line=${raw%%#*}
        [[ -n "${line//[[:space:]]/}" ]] || continue
        reason=""
        [[ "$raw" == *'#'* ]] && reason=${raw#*#}
        checker="" id="" subject="" message="" why=""
        read -r checker id subject message <<< "$line"
        case "$checker" in
            source) ;;
            rpm)
                case "$id" in
                    ERROR|WARNING) ;;
                    *) why="an rpm waiver's id is the validator's ERROR or WARNING, not '$id'" ;;
                esac
                ;;
            *) why="'$checker' is not a checker: the line starts with source or rpm" ;;
        esac
        if [[ -z "$why" ]]; then
            if [[ -z "$subject" || -z "$message" ]]; then
                why="it needs a checker, an id, a subject glob and a message glob"
            elif [[ -z "${message//[*?[:space:]]/}" ]]; then
                why="the message glob '$message' matches any message; name the finding"
            elif [[ "$message" == *$'\t'* ]]; then
                why="a tab in the message glob; write ? or a space"
            elif [[ -z "${reason//[[:space:]]/}" ]]; then
                why="no reason after the #"
            fi
        fi
        if [[ -n "$why" ]]; then
            echo "harbour-waivers: FAIL ci/harbour/waivers.conf:$n: $why" >&2
            bad=1
            continue
        fi
        printf '%s\t%s\t%s\t%s\t%s\n' "$n" "$checker" "$id" "$subject" "$message"
    done < "$file"
    return "$bad"
}
