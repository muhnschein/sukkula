#!/usr/bin/env bash
# Prove that scripts/sonar-report.sh still reports what it claims to report.
#
# The script's whole job is to fetch four things from SonarQube Cloud and
# render them, and it cannot be run against the real service from here: the
# analysis it would read does not exist yet when the tests run, and this
# project's network policy does not reach sonarcloud.io in the first place --
# which is the very reason the script exists. So it is run against a stub
# server that answers the four endpoints, and each case below asserts one
# thing the script got wrong once.
#
# Everything the script needs comes out of the scanner's report-task.txt, so
# a stub server plus a hand-written report-task.txt is the whole harness.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
script="$root/scripts/sonar-report.sh"
work=$(mktemp -d)

server_pid=""
# Inline rather than a cleanup function: shellcheck cannot see that a trap
# calls one, and reports every line of it as unreachable code (SC2317).
trap 'if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null; fi; rm -rf "$work"' EXIT

if [[ ! -x "$script" ]]; then
    echo "selftest: FAIL $script is missing or not executable" >&2
    exit 1
fi
for tool in python3 jq curl; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "selftest: SKIP $tool is not installed" >&2
        exit 0
    fi
done

# ---------------------------------------------------------------- the stub
#
# Answers the four endpoints the script reads, records every path it was
# asked for, and can be told to demand credentials. It refuses with 404
# rather than 403 when it does, because that is what SonarQube Cloud does
# and what the script had to learn to retry through.
cat >"$work/stub.py" <<'PY'
import json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATUS = os.environ["STUB_TASK_STATUS"]
AUTH = os.environ["STUB_AUTH_REQUIRED"] == "1"
# Everything but the compute task refused, token or not: the task is read
# with rights a project token has, the rest needs Browse.
REFUSE = os.environ.get("STUB_REFUSE") == "1"
SEEN = os.environ["STUB_SEEN"]

GATE = {
    "projectStatus": {
        "status": "OK",
        "conditions": [
            {
                "status": "OK",
                "metricKey": "new_coverage",
                "comparator": "LT",
                "errorThreshold": "80",
                "actualValue": "91.4",
            },
            {
                "status": "ERROR",
                "metricKey": "new_violations",
                "comparator": "GT",
                "errorThreshold": "0",
                "actualValue": "2",
            },
        ],
    }
}

MEASURES = {
    "component": {
        "measures": [
            {"metric": "ncloc", "value": "4120"},
            {"metric": "coverage", "value": "73.1"},
            {"metric": "security_rating", "value": "1.0"},
            {"metric": "sqale_rating", "value": "2.0"},
            {"metric": "new_coverage", "period": {"value": "91.4"}},
            {"metric": "new_lines_to_cover", "periods": [{"index": 1, "value": "1"}]},
        ]
    }
}

ISSUES = {
    "total": 2,
    "issues": [
        {
            "severity": "MAJOR",
            "rule": "rust:S1234",
            "component": "muhnschein_sukkula:crates/sukkula-core/src/lib.rs",
            "line": 42,
            "message": "Remove this unused import.",
        },
        {
            "rule": "shell:S5678",
            "component": "muhnschein_sukkula:ci/sonar-report-selftest.sh",
            "impacts": [{"severity": "LOW"}],
            "message": "Quote this expansion.",
        },
    ],
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        with open(SEEN, "a", encoding="utf-8") as seen:
            seen.write(self.path + "\n")

        if AUTH and not self.headers.get("Authorization"):
            self.send(404, {"errors": [{"msg": "Component key not found"}]})
            return

        if self.path.startswith("/api/ce/task"):
            self.send(200, {"task": {"status": STATUS, "analysisId": "AN1"}})
        elif REFUSE:
            if self.headers.get("Authorization"):
                self.send(404, {"errors": [{"msg": "Component key not found"}]})
            else:
                self.send(403, {"errors": [{"msg": "Insufficient privileges"}]})
        elif self.path.startswith("/api/qualitygates/project_status"):
            self.send(200, GATE)
        elif self.path.startswith("/api/measures/component"):
            self.send(200, MEASURES)
        elif self.path.startswith("/api/issues/search"):
            self.send(200, ISSUES)
        else:
            self.send(404, {"errors": [{"msg": "no such endpoint"}]})

    def send(self, code, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(os.environ["STUB_PORT_FILE"], "w", encoding="utf-8") as handle:
    handle.write(str(httpd.server_port))
sys.stderr.write("stub listening on %d\n" % httpd.server_port)
httpd.serve_forever()
PY

status=0
cases=0

# start_stub <task status> <auth required>
start_stub() {
    if [[ -n "$server_pid" ]]; then
        kill "$server_pid" 2>/dev/null
        wait "$server_pid" 2>/dev/null
        server_pid=""
    fi
    rm -f "$work/port" "$work/seen"
    : >"$work/seen"

    STUB_TASK_STATUS=$1 \
    STUB_AUTH_REQUIRED=$2 \
    STUB_PORT_FILE="$work/port" \
    STUB_SEEN="$work/seen" \
        python3 "$work/stub.py" 2>/dev/null &
    server_pid=$!

    local waited=0
    while [[ ! -s "$work/port" ]]; do
        sleep 0.1
        waited=$((waited + 1))
        if [[ "$waited" -ge 100 ]]; then
            echo "selftest: FAIL the stub server never came up" >&2
            return 1
        fi
    done
}

# write_task <scope query, may be empty>
write_task() {
    local scope=$1
    local port
    port=$(cat "$work/port")
    local dash="http://127.0.0.1:$port/dashboard?id=muhnschein_sukkula"
    if [[ -n "$scope" ]]; then
        dash="$dash&$scope"
    fi
    cat >"$work/report-task.txt" <<EOF
projectKey=muhnschein_sukkula
serverUrl=http://127.0.0.1:$port
serverVersion=8.0
dashboardUrl=$dash
ceTaskId=TASK1
ceTaskUrl=http://127.0.0.1:$port/api/ce/task?id=TASK1
EOF
    return 0
}

# expect <description> <text that must appear> [file to search]
expect() {
    local what=$1 needle=$2 where=$3
    cases=$((cases + 1))
    # `--` because a needle beginning with a dash is an option to grep,
    # and every measure line begins with one.
    if grep -qF -- "$needle" "$where"; then
        echo "selftest: ok   $what"
    else
        echo "selftest: FAIL $what -- no '$needle' in $where" >&2
        sed -n '1,60p' "$where" >&2
        status=1
    fi
    return 0
}

# reject <description> <text that must NOT appear> <file>
reject() {
    local what=$1 needle=$2 where=$3
    cases=$((cases + 1))
    if grep -qF -- "$needle" "$where"; then
        echo "selftest: FAIL $what -- found '$needle' in $where" >&2
        status=1
    else
        echo "selftest: ok   $what"
    fi
    return 0
}

# ------------------------------------------------- everything, anonymous
#
# A public project the token need not be presented for. This is also the
# case that catches the jq bug that once killed the whole program: `|` binds
# looser than `,`, so an unparenthesised pipe was applied to the heading
# strings too and jq died on "Cannot index string with string". If the
# heading and a condition line are both here, that is fixed.
start_stub SUCCESS 0 || exit 1
write_task ""
env -u SONAR_TOKEN -u GITHUB_STEP_SUMMARY \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"
rc=$?

cases=$((cases + 1))
if [[ "$rc" -eq 0 ]]; then
    echo "selftest: ok   a complete analysis is reported"
else
    echo "selftest: FAIL a complete analysis should exit 0, got $rc" >&2
    cat "$work/err" >&2
    status=1
fi

expect "the quality gate is rendered" "### Quality gate: OK" "$work/out"
expect "a passing condition is listed" "OK  new_coverage LT 80 (actual: 91.4)" "$work/out"
expect "a failing condition is listed" "ERROR  new_violations GT 0 (actual: 2)" "$work/out"
expect "measures are rendered" "- coverage: 73.1" "$work/out"
expect "a new-code measure reads its period value" "- new_coverage: 91.4" "$work/out"
# SonarQube Cloud answers with `periods`, an array, where Server answers
# with `period`. The first real run printed "-" for every new_* measure.
expect "and one in Cloud's periods shape too" "- new_lines_to_cover: 1" "$work/out"
expect "issues are counted" "### Open issues: 2" "$work/out"
expect "an issue names its file and line" "crates/sukkula-core/src/lib.rs:42" "$work/out"
expect "an issue with only impacts still has a severity" "LOW  shell:S5678" "$work/out"
expect "the dashboard link is printed" "/dashboard?id=muhnschein_sukkula" "$work/out"

# The first real run had 115 issues and the script asked for 100, so the
# report ended in "15 more not listed". A page is the most the API gives.
expect "the issue list asks for a whole page" "resolved=false&ps=500" "$work/seen"

# A rating is 1..5 on the wire. Printed as a number it is unreadable, and
# worse, it reads like a score out of five with the polarity reversed.
expect "a rating is a letter" "- security_rating: A" "$work/out"
expect "the other rating is a letter too" "- sqale_rating: B" "$work/out"
reject "no raw rating survives" "security_rating: 1.0" "$work/out"

# ------------------------------------------------------- the scope is passed
#
# The slice comes out of the dashboard URL the scanner wrote, and it has to
# reach the measures and issues queries: without it they answer for the
# default branch, which is not what was analysed and looks entirely
# plausible.
start_stub SUCCESS 0 || exit 1
write_task "pullRequest=45"
env -u SONAR_TOKEN -u GITHUB_STEP_SUMMARY \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"

expect "the scope is named in the report" "pullRequest=45" "$work/out"
expect "the measures query carries the scope" \
    "/api/measures/component?component=muhnschein_sukkula&pullRequest=45&" \
    "$work/seen"
expect "the issues query carries the scope" \
    "/api/issues/search?componentKeys=muhnschein_sukkula&pullRequest=45&" \
    "$work/seen"

# --------------------------------------------------------- the token first
#
# SonarQube Cloud answers 404, not 403, for something the caller may not
# read. The first version of this script asked anonymously, took the 404 for
# "no such thing", and reported nothing at all. So the token goes first.
start_stub SUCCESS 1 || exit 1
write_task ""
SONAR_TOKEN=squ_stub \
    env -u GITHUB_STEP_SUMMARY \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"

expect "a project that needs the token is still reported" \
    "### Quality gate: OK" "$work/out"
expect "and its measures with it" "- coverage: 73.1" "$work/out"

# Without a token, the same server must fail loudly rather than print an
# empty report that looks like a clean bill of health.
start_stub SUCCESS 1 || exit 1
write_task ""
env -u SONAR_TOKEN -u GITHUB_STEP_SUMMARY \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"
rc=$?

cases=$((cases + 1))
if [[ "$rc" -ne 0 ]]; then
    echo "selftest: ok   an unreadable project fails instead of reporting nothing"
else
    echo "selftest: FAIL an unreadable project should not exit 0" >&2
    status=1
fi
reject "and prints no gate" "Quality gate" "$work/out"

# The task readable and nothing else: a heading and a link, which the
# first report from main was, reads as a clean bill of health. It has to
# say what is missing, give both refusals in the server's words, and fail.
STUB_REFUSE=1 start_stub SUCCESS 0 || exit 1
write_task "branch=main"
SONAR_TOKEN=squ_stub GITHUB_STEP_SUMMARY="$work/summary-refused" \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"
rc=$?

cases=$((cases + 1))
if [[ "$rc" -ne 0 ]]; then
    echo "selftest: ok   a report with nothing readable in it fails"
else
    echo "selftest: FAIL a report with nothing readable in it should not exit 0" >&2
    status=1
fi
expect "the report says what is missing" "3 of 3 sections could not be read" "$work/out"
expect "and so does the step summary" "3 of 3 sections could not be read" "$work/summary-refused"
expect "the token's refusal is given" \
    "with the token: HTTP 404 Component key not found" "$work/err"
expect "and the anonymous one" \
    "anonymously:    HTTP 403 Insufficient privileges" "$work/err"

# ------------------------------------------------------ the step summary
#
# The job log scrolls; the step summary is what a reviewer opens. The same
# text has to land in both.
start_stub SUCCESS 0 || exit 1
write_task ""
GITHUB_STEP_SUMMARY="$work/summary" \
    env -u SONAR_TOKEN \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"

expect "the step summary gets the report" "### Quality gate: OK" "$work/summary"
expect "the step summary gets the measures" "- coverage: 73.1" "$work/summary"

# ------------------------------------------------------- what went wrong
#
# A report the server never finished processing must not be rendered from
# whatever the previous run left behind. Asking for measures too early
# returns the PREVIOUS analysis's numbers, which is worse than none: they
# look right.
start_stub FAILED 0 || exit 1
write_task ""
env -u SONAR_TOKEN -u GITHUB_STEP_SUMMARY \
    "$script" "$work/report-task.txt" >"$work/out" 2>"$work/err"
rc=$?

cases=$((cases + 1))
if [[ "$rc" -ne 0 ]]; then
    echo "selftest: ok   a failed compute task is not reported as an analysis"
else
    echo "selftest: FAIL a failed compute task should not exit 0" >&2
    status=1
fi
expect "and says so" "status: FAILED" "$work/err"
reject "and prints no measures" "### Measures" "$work/out"

# A run where the scanner never uploaded has nothing to report on, and the
# workflow marks this step continue-on-error, so failing here is a warning.
cases=$((cases + 1))
if "$script" "$work/nothing-here.txt" >"$work/out" 2>"$work/err"; then
    echo "selftest: FAIL a missing report-task.txt should not exit 0" >&2
    status=1
else
    echo "selftest: ok   a missing report-task.txt is refused"
fi
expect "and names the file" "nothing-here.txt" "$work/err"

echo "selftest: $cases case(s)"
if [[ "$status" -ne 0 ]]; then
    echo "selftest: FAILED" >&2
fi
exit "$status"
