#!/bin/bash
# Every fuzz target has committed seeds and a dictionary libFuzzer can
# read; checked before anything is fuzzed. clove's ci/check-dicts.sh is
# the model.
#
#     ci/check-dicts.sh              the fuzz crate in this tree
#     ci/check-dicts.sh --root DIR   the fuzz crate in DIR/fuzz (the self-test)
#     ci/check-dicts.sh --self-test  prove the checker still refuses
#
# Why each rule (finding "fuzz-smoke silently fuzzes a target with no
# dictionary or seeds"):
#
#   - seeds: fuzz/corpus/ is not committed, so in CI a target starts from
#     fuzz/seeds/<target>/ alone, and sixty seconds from nothing never gets
#     past the first length or magic check of a protocol parser. A target
#     without seeds used to be fuzzed anyway, and reported ok.
#   - a dictionary: the same, for the tokens a parser branches on.
#   - the dictionary's grammar: libFuzzer exits before fuzzing anything on
#     one line it cannot parse, and scripts/fuzz-smoke.sh used to report
#     that as a crash with no reproducer. The grammar is libFuzzer's
#     ParseDictionaryFile: blank lines and `#` comments are skipped; every
#     other line is `"token"` or `name="token"`, and inside the quotes the
#     only escapes are `\\`, `\"` and `\xHH` -- `\n`, `\r`, `\t` are not
#     escapes to it; spell them `\x0a`, `\x0d`, `\x09`. An empty dictionary
#     is an error too.
#   - no leftovers: a dictionary or a seed directory whose target is gone
#     is one nobody is fuzzing with.
#
# The targets are fuzz/Cargo.toml's [[bin]] entries, which is what
# `cargo fuzz list` reads; the self-test exists because this is a
# restatement of somebody else's grammar, the kind of check that drifts
# into printing "ok" forever.
set -u

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
self_test=0
case "${1:-}" in
    --self-test) self_test=1 ;;
    --root) root=${2:?--root needs a directory} ;;
    "") ;;
    *) echo "usage: $0 [--self-test | --root DIR]" >&2; exit 2 ;;
esac

# ParseOneDictionaryEntry, line for line; prints libFuzzer's own message
# for a line it would refuse.
# shellcheck disable=SC2016 # awk's program: its $ are awk's own
PARSER='
function is_space(ch) { return ch == " " || ch == "\t" || ch == "\r" || ch == "\v" || ch == "\f" }
function is_hex(ch) { return index("0123456789abcdefABCDEF", ch) > 0 }
function parse_entry(s,   n, L, R, pos, ch, esc) {
    n = length(s)
    if (n == 0) return 0
    L = 1; R = n
    while (L < R && is_space(substr(s, L, 1))) L++
    while (R > L && is_space(substr(s, R, 1))) R--
    # Two quotes and something between them.
    if (R - L < 2) return 0
    if (substr(s, R, 1) != "\"") return 0
    R--
    # Whatever precedes the opening quote is a name: name="token".
    while (L < R && substr(s, L, 1) != "\"") L++
    if (L >= R) return 0
    L++
    for (pos = L; pos <= R; pos++) {
        ch = substr(s, pos, 1)
        if (ch != "\\") continue
        if (pos + 1 > R) return 0
        pos++
        esc = substr(s, pos, 1)
        if (esc == "\\" || esc == "\"") continue
        if (esc != "x") return 0
        if (pos + 2 > R) return 0
        if (!is_hex(substr(s, pos + 1, 1)) || !is_hex(substr(s, pos + 2, 1))) return 0
        pos += 2
    }
    return 1
}
{
    line = $0; pos = 1
    while (pos <= length(line) && is_space(substr(line, pos, 1))) pos++
    if (pos > length(line)) next
    if (substr(line, pos, 1) == "#") next
    if (parse_entry(line)) { entries++; next }
    printf "check-dicts: FAIL %s: ParseDictionaryFile: error in line %d\n\t\t%s\n", FILENAME, FNR, line
    bad++
}
END {
    if (bad) exit 1
    if (entries == 0) {
        printf "check-dicts: FAIL %s: no entries; libFuzzer refuses an empty dictionary\n", FILENAME
        exit 1
    }
}
'

check_dict() { awk "$PARSER" "$1" >&2; }

# check_tree <root>: every rule above, over <root>/fuzz.
check_tree() {
    local fuzz="$1/fuzz" status=0 t d count=0
    [[ -f "$fuzz/Cargo.toml" ]] || { echo "check-dicts: FAIL no $fuzz/Cargo.toml" >&2; return 1; }
    local -a targets
    mapfile -t targets < <(awk '
        /^\[\[bin\]\]/ { bin = 1; next }
        /^\[/ { bin = 0 }
        bin && /^name[[:space:]]*=/ { v = $0; sub(/^[^"]*"/, "", v); sub(/".*/, "", v); print v }
    ' "$fuzz/Cargo.toml")
    if [[ ${#targets[@]} -eq 0 ]]; then
        echo "check-dicts: FAIL fuzz/Cargo.toml has no [[bin]] targets" >&2
        return 1
    fi
    for t in "${targets[@]}"; do
        if [[ ! -f "$fuzz/dicts/$t.dict" ]]; then
            echo "check-dicts: FAIL fuzz target '$t' has no fuzz/dicts/$t.dict" >&2
            status=1
        fi
        if [[ ! -d "$fuzz/seeds/$t" ]] || [[ -z "$(find "$fuzz/seeds/$t" -type f -print -quit)" ]]; then
            echo "check-dicts: FAIL fuzz target '$t' has no committed seeds in fuzz/seeds/$t/" >&2
            status=1
        fi
    done
    for d in "$fuzz"/dicts/*.dict; do
        [[ -e "$d" ]] || continue
        count=$((count + 1))
        t=$(basename "$d" .dict)
        [[ " ${targets[*]} " == *" $t "* ]] ||
            { echo "check-dicts: FAIL fuzz/dicts/$t.dict belongs to no target" >&2; status=1; }
        check_dict "$d" || status=1
    done
    for d in "$fuzz"/seeds/*/; do
        [[ -d "$d" ]] || continue
        t=$(basename "$d")
        [[ " ${targets[*]} " == *" $t "* ]] ||
            { echo "check-dicts: FAIL fuzz/seeds/$t/ belongs to no target" >&2; status=1; }
    done
    [[ "$status" -eq 0 ]] && echo "check-dicts: ok (${#targets[@]} targets, $count dictionaries)"
    return "$status"
}

if [[ "$self_test" = 0 ]]; then
    check_tree "$root"
    exit
fi

# --- the self-test ------------------------------------------------------------
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
status=0
cases=0

# Entries libFuzzer takes: a bare token, the name= form, both string
# escapes, a hex escape, a line with space around it.
while IFS= read -r entry; do
    cases=$((cases + 1))
    printf '%s\n' "$entry" > "$work/one.dict"
    check_dict "$work/one.dict" 2>/dev/null ||
        { echo "check-dicts: self-test: refused a valid entry: $entry" >&2; status=1; }
done <<'GOOD'
"abc"
name="abc"
"\x0d"
"\\"
"\""
"a\\b\"c\x41"
  "abc"
GOOD

# And ones it refuses. The first three are the ones people write.
while IFS= read -r entry; do
    cases=$((cases + 1))
    printf '%s\n' "$entry" > "$work/one.dict"
    if check_dict "$work/one.dict" 2>/dev/null; then
        echo "check-dicts: self-test: accepted an invalid entry: $entry" >&2
        status=1
    fi
done <<'BAD'
"\n"
"\r"
"\t"
"\q"
"abc
abc"
abc
""
"\x4"
"\xzz"
"\"
BAD

cases=$((cases + 1))
printf '# just a comment\n' > "$work/one.dict"
if check_dict "$work/one.dict" 2>/dev/null; then
    echo "check-dicts: self-test: accepted a dictionary with no entries" >&2
    status=1
fi

# A small fuzz crate: two targets, each with a dictionary and a seed.
make_tree() {
    rm -rf "$work/tree"
    mkdir -p "$work/tree/fuzz/dicts" "$work/tree/fuzz/seeds/alpha" "$work/tree/fuzz/seeds/beta"
    cat > "$work/tree/fuzz/Cargo.toml" <<'EOF'
[package]
name = "x-fuzz"

[[bin]]
name = "alpha"
path = "fuzz_targets/alpha.rs"

[[bin]]
name = "beta"
path = "fuzz_targets/beta.rs"
EOF
    printf '"GET"\nkw="\\x00\\xff"\n' > "$work/tree/fuzz/dicts/alpha.dict"
    printf '"{"\n' > "$work/tree/fuzz/dicts/beta.dict"
    printf 'a' > "$work/tree/fuzz/seeds/alpha/0"
    printf '{}' > "$work/tree/fuzz/seeds/beta/0"
}
# tree_case <pass|fail> <what> <change, run in the tree>
tree_case() {
    local want=$1 what=$2 change=$3 got
    cases=$((cases + 1))
    make_tree
    ( cd "$work/tree" && eval "$change" ) ||
        { echo "check-dicts: self-test: could not apply '$what'" >&2; status=1; return 0; }
    if check_tree "$work/tree" >/dev/null 2>&1; then got=pass; else got=fail; fi
    if [[ "$got" != "$want" ]]; then
        echo "check-dicts: self-test: $what should $want, got $got" >&2
        check_tree "$work/tree" >&2
        status=1
    fi
    return 0
}
# The changes are single-quoted on purpose: eval runs them in the tree.
# shellcheck disable=SC2016
{
tree_case pass "a crate whose every target has seeds and a dictionary" ':'
tree_case fail "a target with no dictionary" 'rm fuzz/dicts/beta.dict'
tree_case fail "a target with no seed directory" 'rm -r fuzz/seeds/beta'
tree_case fail "a target with an empty seed directory" 'rm fuzz/seeds/beta/0'
tree_case fail "a dictionary libFuzzer cannot parse" 'printf "\"\\\\n\"\n" >> fuzz/dicts/alpha.dict'
tree_case fail "an empty dictionary" ': > fuzz/dicts/beta.dict'
tree_case fail "a dictionary whose target is gone" 'printf "\"x\"\n" > fuzz/dicts/gamma.dict'
tree_case fail "seeds whose target is gone" 'mkdir fuzz/seeds/gamma && : > fuzz/seeds/gamma/0'
tree_case fail "a crate with no targets" 'sed -i "/^\[\[bin\]\]/,\$d" fuzz/Cargo.toml'
}

if [[ "$status" -eq 0 ]]; then
    echo "check-dicts: self-test ok ($cases cases)"
else
    echo "check-dicts: self-test FAILED" >&2
fi
exit "$status"
