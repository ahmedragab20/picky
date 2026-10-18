import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

const HOME = path.resolve(import.meta.dirname, '..');
const { version } = JSON.parse(readFileSync(path.join(HOME, 'package.json'), 'utf8'));

describe('picky --version', () => {
  it.each(['--version', '-v'])('prints the package version for %s', (flag) => {
    const res = spawnSync(path.join(HOME, 'bin/picky'), [flag], { encoding: 'utf8' });
    expect(res.status).toBe(0);
    expect(res.stdout).toBe(`${version}\n`);
  });
});
