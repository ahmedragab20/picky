#!/usr/bin/env bash
# Builds a small TypeScript repo, makes a few kinds of changes, and runs a CI-style
# step through the picky hook exactly the way GitHub Actions runs `run:` steps.
#
#   bash examples/demo.sh             graph-only (no API key needed)
#   PICKY_API_KEY=... bash examples/demo.sh   uses Jev for tests outside the graph
#
# Colors use the Rosé Pine palette (24-bit). NO_COLOR or a non-terminal turns them off;
# FORCE_COLOR turns them on.
set -euo pipefail

PICKY_HOME="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$(mktemp -d "${TMPDIR:-/tmp}/picky-demo.XXXXXX")"
MODEL="none"
[ -n "${PICKY_API_KEY:-}${TYPESAFE_API_KEY:-}" ] && MODEL="jev"

use_color=0
if [ -n "${FORCE_COLOR:-}" ]; then use_color=1; elif [ -z "${NO_COLOR:-}" ] && [ -t 1 ]; then use_color=1; fi
rgb() { [ "$use_color" = 1 ] && printf '\033[38;2;%d;%d;%dm' "$1" "$2" "$3" || true; }
LOVE="$(rgb 235 111 146)"
GOLD="$(rgb 246 193 119)"
ROSE="$(rgb 235 188 186)"
PINE="$(rgb 49 116 143)"
FOAM="$(rgb 156 207 216)"
IRIS="$(rgb 196 167 231)"
SUBTLE="$(rgb 144 140 170)"
MUTED="$(rgb 110 106 134)"
BORDER="$(rgb 64 61 82)"
if [ "$use_color" = 1 ]; then BOLD=$'\033[1m'; RESET=$'\033[0m'; else BOLD=""; RESET=""; fi

cd "$DEMO"
git init -q -b main
git config user.email demo@picky.dev
git config user.name "picky demo"

mkdir -p src test
cat > package.json <<'JSON'
{ "name": "shop", "type": "module", "scripts": { "test": "node --test" } }
JSON
cat > .picky.yml <<YAML
mode: enforce
model: $MODEL
YAML
cat > src/math.ts <<'TS'
export const add = (a: number, b: number) => a + b;
TS
cat > src/cart.ts <<'TS'
import { add } from './math.ts';
export const total = (prices: number[]) => prices.reduce(add, 0);
TS
cat > src/strings.ts <<'TS'
export const shout = (s: string) => `${s.toUpperCase()}!`;
TS
cat > test/math.test.ts <<'TS'
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { add } from '../src/math.ts';
test('adds two numbers', () => assert.equal(add(2, 3), 5));
TS
cat > test/cart.test.ts <<'TS'
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { total } from '../src/cart.ts';
test('sums the cart', () => assert.equal(total([1, 2, 3]), 6));
TS
cat > test/strings.test.ts <<'TS'
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shout } from '../src/strings.ts';
test('shouts', () => assert.equal(shout('hi'), 'HI!'));
TS
echo "# Shop" > README.md
git add -A && git commit -qm "initial"

# The CI step, as written in a workflow:  - run: npm test   # picky
cat > "$DEMO/.step.sh" <<'STEP'
npm test --silent   # picky
STEP

# run_step <run id> [mode]
run_step() {
  # GitHub Actions runs each `run:` step as: bash --noprofile --norc -eo pipefail <script>
  # Each scenario is its own CI run, so each gets its own runner temp (where decisions are cached).
  env ${2:+PICKY_MODE="$2"} BASH_ENV="$PICKY_HOME/hook/hook.sh" PICKY_BIN="$PICKY_HOME/bin/picky" RUNNER_TEMP="$DEMO/.git/runner-$1" \
    bash --noprofile --norc -eo pipefail "$DEMO/.step.sh" 2>&1 | grep -Ev '^ℹ (tests|suites|pass|fail|cancelled|skipped|todo|duration_ms) ' || true
}

# Colors picky's log lines and the test runner's output, inside a left gutter.
colorize() {
  local line rest file reason bar="${BORDER}│${RESET}"
  while IFS= read -r line; do
    case "$line" in
      'picky:   RUN  '*)
        rest="${line#picky:   RUN  }"; file="${rest%%  (*}"; reason="${rest#"$file"}"
        printf '   %s %spicky%s  %s%sRUN %s %s%s%s\n' "$bar" "$MUTED" "$RESET" "$BOLD" "$FOAM" "$RESET" "$file" "$SUBTLE" "$reason$RESET" ;;
      'picky:   skip '*)
        rest="${line#picky:   skip }"; file="${rest%%  (*}"; reason="${rest#"$file"}"
        printf '   %s %spicky%s  %sskip %s%s%s\n' "$bar" "$MUTED" "$RESET" "$MUTED" "$SUBTLE" "$file$MUTED" "$reason$RESET" ;;
      'picky: $ '*)
        printf '   %s %spicky%s  %s%s%s\n' "$bar" "$MUTED" "$RESET" "$IRIS" "${line#picky: }" "$RESET" ;;
      'picky: '*' -> '*)
        printf '   %s %spicky%s  %s%s%s\n' "$bar" "$MUTED" "$RESET" "$BOLD$GOLD" "${line#picky: }" "$RESET" ;;
      'picky: selective'* | 'picky: full'*)
        printf '   %s %spicky%s  %s%s%s\n' "$bar" "$MUTED" "$RESET" "$ROSE" "${line#picky: }" "$RESET" ;;
      'picky: '*)
        printf '   %s %spicky%s  %s%s%s\n' "$bar" "$MUTED" "$RESET" "$SUBTLE" "${line#picky: }" "$RESET" ;;
      '✔'*) printf '   %s %s%s%s\n' "$bar" "$PINE" "$line" "$RESET" ;;
      '✖'*) printf '   %s %s%s%s\n' "$bar" "$LOVE" "$line" "$RESET" ;;
      *) printf '   %s %s\n' "$bar" "$line" ;;
    esac
  done
}

field() { printf '   %s%-8s%s %s\n' "$MUTED" "$1" "$RESET" "$2"; }

TOTAL=4
TITLES=()
RESULTS=()
scenario() {
  local n="$1" title="$2" change="$3" expect="$4" mode="${5:-}" out
  printf '\n %s●%s %s%s/%s%s  %s%s%s\n' "$IRIS" "$RESET" "$MUTED" "$n" "$TOTAL" "$RESET" "$BOLD" "$title" "$RESET"
  field change "$change"
  field expect "$expect"
  field step "${IRIS}npm test   # picky${RESET}"
  echo
  out="$(run_step "$n" "$mode")"
  printf '%s\n' "$out" | colorize
  TITLES+=("$title")
  RESULTS+=("$(printf '%s\n' "$out" | sed -n 's/^picky: .* -> //p' | head -n 1)")
}

printf '\n %s▌%s %spicky demo%s\n' "$ROSE" "$RESET" "$BOLD$ROSE" "$RESET"
printf ' %s▌%s %sRuns only the tests a change can affect. Any doubt means everything runs.%s\n' "$ROSE" "$RESET" "$SUBTLE" "$RESET"
if [ "$MODEL" = none ]; then
  printf ' %s▌%s model  %snone (graph only)%s %s· set PICKY_API_KEY to let Jev judge tests the graph cannot link%s\n' "$ROSE" "$RESET" "$GOLD" "$RESET" "$MUTED" "$RESET"
else
  printf ' %s▌%s model  %sjev%s %s· judges tests the graph cannot link%s\n' "$ROSE" "$RESET" "$FOAM" "$RESET" "$MUTED" "$RESET"
fi

printf '\n %sThe demo repo%s %s(what imports what)%s\n' "$BOLD" "$RESET" "$MUTED" "$RESET"
printf '   %s%-15s%s %s←%s src/cart.ts, test/math.test.ts\n' "$FOAM" src/math.ts "$RESET" "$MUTED" "$RESET"
printf '   %s%-15s%s %s←%s test/cart.test.ts\n' "$FOAM" src/cart.ts "$RESET" "$MUTED" "$RESET"
printf '   %s%-15s%s %s←%s test/strings.test.ts\n' "$FOAM" src/strings.ts "$RESET" "$MUTED" "$RESET"
printf '   %s%-15s%s %s(not imported by anything)%s\n' "$FOAM" README.md "$RESET" "$MUTED" "$RESET"

if [ "$MODEL" = none ]; then STRINGS="strings test skips (no import link)"; else STRINGS="Jev decides on the strings test"; fi

git checkout -qb feature/math
sed -i.bak 's/a + b/b + a/' src/math.ts && rm src/math.ts.bak
git commit -qam "reorder add"
scenario 1 "Source change" \
  "src/math.ts: a + b → b + a" \
  "math and cart tests run (both reach math.ts); $STRINGS"

git checkout -q main && git checkout -qb feature/docs
echo "Now with docs." >> README.md
git commit -qam "docs"
scenario 2 "Docs-only change" \
  "README.md: one line added" \
  "no test can be affected, so the step is skipped"

git checkout -q main && git checkout -qb feature/deps
sed -i.bak 's/"shop"/"shop-app"/' package.json && rm package.json.bak
git commit -qam "rename package"
scenario 3 "package.json change" \
  "package.json: name shop → shop-app" \
  "dependencies or scripts can change anything, so every JS test runs"

git checkout -q feature/math
scenario 4 "Shadow mode" \
  "same commit as 1/$TOTAL, with PICKY_MODE=shadow" \
  "same decision as 1/$TOTAL, but everything runs; picky only logs what it would do" \
  shadow

printf '\n %sSummary%s\n' "$BOLD" "$RESET"
for i in "${!TITLES[@]}"; do
  result="${RESULTS[$i]:-no decision logged}"
  case "$result" in
    skip*) color="$PINE" ;;
    full*) color="$GOLD" ;;
    shadow*) color="$IRIS" ;;
    *) color="$FOAM" ;;
  esac
  printf '   %s%s%s  %-20s %s%s%s\n' "$MUTED" "$((i + 1))" "$RESET" "${TITLES[$i]}" "$color" "$result" "$RESET"
done

printf '\n %srepo%s  %s\n' "$MUTED" "$RESET" "$DEMO"
printf ' %stry%s   %scd %s && git checkout feature/math && %s/bin/picky explain%s\n\n' "$MUTED" "$RESET" "$IRIS" "$DEMO" "$PICKY_HOME" "$RESET"
