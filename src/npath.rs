//! Node's `path` module: `posix` everywhere, `win32` for Windows, and the platform functions
//! (`path.join`, `path.resolve`, ...) that pick one the way Node does.

pub mod posix {
    /// Node's `normalizeString` for `/`-separated paths.
    pub(crate) fn normalize_string(path: &str, allow_above_root: bool, sep: u8, is_sep: fn(u8) -> bool) -> String {
        let b = path.as_bytes();
        let mut res = String::new();
        let mut last_segment_length = 0usize;
        let mut last_slash: isize = -1;
        let mut dots: i32 = 0;
        let mut code = 0u8;
        let sep_str = (sep as char).to_string();
        let mut i = 0usize;
        while i <= b.len() {
            if i < b.len() {
                code = b[i];
            } else if is_sep(code) {
                break;
            } else {
                code = b'/';
            }
            if is_sep(code) {
                if last_slash == i as isize - 1 || dots == 1 {
                    // NOOP
                } else if dots == 2 {
                    let rb = res.as_bytes();
                    if res.len() < 2 || last_segment_length != 2 || rb[rb.len() - 1] != b'.' || rb[rb.len() - 2] != b'.' {
                        if res.len() > 2 {
                            match res.rfind(sep as char) {
                                None => {
                                    res.clear();
                                    last_segment_length = 0;
                                }
                                Some(idx) => {
                                    res.truncate(idx);
                                    let last = res.rfind(sep as char).map_or(-1, |x| x as isize);
                                    last_segment_length = (res.len() as isize - 1 - last) as usize;
                                }
                            }
                            last_slash = i as isize;
                            dots = 0;
                            i += 1;
                            continue;
                        } else if !res.is_empty() {
                            res.clear();
                            last_segment_length = 0;
                            last_slash = i as isize;
                            dots = 0;
                            i += 1;
                            continue;
                        }
                    }
                    if allow_above_root {
                        if res.is_empty() {
                            res.push_str("..");
                        } else {
                            res.push_str(&sep_str);
                            res.push_str("..");
                        }
                        last_segment_length = 2;
                    }
                } else {
                    let seg = &path[(last_slash + 1) as usize..i];
                    if !res.is_empty() {
                        res.push_str(&sep_str);
                    }
                    res.push_str(seg);
                    last_segment_length = (i as isize - last_slash - 1) as usize;
                }
                last_slash = i as isize;
                dots = 0;
            } else if code == b'.' && dots != -1 {
                dots += 1;
            } else {
                dots = -1;
            }
            i += 1;
        }
        res
    }

    fn is_sep(c: u8) -> bool {
        c == b'/'
    }

    pub fn normalize(path: &str) -> String {
        if path.is_empty() {
            return ".".into();
        }
        let is_absolute = path.starts_with('/');
        let trailing = path.ends_with('/');
        let mut p = normalize_string(path, !is_absolute, b'/', is_sep);
        if p.is_empty() {
            if is_absolute {
                return "/".into();
            }
            return if trailing { "./".into() } else { ".".into() };
        }
        if trailing {
            p.push('/');
        }
        if is_absolute { format!("/{p}") } else { p }
    }

    pub fn join(parts: &[&str]) -> String {
        let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return ".".into();
        }
        normalize(&parts.join("/"))
    }

    /// `path.posix.resolve(...parts)`, with `cwd` standing in for `process.cwd()`.
    pub fn resolve(parts: &[&str], cwd: &str) -> String {
        let mut resolved = String::new();
        let mut absolute = false;
        let mut i = parts.len() as isize - 1;
        while i >= -1 && !absolute {
            let p = if i >= 0 { parts[i as usize] } else { cwd };
            i -= 1;
            if p.is_empty() {
                continue;
            }
            resolved = format!("{p}/{resolved}");
            absolute = p.starts_with('/');
        }
        let resolved = normalize_string(&resolved, !absolute, b'/', is_sep);
        if absolute {
            return format!("/{resolved}");
        }
        if resolved.is_empty() { ".".into() } else { resolved }
    }

    pub fn relative(from: &str, to: &str, cwd: &str) -> String {
        if from == to {
            return String::new();
        }
        let from = resolve(&[from], cwd);
        let to = resolve(&[to], cwd);
        if from == to {
            return String::new();
        }
        let (f, t) = (from.as_bytes(), to.as_bytes());
        let from_start = 1;
        let from_end = f.len();
        let from_len = from_end - from_start;
        let to_start = 1;
        let to_len = t.len() - to_start;
        let length = from_len.min(to_len);
        let mut last_common_sep: isize = -1;
        let mut i = 0;
        while i < length {
            let c = f[from_start + i];
            if c != t[to_start + i] {
                break;
            } else if c == b'/' {
                last_common_sep = i as isize;
            }
            i += 1;
        }
        if i == length {
            if to_len > length {
                if t[to_start + i] == b'/' {
                    return to[to_start + i + 1..].to_string();
                }
                if i == 0 {
                    return to[to_start + i..].to_string();
                }
            } else if from_len > length {
                if f[from_start + i] == b'/' {
                    last_common_sep = i as isize;
                } else if i == 0 {
                    last_common_sep = 0;
                }
            }
        }
        let mut out = String::new();
        let mut i = (from_start as isize + last_common_sep + 1) as usize;
        while i <= from_end {
            if i == from_end || f[i] == b'/' {
                out.push_str(if out.is_empty() { ".." } else { "/.." });
            }
            i += 1;
        }
        format!("{out}{}", &to[(to_start as isize + last_common_sep) as usize..])
    }

    pub fn dirname(path: &str) -> String {
        let b = path.as_bytes();
        if b.is_empty() {
            return ".".into();
        }
        let has_root = b[0] == b'/';
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = b.len() - 1;
        while i >= 1 {
            if b[i] == b'/' {
                if !matched_slash {
                    end = i as isize;
                    break;
                }
            } else {
                matched_slash = false;
            }
            i -= 1;
        }
        if end == -1 {
            return if has_root { "/".into() } else { ".".into() };
        }
        if has_root && end == 1 {
            return "//".into();
        }
        path[..end as usize].to_string()
    }

    pub fn basename(path: &str, suffix: Option<&str>) -> String {
        let b = path.as_bytes();
        let mut start = 0usize;
        let mut end: isize = -1;
        let mut matched_slash = true;
        if let Some(suffix) = suffix.filter(|s| !s.is_empty() && s.len() <= path.len()) {
            if suffix == path {
                return String::new();
            }
            let sb = suffix.as_bytes();
            let mut ext_idx = sb.len() as isize - 1;
            let mut first_non_slash_end: isize = -1;
            for i in (0..b.len()).rev() {
                let code = b[i];
                if code == b'/' {
                    if !matched_slash {
                        start = i + 1;
                        break;
                    }
                } else {
                    if first_non_slash_end == -1 {
                        matched_slash = false;
                        first_non_slash_end = i as isize + 1;
                    }
                    if ext_idx >= 0 {
                        if code == sb[ext_idx as usize] {
                            ext_idx -= 1;
                            if ext_idx == -1 {
                                end = i as isize;
                            }
                        } else {
                            ext_idx = -1;
                            end = first_non_slash_end;
                        }
                    }
                }
            }
            if start as isize == end {
                end = first_non_slash_end;
            } else if end == -1 {
                end = b.len() as isize;
            }
            return slice(path, start, end.max(0) as usize);
        }
        for i in (0..b.len()).rev() {
            if b[i] == b'/' {
                if !matched_slash {
                    start = i + 1;
                    break;
                }
            } else if end == -1 {
                matched_slash = false;
                end = i as isize + 1;
            }
        }
        if end == -1 {
            return String::new();
        }
        slice(path, start, end as usize)
    }

    /// `str.slice(start, end)` on byte offsets that may not be char boundaries when a suffix
    /// matched part of a multi-byte character: JavaScript would cut the UTF-16 units the same way.
    fn slice(s: &str, start: usize, end: usize) -> String {
        if end <= start {
            return String::new();
        }
        String::from_utf8_lossy(&s.as_bytes()[start..end]).into_owned()
    }

    pub fn extname(path: &str) -> String {
        let b = path.as_bytes();
        let mut start_dot: isize = -1;
        let mut start_part = 0usize;
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut pre_dot_state = 0;
        for i in (0..b.len()).rev() {
            let code = b[i];
            if code == b'/' {
                if !matched_slash {
                    start_part = i + 1;
                    break;
                }
                continue;
            }
            if end == -1 {
                matched_slash = false;
                end = i as isize + 1;
            }
            if code == b'.' {
                if start_dot == -1 {
                    start_dot = i as isize;
                } else if pre_dot_state != 1 {
                    pre_dot_state = 1;
                }
            } else if start_dot != -1 {
                pre_dot_state = -1;
            }
        }
        if start_dot == -1
            || end == -1
            || pre_dot_state == 0
            || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part as isize + 1)
        {
            return String::new();
        }
        path[start_dot as usize..end as usize].to_string()
    }

    pub fn is_absolute(path: &str) -> bool {
        path.starts_with('/')
    }
}

/// The platform's `path.sep`.
pub const SEP: &str = if cfg!(windows) { "\\" } else { "/" };

fn cwd() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".into())
}

/// `path.join` for this platform.
pub fn join(parts: &[&str]) -> String {
    if cfg!(windows) { win32::join(parts) } else { posix::join(parts) }
}

/// `path.resolve` for this platform.
pub fn resolve(parts: &[&str]) -> String {
    if cfg!(windows) { win32::resolve(parts, &cwd()) } else { posix::resolve(parts, &cwd()) }
}

/// `path.relative` for this platform.
pub fn relative(from: &str, to: &str) -> String {
    if cfg!(windows) { win32::relative(from, to, &cwd()) } else { posix::relative(from, to, &cwd()) }
}

/// `path.dirname` for this platform.
pub fn dirname(path: &str) -> String {
    if cfg!(windows) { win32::dirname(path) } else { posix::dirname(path) }
}

/// `path.basename` for this platform.
pub fn basename(path: &str) -> String {
    if cfg!(windows) { win32::basename(path, None) } else { posix::basename(path, None) }
}

/// `path.extname` for this platform.
pub fn extname(path: &str) -> String {
    if cfg!(windows) { win32::extname(path) } else { posix::extname(path) }
}

/// `path.isAbsolute` for this platform.
pub fn is_absolute(path: &str) -> bool {
    if cfg!(windows) { win32::is_absolute(path) } else { posix::is_absolute(path) }
}

/// `p.split(path.sep).join('/')`.
pub fn to_posix(p: &str) -> String {
    if cfg!(windows) { p.replace('\\', "/") } else { p.to_string() }
}

pub mod win32 {
    //! Node's `path.win32`, for Windows.
    use super::posix::normalize_string;

    fn is_sep(c: u8) -> bool {
        c == b'/' || c == b'\\'
    }

    fn is_device_root(c: u8) -> bool {
        c.is_ascii_alphabetic()
    }

    /// `charCodeAt`: out of range is NaN in JS, which equals nothing; 0 stands in for it here.
    fn at(b: &[u8], i: usize) -> u8 {
        b.get(i).copied().unwrap_or(0)
    }

    const RESERVED: [&str; 28] = [
        "CON",
        "PRN",
        "AUX",
        "NUL",
        "COM1",
        "COM2",
        "COM3",
        "COM4",
        "COM5",
        "COM6",
        "COM7",
        "COM8",
        "COM9",
        "LPT1",
        "LPT2",
        "LPT3",
        "LPT4",
        "LPT5",
        "LPT6",
        "LPT7",
        "LPT8",
        "LPT9",
        "COM\u{b9}",
        "COM\u{b2}",
        "COM\u{b3}",
        "LPT\u{b9}",
        "LPT\u{b2}",
        "LPT\u{b3}",
    ];

    /// `isWindowsReservedName(path, colonIndex)`: `path.slice(0, colonIndex)`, upper-cased, is a
    /// device name. A colon index of -1 (None) slices off the last UTF-16 unit, as JS does.
    fn is_reserved(path: &str, colon: Option<usize>) -> bool {
        let head = match colon {
            Some(i) => &path[..i],
            None => match path.chars().last() {
                None => "",
                // Half a surrogate pair is left, which is no device name.
                Some(c) if c.len_utf16() == 2 => return false,
                Some(c) => &path[..path.len() - c.len_utf8()],
            },
        };
        let upper = head.to_uppercase();
        RESERVED.contains(&upper.as_str())
    }

    pub fn normalize(path: &str) -> String {
        let b = path.as_bytes();
        let len = b.len();
        if len == 0 {
            return ".".into();
        }
        let mut root_end = 0;
        let mut device: Option<String> = None;
        let mut is_absolute = false;
        let code = b[0];
        if path.chars().count() == 1 {
            return if code == b'/' { "\\".into() } else { path.into() };
        }
        if is_sep(code) {
            is_absolute = true;
            if is_sep(at(b, 1)) {
                let mut j = 2;
                let mut last = j;
                while j < len && !is_sep(b[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    let first_part = &path[last..j];
                    last = j;
                    while j < len && is_sep(b[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        last = j;
                        while j < len && !is_sep(b[j]) {
                            j += 1;
                        }
                        if j == len || j != last {
                            if first_part == "." || first_part == "?" {
                                device = Some(format!("\\\\{first_part}"));
                                root_end = 4;
                                // `path.slice(4, colonIndex + 1)`
                                let end = path.find(':').map_or(0, |i| i + 1);
                                let possible = if end > 4 { &path[4..end] } else { "" };
                                let colon = possible.len().checked_sub(1);
                                if is_reserved(possible, colon) {
                                    device = Some(format!("\\\\?\\{possible}"));
                                    root_end = 4 + possible.len();
                                }
                            } else if j == len {
                                return format!("\\\\{first_part}\\{}\\", &path[last..]);
                            } else {
                                device = Some(format!("\\\\{first_part}\\{}", &path[last..j]));
                                root_end = j;
                            }
                        }
                    }
                }
            } else {
                root_end = 1;
            }
        } else if let Some(colon) = path.find(':').filter(|&i| i > 0) {
            if is_device_root(code) && colon == 1 {
                device = Some(path[..2].to_string());
                root_end = 2;
                if len > 2 && is_sep(b[2]) {
                    is_absolute = true;
                    root_end = 3;
                }
            } else if is_reserved(path, Some(colon)) {
                device = Some(path[..colon + 1].to_string());
                root_end = colon + 1;
            }
        }

        let mut tail = if root_end < len { normalize_string(&path[root_end..], !is_absolute, b'\\', is_sep) } else { String::new() };
        if tail.is_empty() && !is_absolute {
            tail = ".".into();
        }
        if !tail.is_empty() && is_sep(b[len - 1]) {
            tail.push('\\');
        }
        if !is_absolute && device.is_none() && path.contains(':') {
            // Keep a relative path from turning into a drive path (CVE-2024-36139).
            let t = tail.as_bytes();
            if t.len() >= 2 && is_device_root(t[0]) && t[1] == b':' {
                return format!(".\\{tail}");
            }
            for (index, _) in path.match_indices(':') {
                if index == len - 1 || is_sep(b[index + 1]) {
                    return format!(".\\{tail}");
                }
            }
        }
        if is_reserved(path, path.find(':')) {
            return format!(".\\{}{tail}", device.unwrap_or_default());
        }
        match device {
            None if is_absolute => format!("\\{tail}"),
            None => tail,
            Some(d) if is_absolute => format!("{d}\\{tail}"),
            Some(d) => format!("{d}{tail}"),
        }
    }

    pub fn join(parts: &[&str]) -> String {
        let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
        let Some(first) = parts.first() else {
            return ".".into();
        };
        let mut joined = parts.join("\\");
        // Don't let the joined path start with two slashes, which normalize() would read as a UNC
        // root, unless the first part clearly is one (exactly two slashes and a name).
        let f = first.as_bytes();
        let mut needs_replace = true;
        let mut slash_count = 0;
        if is_sep(f[0]) {
            slash_count += 1;
            if f.len() > 1 && is_sep(f[1]) {
                slash_count += 1;
                if f.len() > 2 {
                    if is_sep(f[2]) {
                        slash_count += 1;
                    } else {
                        needs_replace = false;
                    }
                }
            }
        }
        if needs_replace {
            let jb = joined.as_bytes();
            while slash_count < jb.len() && is_sep(jb[slash_count]) {
                slash_count += 1;
            }
            if slash_count >= 2 {
                joined = format!("\\{}", &joined[slash_count..]);
            }
        }
        // Reserved device names skip normalization.
        let reserved = joined.split('\\').filter(|p| !p.is_empty()).any(|p| p.find(':').is_some_and(|i| is_reserved(p, Some(i))));
        if reserved {
            return joined.replace('/', "\\");
        }
        normalize(&joined)
    }

    /// `path.win32.resolve(...parts)`, with `cwd` standing in for `process.cwd()`.
    pub fn resolve(parts: &[&str], cwd: &str) -> String {
        let mut resolved_device = String::new();
        let mut resolved_tail = String::new();
        let mut resolved_absolute = false;
        let mut i = parts.len() as isize - 1;
        while i >= -1 {
            let path: String;
            if i >= 0 {
                path = parts[i as usize].to_string();
                i -= 1;
                if path.is_empty() {
                    continue;
                }
            } else if resolved_device.is_empty() {
                i -= 1;
                let mut p = cwd.to_string();
                let only_here = parts.len() == 1 && (parts[0].is_empty() || parts[0] == ".");
                if parts.is_empty() || (only_here && is_sep(at(p.as_bytes(), 0))) {
                    if !cfg!(windows) {
                        p = p.replace('/', "\\");
                    }
                    return p;
                }
                path = p;
            } else {
                i -= 1;
                // The drive's own working directory, or the process's when it is on that drive,
                // or else the drive's root.
                let mut p = std::env::var(format!("={resolved_device}")).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| cwd.to_string());
                let head = p.get(..2).map(str::to_lowercase);
                if head.as_deref() != Some(resolved_device.to_lowercase().as_str()) && at(p.as_bytes(), 2) == b'\\' {
                    p = format!("{resolved_device}\\");
                }
                path = p;
            }

            let b = path.as_bytes();
            let len = b.len();
            let mut root_end = 0;
            let mut device = String::new();
            let mut is_absolute = false;
            let code = at(b, 0);
            if path.chars().count() == 1 {
                if is_sep(code) {
                    root_end = 1;
                    is_absolute = true;
                }
            } else if is_sep(code) {
                is_absolute = true;
                if is_sep(at(b, 1)) {
                    let mut j = 2;
                    let mut last = j;
                    while j < len && !is_sep(b[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        let first_part = &path[last..j];
                        last = j;
                        while j < len && is_sep(b[j]) {
                            j += 1;
                        }
                        if j < len && j != last {
                            last = j;
                            while j < len && !is_sep(b[j]) {
                                j += 1;
                            }
                            if j == len || j != last {
                                if first_part != "." && first_part != "?" {
                                    device = format!("\\\\{first_part}\\{}", &path[last..j]);
                                    root_end = j;
                                } else {
                                    device = format!("\\\\{first_part}");
                                    root_end = 4;
                                }
                            }
                        }
                    }
                } else {
                    root_end = 1;
                }
            } else if is_device_root(code) && at(b, 1) == b':' {
                device = path[..2].to_string();
                root_end = 2;
                if len > 2 && is_sep(b[2]) {
                    is_absolute = true;
                    root_end = 3;
                }
            }

            if !device.is_empty() {
                if !resolved_device.is_empty() {
                    if device.to_lowercase() != resolved_device.to_lowercase() {
                        // Another drive: not part of this path.
                        continue;
                    }
                } else {
                    resolved_device = device;
                }
            }
            if resolved_absolute {
                if !resolved_device.is_empty() {
                    break;
                }
            } else {
                resolved_tail = format!("{}\\{resolved_tail}", &path[root_end..]);
                resolved_absolute = is_absolute;
                if is_absolute && !resolved_device.is_empty() {
                    break;
                }
            }
        }
        let tail = normalize_string(&resolved_tail, !resolved_absolute, b'\\', is_sep);
        if resolved_absolute {
            return format!("{resolved_device}\\{tail}");
        }
        let out = format!("{resolved_device}{tail}");
        if out.is_empty() { ".".into() } else { out }
    }

    pub fn relative(from: &str, to: &str, cwd: &str) -> String {
        if from == to {
            return String::new();
        }
        let from_orig = resolve(&[from], cwd);
        let to_orig = resolve(&[to], cwd);
        if from_orig == to_orig {
            return String::new();
        }
        let from_low = from_orig.to_lowercase();
        let to_low = to_orig.to_lowercase();
        if from_low == to_low {
            return String::new();
        }
        let units = |s: &str| s.encode_utf16().count();
        if units(&from_orig) != units(&from_low) || units(&to_orig) != units(&to_low) {
            // Lower-casing changed a length: compare part by part.
            let mut from_split: Vec<&str> = from_orig.split('\\').collect();
            let mut to_split: Vec<&str> = to_orig.split('\\').collect();
            if from_split.last() == Some(&"") {
                from_split.pop();
            }
            if to_split.last() == Some(&"") {
                to_split.pop();
            }
            let length = from_split.len().min(to_split.len());
            let i = (0..length).find(|&i| from_split[i].to_lowercase() != to_split[i].to_lowercase()).unwrap_or(length);
            if i == 0 {
                return to_orig;
            }
            if i == length {
                if to_split.len() > length {
                    return to_split[i..].join("\\");
                }
                if from_split.len() > length {
                    return "..\\".repeat(from_split.len() - 1 - i) + "..";
                }
                return String::new();
            }
            return "..\\".repeat(from_split.len() - i) + &to_split[i..].join("\\");
        }

        // Equal UTF-16 lengths: walk code units, like Node.
        let from: Vec<u16> = from_low.encode_utf16().collect();
        let to: Vec<u16> = to_low.encode_utf16().collect();
        let to_orig: Vec<u16> = to_orig.encode_utf16().collect();
        let bs = u16::from(b'\\');
        let mut from_start = 0;
        while from_start < from.len() && from[from_start] == bs {
            from_start += 1;
        }
        let mut from_end = from.len();
        while from_end > from_start + 1 && from[from_end - 1] == bs {
            from_end -= 1;
        }
        let from_len = from_end - from_start;
        let mut to_start = 0;
        while to_start < to.len() && to[to_start] == bs {
            to_start += 1;
        }
        let mut to_end = to.len();
        while to_end > to_start + 1 && to[to_end - 1] == bs {
            to_end -= 1;
        }
        let to_len = to_end - to_start;

        let length = from_len.min(to_len);
        let mut last_common_sep: isize = -1;
        let mut i = 0;
        while i < length {
            let c = from[from_start + i];
            if c != to[to_start + i] {
                break;
            } else if c == bs {
                last_common_sep = i as isize;
            }
            i += 1;
        }
        let slice = |a: usize, b: usize| String::from_utf16_lossy(&to_orig[a.min(b)..b]);
        if i != length {
            if last_common_sep == -1 {
                return String::from_utf16_lossy(&to_orig);
            }
        } else {
            if to_len > length {
                if to.get(to_start + i) == Some(&bs) {
                    // `from` is the exact base path for `to`.
                    return slice(to_start + i + 1, to_orig.len());
                }
                if i == 2 {
                    // `from` is the device root.
                    return slice(to_start + i, to_orig.len());
                }
            }
            if from_len > length {
                if from.get(from_start + i) == Some(&bs) {
                    last_common_sep = i as isize;
                } else if i == 2 {
                    last_common_sep = 3;
                }
            }
            if last_common_sep == -1 {
                last_common_sep = 0;
            }
        }
        let mut out = String::new();
        let mut k = from_start as isize + last_common_sep + 1;
        while k <= from_end as isize {
            let k_us = k as usize;
            if k_us == from_end || from[k_us] == bs {
                out.push_str(if out.is_empty() { ".." } else { "\\.." });
            }
            k += 1;
        }
        let mut to_start = to_start + last_common_sep as usize;
        if !out.is_empty() {
            return out + &slice(to_start, to_end);
        }
        if to_orig.get(to_start) == Some(&bs) {
            to_start += 1;
        }
        slice(to_start, to_end)
    }

    pub fn dirname(path: &str) -> String {
        let b = path.as_bytes();
        let len = b.len();
        if len == 0 {
            return ".".into();
        }
        let mut root_end: isize = -1;
        let mut offset = 0;
        let code = b[0];
        if path.chars().count() == 1 {
            return if is_sep(code) { path.into() } else { ".".into() };
        }
        if is_sep(code) {
            root_end = 1;
            offset = 1;
            if is_sep(at(b, 1)) {
                let mut j = 2;
                let mut last = j;
                while j < len && !is_sep(b[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && is_sep(b[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        last = j;
                        while j < len && !is_sep(b[j]) {
                            j += 1;
                        }
                        if j == len {
                            // A UNC root only.
                            return path.into();
                        }
                        if j != last {
                            root_end = j as isize + 1;
                            offset = j + 1;
                        }
                    }
                }
            }
        } else if is_device_root(code) && at(b, 1) == b':' {
            let end = if len > 2 && is_sep(b[2]) { 3 } else { 2 };
            root_end = end as isize;
            offset = end;
        }
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut i = len as isize - 1;
        while i >= offset as isize {
            if is_sep(b[i as usize]) {
                if !matched_slash {
                    end = i;
                    break;
                }
            } else {
                matched_slash = false;
            }
            i -= 1;
        }
        if end == -1 {
            if root_end == -1 {
                return ".".into();
            }
            end = root_end;
        }
        path[..end as usize].to_string()
    }

    pub fn basename(path: &str, suffix: Option<&str>) -> String {
        let b = path.as_bytes();
        let mut start = 0;
        let mut end: isize = -1;
        let mut matched_slash = true;
        if b.len() >= 2 && is_device_root(b[0]) && b[1] == b':' {
            start = 2;
        }
        if let Some(suffix) = suffix.filter(|s| !s.is_empty() && s.encode_utf16().count() <= path.encode_utf16().count()) {
            if suffix == path {
                return String::new();
            }
            let sb = suffix.as_bytes();
            let mut ext_idx = sb.len() as isize - 1;
            let mut first_non_slash_end: isize = -1;
            let mut i = b.len() as isize - 1;
            while i >= start as isize {
                let code = b[i as usize];
                if is_sep(code) {
                    if !matched_slash {
                        start = i as usize + 1;
                        break;
                    }
                } else {
                    if first_non_slash_end == -1 {
                        matched_slash = false;
                        first_non_slash_end = i + 1;
                    }
                    if ext_idx >= 0 {
                        if code == sb[ext_idx as usize] {
                            ext_idx -= 1;
                            if ext_idx == -1 {
                                end = i;
                            }
                        } else {
                            ext_idx = -1;
                            end = first_non_slash_end;
                        }
                    }
                }
                i -= 1;
            }
            if start as isize == end {
                end = first_non_slash_end;
            } else if end == -1 {
                end = b.len() as isize;
            }
            return path[start..(end.max(start as isize)) as usize].to_string();
        }
        let mut i = b.len() as isize - 1;
        while i >= start as isize {
            if is_sep(b[i as usize]) {
                if !matched_slash {
                    start = i as usize + 1;
                    break;
                }
            } else if end == -1 {
                matched_slash = false;
                end = i + 1;
            }
            i -= 1;
        }
        if end == -1 {
            return String::new();
        }
        path[start..end as usize].to_string()
    }

    pub fn extname(path: &str) -> String {
        let b = path.as_bytes();
        let mut start = 0;
        let mut start_dot: isize = -1;
        let mut start_part = 0;
        let mut end: isize = -1;
        let mut matched_slash = true;
        let mut pre_dot_state = 0;
        if b.len() >= 2 && b[1] == b':' && is_device_root(b[0]) {
            start = 2;
            start_part = 2;
        }
        let mut i = b.len() as isize - 1;
        while i >= start as isize {
            let code = b[i as usize];
            if is_sep(code) {
                if !matched_slash {
                    start_part = i as usize + 1;
                    break;
                }
                i -= 1;
                continue;
            }
            if end == -1 {
                matched_slash = false;
                end = i + 1;
            }
            if code == b'.' {
                if start_dot == -1 {
                    start_dot = i;
                } else if pre_dot_state != 1 {
                    pre_dot_state = 1;
                }
            } else if start_dot != -1 {
                pre_dot_state = -1;
            }
            i -= 1;
        }
        if start_dot == -1
            || end == -1
            || pre_dot_state == 0
            || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part as isize + 1)
        {
            return String::new();
        }
        path[start_dot as usize..end as usize].to_string()
    }

    pub fn is_absolute(path: &str) -> bool {
        let b = path.as_bytes();
        match b.first() {
            None => false,
            Some(&c) => is_sep(c) || (b.len() > 2 && is_device_root(c) && b[1] == b':' && is_sep(b[2])),
        }
    }
}

/// `os.tmpdir()`.
pub fn tmpdir() -> String {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if cfg!(windows) {
        let path = var("TEMP").or_else(|| var("TMP")).unwrap_or_else(|| {
            let root = var("SystemRoot").or_else(|| var("windir")).unwrap_or_else(|| "undefined".into());
            format!("{root}\\temp")
        });
        if path.len() > 1 && path.ends_with('\\') && !path.ends_with(":\\") {
            return path[..path.len() - 1].to_string();
        }
        return path;
    }
    let path = var("TMPDIR").or_else(|| var("TMP")).or_else(|| var("TEMP")).unwrap_or_else(|| "/tmp".into());
    if path.len() > 1 && path.ends_with('/') { path[..path.len() - 1].to_string() } else { path }
}

/// `fs.realpathSync(p)`, or None when it throws. Windows verbatim prefixes (`\\?\`) are removed,
/// as Node's paths don't have them.
pub fn realpath(p: &str) -> Option<String> {
    if p.is_empty() {
        return None;
    }
    let real = std::fs::canonicalize(p).ok()?.to_string_lossy().into_owned();
    if let Some(rest) = real.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    Some(real.strip_prefix(r"\\?\").map_or(real.clone(), str::to_string))
}

#[cfg(test)]
mod tests {
    use super::win32;

    // Expected values are Node 26's `path.win32`, with `process.cwd()` returning CWD.
    const CWD: &str = r"C:\Users\me\repo";

    #[test]
    fn win32_normalizes_and_joins_like_node() {
        assert_eq!(win32::normalize("C:/a//b/../c/"), r"C:\a\c\");
        assert_eq!(win32::normalize(r"\\server\share"), r"\\server\share\");
        assert_eq!(win32::normalize("a:b:"), "a:b:");
        assert_eq!(win32::normalize("CON:x"), r".\CON:x");
        assert_eq!(win32::join(&["//server", "share"]), r"\\server\share\");
        assert_eq!(win32::join(&[r"C:\a", r"..\b", "c.ts"]), r"C:\b\c.ts");
    }

    #[test]
    fn win32_resolves_and_relativizes_like_node() {
        assert_eq!(win32::resolve(&["src", "../lib/x.ts"], CWD), r"C:\Users\me\repo\lib\x.ts");
        assert_eq!(win32::resolve(&["D:x"], CWD), r"D:\x");
        assert_eq!(win32::resolve(&[r"\\srv\sh\a", "b"], CWD), r"\\srv\sh\a\b");
        assert_eq!(win32::relative(CWD, r"C:\Users\me\repo\src\a.ts", CWD), r"src\a.ts");
        assert_eq!(win32::relative(r"C:\a\b", r"D:\c", CWD), r"D:\c");
        assert_eq!(win32::relative(r"C:\A\b", r"c:\a\B\x", CWD), "x");
        assert_eq!(win32::relative(r"C:\a\İ", r"C:\a\x", CWD), r"..\x");
    }

    #[test]
    fn win32_splits_paths_like_node() {
        assert_eq!(win32::dirname(r"C:\a\b.ts"), r"C:\a");
        assert_eq!(win32::dirname(r"\\srv\sh\"), r"\\srv\sh\");
        assert_eq!(win32::basename(r"C:\a\b.ts\", None), "b.ts");
        assert_eq!(win32::extname("C:.git"), "");
        assert_eq!(win32::extname(r"a\b.test.ts"), ".ts");
        assert!(win32::is_absolute("C:/x") && win32::is_absolute(r"\x") && !win32::is_absolute("C:x"));
    }
}
