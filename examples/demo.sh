#!/usr/bin/env bash
# Builds a small TypeScript repo, makes a few kinds of changes, and runs a CI-style
# step through the picky hook exactly the way GitHub Actions runs `run:` steps.
#
#   bash examples/demo.sh             graph-only (no API key needed)
#   PICKY_API_KEY=... bash examples/demo.sh   uses Jev for tests outside the graph
set -euo pipefail

PICKY_HOME="$(cd "$(dirname "$0")/.." && pwd)"
DEMO="$(mktemp -d "${TMPDIR:-/tmp}/picky-demo.XXXXXX")"
MODEL="none"
[ -n "${PICKY_API_KEY:-}${TYPESAFE_API_KEY:-}" ] && MODEL="jev"

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
echo "== step: npm test   # picky"
npm test --silent   # picky
STEP

run_step() {
  # GitHub Actions runs each `run:` step as: bash --noprofile --norc -eo pipefail <script>
  BASH_ENV="$PICKY_HOME/hook/hook.sh" PICKY_BIN="$PICKY_HOME/bin/picky" \
    bash --noprofile --norc -eo pipefail "$DEMO/.step.sh" 2>&1 | grep -Ev '^ℹ (tests|suites|pass|fail|cancelled|skipped|todo|duration_ms) ' || true
}

scenario() {
  echo
  echo "################################################################"
  echo "## $1"
  echo "################################################################"
}

scenario "1. Change src/math.ts (math and cart tests depend on it)"
git checkout -qb feature/math
sed -i.bak 's/a + b/b + a/' src/math.ts && rm src/math.ts.bak
git commit -qam "reorder add"
run_step

scenario "2. Docs-only change (README.md)"
git checkout -q main && git checkout -qb feature/docs
echo "Now with docs." >> README.md
git commit -qam "docs"
run_step

scenario "3. package.json changed (always a full run)"
git checkout -q main && git checkout -qb feature/deps
sed -i.bak 's/"shop"/"shop-app"/' package.json && rm package.json.bak
git commit -qam "rename package"
run_step

scenario "4. Same as 1 but in shadow mode (decides, still runs everything)"
git checkout -q feature/math
PICKY_MODE=shadow run_step

echo
echo "Demo repo kept at: $DEMO"
echo "Try:  cd $DEMO && git checkout feature/math && $PICKY_HOME/bin/picky explain"
