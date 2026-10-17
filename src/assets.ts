import { readFileSync } from 'node:fs';

/** The step hook that `picky init` installs. The build inlines it: the bundle ships without the repo's files. */
export function hookScript(): string {
  return readFileSync(new URL('../hook/hook.sh', import.meta.url), 'utf8');
}
