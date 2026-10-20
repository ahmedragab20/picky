//! Minimal shell-word handling. picky only rewrites simple commands; anything with
//! pipes, redirects, substitutions or globs returns None and runs unchanged.

use indexmap::IndexMap;

const UNSAFE: &[char] = &['|', '&', ';', '<', '>', '(', ')', '`', '$', '*', '?', '[', '{', '~'];

/// `NAME=value`: true when the word starts with an assignment.
pub fn is_assignment(word: &str) -> bool {
    let b = word.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return false;
    }
    let name = b.iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
    b.get(name) == Some(&b'=')
}

pub fn tokenize(line: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if i + 1 >= chars.len() {
                return None; // line continuation
            }
            cur.push(chars[i + 1]);
            in_word = true;
            i += 2;
        } else if c == '\'' {
            let end = (i + 1..chars.len()).find(|&j| chars[j] == '\'')?;
            cur.extend(&chars[i + 1..end]);
            in_word = true;
            i = end + 1;
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '$' || chars[j] == '`' {
                    return None;
                }
                if chars[j] == '\\' && j + 1 < chars.len() {
                    j += 1;
                }
                cur.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() {
                return None;
            }
            in_word = true;
            i = j + 1;
        } else if c == ' ' || c == '\t' {
            if in_word {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            in_word = false;
            i += 1;
        } else if c == '#' && !in_word {
            break; // comment, including the `# picky` marker
        } else if UNSAFE.contains(&c) {
            return None;
        } else {
            cur.push(c);
            in_word = true;
            i += 1;
        }
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

pub fn quote(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_/.,:=@%+-".contains(c));
    if plain { word.to_string() } else { format!("'{}'", word.replace('\'', "'\\''")) }
}

/// Quotes argv for bash, keeping leading `NAME=value` words as assignments.
pub fn quote_argv<S: AsRef<str>>(argv: &[S]) -> String {
    let mut in_prefix = true;
    argv.iter()
        .map(|w| {
            let word = w.as_ref();
            if in_prefix && is_assignment(word) {
                let eq = word.find('=').unwrap();
                return format!("{}{}", &word[..=eq], quote(&word[eq + 1..]));
            }
            in_prefix = false;
            quote(word)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Leading `NAME=value` words, split from the command.
pub struct Assignments {
    pub env: IndexMap<String, String>,
    pub rest: Vec<String>,
}

pub fn split_assignments<S: AsRef<str>>(argv: &[S]) -> Assignments {
    let mut env = IndexMap::new();
    let mut i = 0;
    while i < argv.len() && is_assignment(argv[i].as_ref()) {
        let word = argv[i].as_ref();
        let eq = word.find('=').unwrap();
        env.insert(word[..eq].to_string(), word[eq + 1..].to_string());
        i += 1;
    }
    Assignments { env, rest: argv[i..].iter().map(|s| s.as_ref().to_string()).collect() }
}
