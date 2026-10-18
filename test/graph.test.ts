import { describe, expect, it } from 'bun:test';
import { buildGraph, importersOfMissing, transitiveDependents } from '../src/graph.ts';
import { tempDir, writeFiles } from './helpers.ts';

function graphOf(files: Record<string, string>) {
  const dir = tempDir();
  writeFiles(dir, files);
  return buildGraph(dir, Object.keys(files));
}

describe('buildGraph', () => {
  it('resolves relative imports, .js-to-.ts, index files, re-exports and dynamic imports', () => {
    const g = graphOf({
      'src/a.ts': "import './b.js'; export * from './lib'; const c = await import('./c');",
      'src/b.ts': '',
      'src/lib/index.ts': '',
      'src/c.tsx': '',
    });
    expect([...g.imports.get('src/a.ts')!].sort()).toEqual(['src/b.ts', 'src/c.tsx', 'src/lib/index.ts']);
  });

  it('resolves tsconfig paths and workspace packages', () => {
    const g = graphOf({
      'tsconfig.json': '{ // comments are fine\n "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"] } } }',
      'src/app.ts': "import '@/util'; import '@shop/core'; import '@shop/core/money'; import 'react';",
      'src/util.ts': '',
      'packages/core/package.json': '{ "name": "@shop/core", "main": "dist/index.js" }',
      'packages/core/src/index.ts': '',
      'packages/core/money.ts': '',
    });
    expect([...g.imports.get('src/app.ts')!].sort()).toEqual(['packages/core/money.ts', 'packages/core/src/index.ts', 'src/util.ts']);
  });

  it('finds transitive dependents', () => {
    const g = graphOf({
      'src/math.ts': '',
      'src/cart.ts': "import './math.ts';",
      'test/cart.test.ts': "import '../src/cart.ts';",
      'test/other.test.ts': '',
    });
    expect([...transitiveDependents(g, ['src/math.ts'])].sort()).toEqual(['src/cart.ts', 'test/cart.test.ts']);
  });

  it('finds files that still import a removed file', () => {
    const g = graphOf({ 'test/old.test.ts': "import '../src/gone.js';" });
    expect([...importersOfMissing(g, ['src/gone.ts'])]).toEqual(['test/old.test.ts']);
  });
});
