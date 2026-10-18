// Copies files that live at the repo root into the docs app before dev and build:
// CHANGELOG.md becomes the Changelog page, bench/results.json and bench/hero.json feed the charts, and
// package.json gives the version the install commands download. skills/picky/SKILL.md is
// served at /skill/SKILL.md for agents.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';

const root = new URL('../../', import.meta.url);
const docs = new URL('../', import.meta.url);

const changelog = readFileSync(new URL('CHANGELOG.md', root), 'utf8').replace(/^# Changelog\s*\n/, '');
writeFileSync(
  new URL('content/docs/changelog.md', docs),
  `---\ntitle: Changelog\ndescription: Every release, synced from CHANGELOG.md.\n---\n\n${changelog}`,
);

const results = JSON.parse(readFileSync(new URL('bench/results.json', root), 'utf8'));
mkdirSync(new URL('data/', docs), { recursive: true });
writeFileSync(new URL('data/benchmarks.json', docs), `${JSON.stringify(results)}\n`);
copyFileSync(new URL('bench/hero.json', root), new URL('data/hero.json', docs));

const { version } = JSON.parse(readFileSync(new URL('package.json', root), 'utf8'));
writeFileSync(new URL('data/meta.json', docs), `${JSON.stringify({ version })}\n`);

mkdirSync(new URL('public/skill/', docs), { recursive: true });
copyFileSync(new URL('skills/picky/SKILL.md', root), new URL('public/skill/SKILL.md', docs));
