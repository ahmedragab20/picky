import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'bun:test';
import { changedFiles, resolveRange } from '../src/git.ts';
import { cleanEnv, repo, sh, SHOP, writeFiles } from './helpers.ts';

describe('resolveRange on a pull_request merge commit', () => {
  it('diffs against the merge commit first parent, even when the event base sha is stale', () => {
    // feature changes math.ts; main later gains its own commit; GitHub merges feature into the new main.
    const root = repo(SHOP, { 'src/math.ts': 'export const add = (a: number, b: number) => b + a;\n' });
    const git = (...args: string[]) => sh(root, 'git', ['-c', 'user.name=t', '-c', 'user.email=t@t', ...args]).trim();
    const staleBase = git('rev-parse', 'main');
    const prHead = git('rev-parse', 'feature');
    git('checkout', '-q', 'main');
    writeFiles(root, { 'README.md': '# moved on\n' });
    git('commit', '-qam', 'main moves on');
    git('checkout', '-q', '--detach');
    git('merge', '-q', '--no-ff', '-m', 'merge', 'feature');

    const event = path.join(root, '.git', 'event.json');
    writeFileSync(event, JSON.stringify({ pull_request: { base: { sha: staleBase }, head: { sha: prHead } } }));
    const range = resolveRange(root, cleanEnv({ GITHUB_EVENT_PATH: event }))!;

    expect(range.via).toBe('pull request merge commit (first parent)');
    expect(changedFiles(root, range).map((c) => c.path)).toEqual(['src/math.ts']);
  });
});
