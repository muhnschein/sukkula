#!/usr/bin/env bash
# What SonarQube Cloud made of an analysis, printed where it can be read.
#
# The scanner uploads a report and exits; the server processes it afterwards,
# so the run that produced the analysis finishes knowing nothing about its
# result. Everything -- the quality gate, the ratings, the issue list -- lives
# on a dashboard behind a login. That is fine for a person with a browser and
# useless to anything else: CI cannot act on it, and neither can a reviewer
# reading the job log, or an agent whose network policy does not reach
# sonarcloud.io.
#
# So this asks the server, from the runner that just fed it, and prints the
# answer into the job log and the step summary. The numbers land beside the
# commit that earned them.
#
# It reports and never gates: ci.yml decides what is allowed in, and nothing
# here can turn a red build green or a green build red. The workflow marks
# the step continue-on-error, so a Sonar outage costs a warning, not a build.
#
# Usage:  scripts/sonar-report.sh [path/to/report-task.txt]
#
# The scanner writes that file at the end of a run. Everything needed is in
# it -- server, project, task, and which branch or pull request was analysed
# -- which is also what makes this testable against a stub server, as
# ci/sonar-report-selftest.sh does.
#
# Taken from the sibling projects vuo and piirit.
set -euo pipefail

# `${1:-default}` and `${VAR:+...}` are avoided throughout. Not taste:
# SonarQube's own shell analyser cannot parse them and gives up on the whole
# file. A script that turns off the checker it ships beside is not clever.
if [[ "$#" -ge 1 ]]; then
    TASK_FILE=$1
else
    TASK_FILE=.scannerwork/report-task.txt
fi

# printenv rather than ${VAR:-}, for the same reason, and `|| true` because
# `set -u` would otherwise make an absent variable fatal.
SONAR_TOKEN=$(printenv SONAR_TOKEN || true)
SUMMARY=$(printenv GITHUB_STEP_SUMMARY || true)

# How many issues to list. 500 is the most the API hands over in one page.
# The first real run had 115 and this asked for 100, so the report ended
# in "15 more not listed" -- which is precisely the reading this script
# exists to replace.
PAGE=500

if [[ ! -f "$TASK_FILE" ]]; then
    echo "no $TASK_FILE -- the scanner did not get as far as uploading" >&2
    exit 1
fi

field() {
    local key=$1
    sed -n "s|^$key=||p" "$TASK_FILE" | head -1
}

SERVER=$(field serverUrl)
KEY=$(field projectKey)
TASK_URL=$(field ceTaskUrl)
DASHBOARD=$(field dashboardUrl)

# Which slice was analysed. Taken from the dashboard URL the scanner wrote
# rather than passed in, so this cannot disagree with what was uploaded.
SCOPE=$(printf '%s' "$DASHBOARD" | sed -n 's/.*[?&]\(pullRequest=[^&]*\).*/\1/p')
if [[ -z "$SCOPE" ]]; then
    SCOPE=$(printf '%s' "$DASHBOARD" | sed -n 's/.*[?&]\(branch=[^&]*\).*/\1/p')
fi
SCOPE_Q=""
SCOPE_LABEL="default branch"
if [[ -n "$SCOPE" ]]; then
    SCOPE_Q="$SCOPE&"
    SCOPE_LABEL=$SCOPE
fi

# Where every response body lands, so nothing has to survive a shell variable.
BODY=$(mktemp)

# Ask, with credentials when there are any.
#
# SonarQube Cloud answers 404 -- not 403 -- for a resource the caller may not
# read, so a 404 taken at face value reads as "no such thing" and nothing is
# ever reported. The token therefore goes FIRST. The scanner itself reads these
# same endpoints with the analysis token when `sonar.qualitygate.wait` is set,
# which makes it the likelier of the two to be allowed; anonymous is the
# fallback, for a public project where the token lacks browse rights.
code=""
fetch() {
    local url=$1
    local auth=$2
    # Emptied first: curl leaves the file alone when nothing arrives, and a
    # refusal must not be explained with the previous answer's words.
    : >"$BODY"
    if [[ -n "$auth" ]]; then
        code=$(curl -sS --max-time 30 -u "$auth:" -o "$BODY" -w '%{http_code}' "$url") || return 1
    else
        code=$(curl -sS --max-time 30 -o "$BODY" -w '%{http_code}' "$url") || return 1
    fi
    [[ "$code" = "200" ]]
}

# Why the server said no, in its own words. SonarQube answers with
# {"errors":[{"msg":...}]}; whatever else stands in front of it -- a proxy, a
# firewall -- answers with a page of its own, the start of which is shown.
why() {
    local msg
    msg=$(jq -r '[(.errors // [])[].msg] | join("; ")' "$BODY" 2>/dev/null) || msg=""
    if [[ -z "$msg" ]]; then
        msg=$(tr -s '[:space:]' ' ' <"$BODY" | cut -c1-160)
    fi
    printf '%s' "$msg"
}

# Both refusals are printed, not just the last. The anonymous answer alone
# says only that the project is not public; which of the two a fix has to
# reach -- the token's rights or the project's visibility -- is in the
# token's.
api() {
    local url=$1
    local refused=""
    if [[ -n "$SONAR_TOKEN" ]]; then
        if fetch "$url" "$SONAR_TOKEN"; then
            return 0
        fi
        refused="    with the token: HTTP $code $(why)"
    fi
    if fetch "$url" ""; then
        return 0
    fi
    echo "  cannot read $url" >&2
    if [[ -n "$refused" ]]; then
        echo "$refused" >&2
    fi
    echo "    anonymously:    HTTP $code $(why)" >&2
    return 1
}

# How many of the three sections below could not be read.
unread=0

# A rating is 1..5 on the wire and A..E everywhere a person reads it.
letter() {
    local rating=$1
    echo "$rating" | sed 's/^1.*/A/; s/^2.*/B/; s/^3.*/C/; s/^4.*/D/; s/^5.*/E/'
}

# ------------------------------------------------------------ wait for it
#
# Analysis is asynchronous. Asking for measures before the server has finished
# processing returns the PREVIOUS run's numbers, which is worse than no
# numbers at all: they look right.
status=""
analysis=""
misses=0
for _ in $(seq 60); do
    if api "$TASK_URL"; then
        misses=0
        status=$(jq -r '.task.status // "?"' "$BODY")
        if [[ "$status" = "SUCCESS" ]]; then
            analysis=$(jq -r '.task.analysisId // ""' "$BODY")
            break
        fi
        if [[ "$status" = "FAILED" ]] || [[ "$status" = "CANCELED" ]]; then
            break
        fi
    else
        # A task can be briefly invisible right after upload, so one bad
        # answer is not a verdict -- but a permanent one must not cost five
        # minutes of runner time either.
        misses=$((misses + 1))
        if [[ "$misses" -ge 5 ]]; then
            echo "the compute task cannot be read; giving up" >&2
            break
        fi
    fi
    sleep 5
done

if [[ "$status" != "SUCCESS" ]]; then
    if [[ -z "$status" ]]; then
        status=unknown
    fi
    echo "the server did not finish processing the report (status: $status)" >&2
    exit 1
fi

out=$(mktemp)
trap 'rm -f "$out" "$BODY"' EXIT

{
    echo "## SonarQube Cloud"
    echo
    echo "\`$KEY\` — $SCOPE_LABEL"
    echo
} >"$out"

# ------------------------------------------------------------ quality gate
if api "$SERVER/api/qualitygates/project_status?analysisId=$analysis"; then
    # The parentheses are load-bearing: `|` binds looser than `,` in jq, so
    # without them the pipe is applied to the two heading strings as well and
    # the whole program dies on "Cannot index string with string".
    jq -r '
        "### Quality gate: \(.projectStatus.status)", "",
        ((.projectStatus.conditions // [])[]
         | "- \(.status)  \(.metricKey) \(.comparator) \(.errorThreshold) (actual: \(.actualValue // "none"))")
    ' "$BODY" >>"$out"
    echo >>"$out"
else
    unread=$((unread + 1))
fi

# ------------------------------------------------------------ measures
# new_lines_to_cover is what turns "new_coverage: 0.0" from a verdict into
# a reading: 0.0% of one line is a file no coverage tool can reach, not a
# change nobody tested.
metrics=ncloc,coverage,line_coverage,duplicated_lines_density,violations,security_hotspots,security_rating,reliability_rating,sqale_rating,new_coverage,new_lines_to_cover,new_violations
if api "$SERVER/api/measures/component?component=$KEY&${SCOPE_Q}metricKeys=$metrics"; then
    {
        echo "### Measures"
        echo
        # A new-code measure carries its value under `period` on SonarQube
        # Server and under `periods` (an array of one) on SonarQube Cloud.
        # Read either: with only the first, every new_* line printed "-"
        # while the gate two sections up quoted a number for the same
        # metric.
        jq -r '
            (.component.measures // [])[]
            | "\(.metric)=\(.value // .period.value // (.periods // [])[0].value // "-")"
        ' "$BODY" | while IFS='=' read -r metric value; do
            case "$metric" in
                *_rating) echo "- $metric: $(letter "$value")" ;;
                *)        echo "- $metric: $value" ;;
            esac
        done
        echo
    } >>"$out"
else
    unread=$((unread + 1))
fi

# ------------------------------------------------------------ issues
if api "$SERVER/api/issues/search?componentKeys=$KEY&${SCOPE_Q}resolved=false&ps=$PAGE"; then
    total=$(jq -r '.total // 0' "$BODY")
    {
        echo "### Open issues: $total"
        echo
        if [[ "$total" = "0" ]]; then
            echo "None."
        else
            echo '```'
            jq -r '
                (.issues // [])[]
                | "\(.severity // (.impacts[0].severity? // "?"))  \(.rule)  \(.component | sub("^[^:]*:";""))\(if .line then ":\(.line)" else "" end)  \(.message)"
            ' "$BODY"
            if [[ "$total" -gt "$PAGE" ]]; then
                echo "... $((total - PAGE)) more not listed"
            fi
            echo '```'
        fi
        echo
    } >>"$out"
else
    unread=$((unread + 1))
fi

# A report with its sections missing is a heading and a link, which reads
# as nothing to report. Say so in it, and fail the step, which the workflow
# turns into a warning.
if [[ "$unread" -gt 0 ]]; then
    {
        echo "**$unread of 3 sections could not be read.** The job log says why."
        echo
    } >>"$out"
fi

echo "$DASHBOARD" >>"$out"

cat "$out"
if [[ -n "$SUMMARY" ]]; then
    cat "$out" >>"$SUMMARY"
fi

if [[ "$unread" -gt 0 ]]; then
    exit 1
fi
