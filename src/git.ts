import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

export function git(cwd: string, args: string[]): string {
  // A container job runs as root over a workspace the runner user owns, which git refuses as
  // "dubious ownership". picky only reads the repo the job already runs code from.
  const safe = process.env.GITHUB_ACTIONS === 'true' ? ['-c', 'safe.directory=*'] : [];
  return execFileSync('git', [...safe, ...args], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function tryGit(cwd: string, args: string[]): string | null {
  try {
    return git(cwd, args).trim();
  } catch {
    return null;
  }
}

export function repoRoot(cwd: string): string {
  return git(cwd, ['rev-parse', '--show-toplevel']).trim();
}

export interface Range {
  base: string;
  head: string;
  /** How the base was found, for the summary. */
  via: string;
}

/**
 * Finds the commit to diff against. Prefers the merge base so commits on the
 * target branch don't count as changes. When only the base tip is reachable, a
 * two-tree diff over-selects, which is the safe direction.
 */
export function resolveRange(root: string, env: NodeJS.ProcessEnv): Range | null {
  const head = tryGit(root, ['rev-parse', 'HEAD']);
  if (!head) return null;

  const explicit = env.PICKY_BASE;
  if (explicit) {
    const base = tryGit(root, ['merge-base', explicit, head]) ?? tryGit(root, ['rev-parse', explicit]);
    return base ? { base, head, via: `PICKY_BASE=${explicit}` } : null;
  }

  const merge = githubMergeParent(root, head, env);
  if (merge) return { base: merge, head, via: 'pull request merge commit (first parent)' };

  const prBase = githubPullRequestBase(env);
  if (prBase) {
    const mb = tryGit(root, ['merge-base', prBase, head]);
    if (mb) return { base: mb, head, via: 'pull request merge base' };
    if (tryGit(root, ['cat-file', '-e', `${prBase}^{commit}`]) === null) {
      tryGit(root, ['fetch', '--no-tags', '--depth=1', 'origin', prBase]);
    }
    const tip = tryGit(root, ['rev-parse', `${prBase}^{commit}`]);
    return tip ? { base: tip, head, via: 'pull request base tip (shallow)' } : null;
  }

  for (const ref of ['origin/HEAD', 'origin/main', 'main', 'origin/master', 'master']) {
    const mb = tryGit(root, ['merge-base', ref, head]);
    if (mb && mb !== head) return { base: mb, head, via: `merge base with ${ref}` };
  }
  return null;
}

function pullRequest(env: NodeJS.ProcessEnv): { base?: { sha?: string }; head?: { sha?: string } } | null {
  if (!env.GITHUB_EVENT_PATH) return null;
  try {
    return JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, 'utf8')).pull_request ?? null;
  } catch {
    return null;
  }
}

function githubPullRequestBase(env: NodeJS.ProcessEnv): string | null {
  return pullRequest(env)?.base?.sha ?? null;
}

/**
 * On a pull_request event, the checkout is GitHub's merge of the PR into the current base.
 * Its first parent is that base, so diffing against it gives exactly the PR's changes, even
 * when the event's base sha is stale because the base branch moved on.
 */
function githubMergeParent(root: string, head: string, env: NodeJS.ProcessEnv): string | null {
  const prHead = pullRequest(env)?.head?.sha;
  if (!prHead) return null;
  // The raw commit keeps its parent ids even in a shallow clone, where history is cut off.
  const parents = (tryGit(root, ['cat-file', '-p', head]) ?? '').split('\n').flatMap((l) => (l.startsWith('parent ') ? [l.slice(7)] : []));
  if (parents.length !== 2 || parents[1] !== prHead) return null;
  if (tryGit(root, ['cat-file', '-e', `${parents[0]}^{commit}`]) === null) {
    tryGit(root, ['fetch', '--no-tags', '--depth=1', 'origin', parents[0]]);
  }
  return tryGit(root, ['cat-file', '-e', `${parents[0]}^{commit}`]) === null ? null : parents[0];
}

export interface Change {
  status: 'A' | 'M' | 'D' | 'R' | 'C' | 'T';
  path: string;
  /** Previous path for renames and copies. */
  from?: string;
  added: number;
  removed: number;
}

export function changedFiles(root: string, range: Range): Change[] {
  // -z keeps paths unquoted and unambiguous.
  const numstat = git(root, ['diff', '--numstat', '-z', '-M', range.base, range.head]).split('\0');
  const counts = new Map<string, [number, number]>();
  for (let i = 0; i < numstat.length; i++) {
    if (!numstat[i]) continue;
    const [a, r, file] = numstat[i].split('\t');
    // Renames print an empty path, then the old and new paths as separate fields.
    const target = file === '' ? numstat[(i += 2)] : file;
    counts.set(target, [Number(a) || 0, Number(r) || 0]);
  }

  const fields = git(root, ['diff', '--name-status', '-z', '-M', range.base, range.head]).split('\0');
  const changes: Change[] = [];
  for (let i = 0; i < fields.length && fields[i]; ) {
    const status = fields[i][0] as Change['status'];
    if (status === 'R' || status === 'C') {
      const [from, file] = [fields[i + 1], fields[i + 2]];
      const [added, removed] = counts.get(file) ?? [0, 0];
      changes.push({ status, path: file, from, added, removed });
      i += 3;
    } else {
      const file = fields[i + 1];
      const [added, removed] = counts.get(file) ?? [0, 0];
      changes.push({ status, path: file, added, removed });
      i += 2;
    }
  }
  return changes;
}

/** Zero-context unified diff for the given files. */
export function hunks(root: string, range: Range, files: string[]): string {
  if (files.length === 0) return '';
  return git(root, ['diff', '-U0', '-M', '--no-color', range.base, range.head, '--', ...files]);
}

export function trackedFiles(root: string): string[] {
  return git(root, ['ls-files', '-z']).split('\0').filter(Boolean);
}
