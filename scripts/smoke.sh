#!/usr/bin/env bash
# Smoke test: builds bashprof, profiles a real script end to end, then
# asserts on the report, the collapsed stacks, the JSON output, the source
# annotation, exit-code passthrough and offline trace replay. Self-contained:
# temp dirs only, no network.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }

echo "[smoke] building..."
cargo build --quiet
BIN=target/debug/bashprof

WORK=$(mktemp -d "${TMPDIR:-/tmp}/bashprof-smoke.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# --- 1. version/help sanity -------------------------------------------------
"$BIN" --version | grep -q '^bashprof 0\.1\.0$' || fail "--version mismatch"
"$BIN" --help | grep -q 'COMMANDS:' || fail "--help missing sections"

# --- 2. profile a realistic script ------------------------------------------
cat > "$WORK/setup.sh" <<'EOF'
set -euo pipefail
fetch() {
  local pkg
  for pkg in alpha beta gamma; do
    sleep 0.05
  done
}
compile() {
  sleep 0.12
}
fetch
compile
for i in 1 2 3 4 5; do
  echo "cache-$i" > /dev/null
done
echo setup-complete
EOF

echo "[smoke] bashprof run (report + kept trace)"
"$BIN" run --top 0 --out "$WORK/setup.trace" "$WORK/setup.sh" > "$WORK/report.out" 2> "$WORK/report.err"
grep -q 'setup-complete' "$WORK/report.out" || fail "script stdout not passed through"
grep -q '(exit 0)' "$WORK/report.out" || fail "report missing exit code"
grep -q 'commands traced' "$WORK/report.out" || fail "report missing totals line"
# The sleep inside the loop ran exactly 3 times; the cache loop 5 times.
grep -Eq ' 3  setup.sh:5 +sleep 0.05' "$WORK/report.out" || fail "loop count wrong for setup.sh:5"
grep -Eq ' 5  setup.sh:14 ' "$WORK/report.out" || fail "loop count wrong for setup.sh:14"
grep -q 'FUNCTIONS (self-time)' "$WORK/report.out" || fail "functions section missing"
grep -Eq '1x +fetch' "$WORK/report.out" || fail "fetch call count missing"
# The slow line must dominate: it appears before the echo lines (sort=self).
SLEEP_ROW=$(grep -n 'setup.sh:5' "$WORK/report.out" | head -1 | cut -d: -f1)
ECHO_ROW=$(grep -n 'setup.sh:16' "$WORK/report.out" | head -1 | cut -d: -f1)
[ "$SLEEP_ROW" -lt "$ECHO_ROW" ] || fail "hot line not sorted first"
grep -q 'raw trace kept at' "$WORK/report.err" || fail "--out note missing"
[ -s "$WORK/setup.trace" ] || fail "trace file empty"
echo "[smoke] report OK (hot line: sleep 0.05 x3)"

# --- 3. offline replay: report / collapse / annotate / json -----------------
echo "[smoke] offline replay from the kept trace"
"$BIN" report --top 0 "$WORK/setup.trace" > "$WORK/replay.out"
grep -q '(exit 0)' "$WORK/replay.out" || fail "replayed report missing exit code"

"$BIN" collapse "$WORK/setup.trace" > "$WORK/folded.out"
grep -Eq '^main;fetch;setup.sh:5 [0-9]+$' "$WORK/folded.out" || fail "collapsed stack missing fetch frame"
grep -Eq '^main;setup.sh:16 [0-9]+$' "$WORK/folded.out" || fail "collapsed stack missing top-level line"
sort -c "$WORK/folded.out" || fail "collapsed output not sorted/deterministic"
echo "[smoke] collapse OK ($(wc -l < "$WORK/folded.out") folded stacks)"

"$BIN" annotate "$WORK/setup.trace" > "$WORK/annotate.out"
grep -Eq '^\s+-\s+-\s+set -euo pipefail' "$WORK/annotate.out" && fail "executed line shown as untouched"
grep -q 'sleep 0.05' "$WORK/annotate.out" || fail "annotate lost source text"
head -1 "$WORK/annotate.out" | grep -q 'SELF' || fail "annotate header missing"

"$BIN" report --json "$WORK/setup.trace" > "$WORK/report.json"
grep -q '"tool": "bashprof"' "$WORK/report.json" || fail "json missing tool key"
grep -q '"exit_code": 0' "$WORK/report.json" || fail "json missing exit code"
grep -q '"name": "fetch", "calls": 1' "$WORK/report.json" || fail "json missing function stats"
echo "[smoke] annotate + json OK"

# --- 4. failing script: exit code passthrough --------------------------------
cat > "$WORK/fail.sh" <<'EOF'
echo before-failure
exit 7
EOF
set +e
"$BIN" run "$WORK/fail.sh" > "$WORK/fail.out" 2>/dev/null
RC=$?
set -e
[ "$RC" -eq 7 ] || fail "exit code not passed through (got $RC, want 7)"
grep -q '(exit 7)' "$WORK/fail.out" || fail "failing run still needs a report"
echo "[smoke] exit-code passthrough OK (7)"

# --- 5. usage errors keep their own exit code ---------------------------------
set +e
"$BIN" frobnicate 2>/dev/null
[ $? -eq 2 ] || fail "usage error must exit 2"
"$BIN" report "$WORK/fail.sh" 2>/dev/null
[ $? -eq 1 ] || fail "non-trace input must exit 1"
set -e

echo "SMOKE OK"
