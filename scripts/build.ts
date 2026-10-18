import { readFileSync } from 'node:fs';
import type { BunPlugin } from 'bun';

/** Inlines the hook script, which `picky init` writes into repos. */
const inlineAssets: BunPlugin = {
  name: 'inline-assets',
  setup(b) {
    b.onLoad({ filter: /src[\\/]assets\.ts$/ }, () => ({
      contents: `export function hookScript() { return ${JSON.stringify(readFileSync('hook/hook.sh', 'utf8'))}; }`,
      loader: 'js',
    }));
  },
};

/**
 * Bun bakes the build machine's path into `__filename` and `__dirname` in CommonJS modules.
 * typescript probes `__filename` to tell whether the file system is case sensitive, so point
 * it at the bundle itself: the banner defines these from `import.meta.url`.
 */
const bundlePaths: BunPlugin = {
  name: 'bundle-paths',
  setup(b) {
    b.onLoad({ filter: /typescript[\\/]lib[\\/]typescript\.js$/ }, ({ path }) => ({
      contents: readFileSync(path, 'utf8').replace(/\b__(file|dir)name\b/g, '__picky_$1name'),
      loader: 'js',
    }));
  },
};

// One ESM file that runs on Node 20+. `check` type checks src/cli.ts and everything it imports
// first, against src/tsconfig.json: nothing is written if that fails.
const result = await Bun.build({
  entrypoints: ['src/cli.ts'],
  outdir: 'dist',
  naming: 'picky.mjs',
  target: 'node',
  format: 'esm',
  minify: true,
  check: true,
  throw: false,
  plugins: [inlineAssets, bundlePaths],
  define: { __PICKY_VERSION__: JSON.stringify(JSON.parse(readFileSync('package.json', 'utf8')).version) },
  banner: [
    "import { fileURLToPath as __picky_fileURLToPath } from 'node:url';",
    "import { dirname as __picky_dirnameOf } from 'node:path';",
    'const __picky_filename = __picky_fileURLToPath(import.meta.url);',
    'const __picky_dirname = __picky_dirnameOf(__picky_filename);',
  ].join(' '),
});

for (const log of result.logs) console.error(log);
if (!result.success) process.exit(1);
