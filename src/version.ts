import { readFileSync } from 'node:fs';

/** Set by the bundle build; read from package.json when running from source. */
declare const __PICKY_VERSION__: string | undefined;

export const VERSION: string =
  typeof __PICKY_VERSION__ === 'string'
    ? __PICKY_VERSION__
    : JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version;
