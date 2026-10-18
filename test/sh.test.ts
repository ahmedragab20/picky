import { describe, expect, it } from 'bun:test';
import { quote, quoteArgv, splitAssignments, tokenize } from '../src/sh.ts';

describe('tokenize', () => {
  it('splits words and handles quotes and escapes', () => {
    expect(tokenize(`npx vitest run --reporter='dot' "a b" c\\ d`)).toEqual(['npx', 'vitest', 'run', '--reporter=dot', 'a b', 'c d']);
  });

  it('drops a trailing comment such as the picky marker', () => {
    expect(tokenize('npm test   # picky')).toEqual(['npm', 'test']);
  });

  it.each([
    ['pipes', 'npm test | tee out'],
    ['and-lists', 'npm ci && npm test'],
    ['redirects', 'npm test > log'],
    ['substitution', 'vitest $(cat files)'],
    ['variables in double quotes', 'vitest "$FILES"'],
    ['globs', 'vitest src/*.test.ts'],
    ['line continuations', 'vitest run \\'],
    ['unterminated quotes', "vitest 'oops"],
  ])('refuses %s', (_, line) => {
    expect(tokenize(line)).toBeNull();
  });
});

describe('quoting', () => {
  it('leaves plain words alone and single-quotes the rest', () => {
    expect(quote('test/a.test.ts')).toBe('test/a.test.ts');
    expect(quote("it's here")).toBe(`'it'\\''s here'`);
  });

  it('keeps leading assignments as assignments', () => {
    expect(quoteArgv(['CI=a b', 'npx', 'vitest'])).toBe("CI='a b' npx vitest");
    expect(splitAssignments(['A=1', 'B=2', 'jest', 'X=3'])).toEqual({ env: { A: '1', B: '2' }, rest: ['jest', 'X=3'] });
  });
});
