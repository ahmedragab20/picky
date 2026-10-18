// Minimal shell-word handling. picky only rewrites simple commands; anything with
// pipes, redirects, substitutions or globs returns null and runs unchanged.

const ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;
const UNSAFE = new Set(['|', '&', ';', '<', '>', '(', ')', '`', '$', '*', '?', '[', '{', '~']);

export function tokenize(line: string): string[] | null {
  const out: string[] = [];
  let cur = '';
  let inWord = false;
  let i = 0;
  while (i < line.length) {
    const c = line[i];
    if (c === '\\') {
      if (i + 1 >= line.length) return null; // line continuation
      cur += line[i + 1];
      inWord = true;
      i += 2;
    } else if (c === "'") {
      const end = line.indexOf("'", i + 1);
      if (end < 0) return null;
      cur += line.slice(i + 1, end);
      inWord = true;
      i = end + 1;
    } else if (c === '"') {
      let j = i + 1;
      while (j < line.length && line[j] !== '"') {
        if (line[j] === '$' || line[j] === '`') return null;
        if (line[j] === '\\' && j + 1 < line.length) j++;
        cur += line[j];
        j++;
      }
      if (j >= line.length) return null;
      inWord = true;
      i = j + 1;
    } else if (c === ' ' || c === '\t') {
      if (inWord) out.push(cur);
      cur = '';
      inWord = false;
      i++;
    } else if (c === '#' && !inWord) {
      break; // comment, including the `# picky` marker
    } else if (UNSAFE.has(c)) {
      return null;
    } else {
      cur += c;
      inWord = true;
      i++;
    }
  }
  if (inWord) out.push(cur);
  return out;
}

export function quote(word: string): string {
  return /^[A-Za-z0-9_/.,:=@%+-]+$/.test(word) ? word : `'${word.replace(/'/g, `'\\''`)}'`;
}

/** Quotes argv for bash, keeping leading `NAME=value` words as assignments. */
export function quoteArgv(argv: string[]): string {
  let inPrefix = true;
  return argv
    .map((word) => {
      if (inPrefix && ASSIGNMENT.test(word)) {
        const eq = word.indexOf('=');
        return word.slice(0, eq + 1) + quote(word.slice(eq + 1));
      }
      inPrefix = false;
      return quote(word);
    })
    .join(' ');
}

/** Splits leading `NAME=value` words from the command. */
export function splitAssignments(argv: string[]): { env: Record<string, string>; rest: string[] } {
  const env: Record<string, string> = {};
  let i = 0;
  while (i < argv.length && ASSIGNMENT.test(argv[i])) {
    const eq = argv[i].indexOf('=');
    env[argv[i].slice(0, eq)] = argv[i].slice(eq + 1);
    i++;
  }
  return { env, rest: argv.slice(i) };
}
