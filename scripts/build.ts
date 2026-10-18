import { readFileSync } from 'node:fs';
import { build, type Plugin } from 'esbuild';

/** Inlines the hook script, which `picky init` writes into repos. */
const inlineAssets: Plugin = {
  name: 'inline-assets',
  setup(b) {
    b.onLoad({ filter: /src[\\/]assets\.ts$/ }, () => ({
      contents: `export function hookScript() { return ${JSON.stringify(readFileSync('hook/hook.sh', 'utf8'))}; }`,
      loader: 'js',
    }));
  },
};

// One ESM file that runs on Node 20+. Bundled CommonJS deps (typescript) still expect
// require, __filename and __dirname, so the banner provides them.
await build({
  entryPoints: ['src/cli.ts'],
  outfile: 'dist/picky.mjs',
  bundle: true,
  minify: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  legalComments: 'none',
  plugins: [inlineAssets],
  define: { __PICKY_VERSION__: JSON.stringify(JSON.parse(readFileSync('package.json', 'utf8')).version) },
  banner: {
    js: [
      "import { createRequire as __cr } from 'node:module';",
      "import { fileURLToPath as __fu } from 'node:url';",
      "import { dirname as __dn } from 'node:path';",
      'const require = __cr(import.meta.url);',
      'const __filename = __fu(import.meta.url);',
      'const __dirname = __dn(__filename);',
    ].join(' '),
  },
});
