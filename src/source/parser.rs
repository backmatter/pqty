#[derive(Debug, Clone)]
pub(crate) struct ParsedCommand {
    pub(crate) name: String,
    pub(crate) options: Vec<String>,
    pub(crate) required: Vec<String>,
}

/// Commands whose arguments we consume as data. Everything else is scanned
/// through (so nested commands like `\textbf{\input{x}}` are still found).
fn is_known_command(name: &str) -> bool {
    matches!(
        name,
        "documentclass"
            | "LoadClass"
            | "usepackage"
            | "RequirePackage"
            | "input"
            | "include"
            | "IfFileExists"
            | "InputIfFileExists"
            | "includeonly"
            | "addbibresource"
            | "bibliography"
            | "bibliographystyle"
            | "includegraphics"
    )
}

/// Environments whose bodies are verbatim and must not be scanned for commands.
fn is_verbatim_env(name: &str) -> bool {
    matches!(
        name.trim(),
        "verbatim"
            | "verbatim*"
            | "lstlisting"
            | "lstlisting*"
            | "minted"
            | "comment"
            | "alltt"
            | "Verbatim"
            | "Verbatim*"
            | "BVerbatim"
    )
}

/// Byte offsets where each line starts, for offset → line-number lookup.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_at(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&start| start <= offset)
}

/// Whole-file command scan: tracks line numbers, honors comments, skips verbatim
/// environments and `\verb`, and reads arguments across line breaks (but not
/// across a blank line). Returns `(line, command)` for each known command.
pub(crate) fn scan_commands(text: &str) -> Vec<(usize, ParsedCommand)> {
    let starts = line_starts(text);
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut commands = Vec::new();
    let mut i = 0;
    while i < n {
        match bytes[i] {
            b'%' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\\' => {
                let name_start = i + 1;
                let mut j = name_start;
                while j < n && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == name_start {
                    // Escaped control symbol (\%, \{, \\, ...): skip the symbol.
                    i = (name_start + 1).min(n);
                    continue;
                }
                let name = &text[name_start..j];
                if name == "verb" {
                    i = skip_verb(bytes, j, n);
                    continue;
                }
                if name == "begin" {
                    let cursor = skip_gaps(text, j);
                    if let Some((env, next)) = read_group(text, cursor, '{', '}') {
                        i = if is_verbatim_env(&env) {
                            find_end_env(text, next, env.trim()).unwrap_or(next)
                        } else {
                            next
                        };
                    } else {
                        i = j;
                    }
                    continue;
                }
                if is_known_command(name) {
                    let line = line_at(&starts, i);
                    let (options, required, end) = parse_args(text, j, name);
                    commands.push((
                        line,
                        ParsedCommand {
                            name: name.to_string(),
                            options,
                            required,
                        },
                    ));
                    i = end.max(j);
                } else {
                    // Unknown command: keep scanning after the name so nested
                    // commands inside its arguments are still discovered.
                    i = j;
                }
            }
            _ => i += 1,
        }
    }
    commands
}

/// Parse a known command's optional `[...]` and required `{...}` arguments from
/// `start`, allowing whitespace/comments (but not a blank line) between groups.
fn parse_args(text: &str, start: usize, name: &str) -> (Vec<String>, Vec<String>, usize) {
    let mut cursor = skip_gaps(text, start);
    let mut options = Vec::new();
    while text[cursor..].starts_with('[') {
        if let Some((value, next)) = read_group(text, cursor, '[', ']') {
            options.extend(split_names(&value));
            cursor = skip_gaps(text, next);
        } else {
            break;
        }
    }
    let mut required = Vec::new();
    while text[cursor..].starts_with('{') {
        if let Some((value, next)) = read_group(text, cursor, '{', '}') {
            required.push(value);
            cursor = skip_gaps(text, next);
        } else {
            break;
        }
    }
    if required.is_empty()
        && (name == "input" || name == "include")
        && let Some((value, next)) = read_bare_argument(text, cursor)
    {
        required.push(value);
        cursor = next;
    }
    (options, required, cursor)
}

/// Skip spaces, single newlines, and comments — but stop at a blank line, so an
/// argument scan never reaches across a paragraph break.
fn skip_gaps(text: &str, mut position: usize) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut newlines = 0;
    loop {
        while position < n && matches!(bytes[position], b' ' | b'\t' | b'\r') {
            position += 1;
        }
        if position < n && bytes[position] == b'\n' {
            newlines += 1;
            if newlines >= 2 {
                return position;
            }
            position += 1;
            continue;
        }
        if position < n && bytes[position] == b'%' {
            while position < n && bytes[position] != b'\n' {
                position += 1;
            }
            continue;
        }
        return position;
    }
}

/// Skip a `\verb<delim>...<delim>` (and `\verb*`) span; verb does not cross lines.
fn skip_verb(bytes: &[u8], mut position: usize, n: usize) -> usize {
    if position < n && bytes[position] == b'*' {
        position += 1;
    }
    if position >= n {
        return position;
    }
    let delim = bytes[position];
    position += 1;
    while position < n && bytes[position] != delim && bytes[position] != b'\n' {
        position += 1;
    }
    if position < n && bytes[position] == delim {
        position += 1;
    }
    position
}

/// Find the offset just past `\end{env}`, allowing whitespace before the group.
fn find_end_env(text: &str, from: usize, env: &str) -> Option<usize> {
    let mut search = from;
    while let Some(rel) = text[search..].find("\\end") {
        let after = search + rel + "\\end".len();
        let cursor = skip_gaps(text, after);
        if let Some((found, next)) = read_group(text, cursor, '{', '}')
            && found.trim() == env
        {
            return Some(next);
        }
        search = after;
    }
    None
}

fn skip_spaces(text: &str, mut position: usize) -> usize {
    while text
        .as_bytes()
        .get(position)
        .is_some_and(u8::is_ascii_whitespace)
    {
        position += 1;
    }
    position
}

fn read_group(text: &str, start: usize, open: char, close: char) -> Option<(String, usize)> {
    let mut depth = 0usize;
    let mut escaped = false;
    let mut body_start = None;
    for (offset, ch) in text[start..].char_indices() {
        let index = start + offset;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == open {
            if depth == 0 {
                body_start = Some(index + ch.len_utf8());
            }
            depth += 1;
            continue;
        }
        if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                let body_start = body_start?;
                return Some((
                    text[body_start..index].trim().to_string(),
                    index + ch.len_utf8(),
                ));
            }
        }
    }
    None
}

fn read_bare_argument(text: &str, start: usize) -> Option<(String, usize)> {
    let start = skip_spaces(text, start);
    let mut end = start;
    while text
        .as_bytes()
        .get(end)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        end += 1;
    }
    (end > start).then(|| (text[start..end].to_string(), end))
}

pub(crate) fn split_names(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .collect()
}
