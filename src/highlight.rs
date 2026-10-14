//! Syntax colours for the file preview. Each language gets a small lexer
//! that colours one line at a time and carries what's still open (a block
//! comment, a string, a tag, a code fence) on to the next line. Close
//! enough to read code by, without a parser or a grammar library.

use std::path::Path;

/// What a stretch of a line is, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Function,
    /// true, None, nil, ALL_CAPS names.
    Constant,
    /// Rust macros and attributes, decorators, YAML anchors, shell options.
    Macro,
    /// Object keys and properties: JSON, YAML, TOML, CSS, HTML attributes.
    Key,
    Tag,
    /// `$name` in the shell.
    Variable,
    Heading,
}

/// A coloured stretch of a line, in bytes. Whatever no span covers is plain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    Go,
    JavaScript,
    TypeScript,
    Shell,
    Css,
    Html,
    Json,
    Yaml,
    Toml,
    Dockerfile,
    Markdown,
}

impl Lang {
    /// The language of a file, from its name, else from a `#!` line.
    pub fn detect(path: &Path, first_line: &str) -> Option<Lang> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        let by_name = match name.as_str() {
            "dockerfile" | "containerfile" => Some(Lang::Dockerfile),
            "cargo.lock" | "pipfile" | "poetry.lock" | "uv.lock" => Some(Lang::Toml),
            ".bashrc" | ".bash_profile" | ".bash_aliases" | ".profile" | ".zshrc" | ".zprofile" | ".zshenv"
            | ".envrc" => Some(Lang::Shell),
            _ if name.starts_with("dockerfile.") || name.ends_with(".dockerfile") => Some(Lang::Dockerfile),
            _ => None,
        };
        let by_extension = || name.rsplit_once('.').and_then(|(_, ext)| Lang::named(ext));
        by_name.or_else(by_extension).or_else(|| shebang(first_line))
    }

    /// A language by a name or file extension, as code fences give them.
    pub fn named(name: &str) -> Option<Lang> {
        Some(match name.to_ascii_lowercase().as_str() {
            "rs" | "rust" => Lang::Rust,
            "py" | "pyi" | "pyw" | "python" | "python3" => Lang::Python,
            "go" | "golang" => Lang::Go,
            "js" | "mjs" | "cjs" | "jsx" | "javascript" | "node" => Lang::JavaScript,
            "ts" | "mts" | "cts" | "tsx" | "typescript" => Lang::TypeScript,
            "sh" | "bash" | "zsh" | "ksh" | "dash" | "shell" | "console" | "shellscript" => Lang::Shell,
            "css" | "scss" | "less" => Lang::Css,
            "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "svelte" => Lang::Html,
            "json" | "jsonc" | "json5" | "geojson" | "webmanifest" => Lang::Json,
            "yaml" | "yml" => Lang::Yaml,
            "toml" => Lang::Toml,
            "dockerfile" | "docker" | "containerfile" => Lang::Dockerfile,
            "md" | "markdown" | "mdx" => Lang::Markdown,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "Rust",
            Lang::Python => "Python",
            Lang::Go => "Go",
            Lang::JavaScript => "JavaScript",
            Lang::TypeScript => "TypeScript",
            Lang::Shell => "shell",
            Lang::Css => "CSS",
            Lang::Html => "HTML",
            Lang::Json => "JSON",
            Lang::Yaml => "YAML",
            Lang::Toml => "TOML",
            Lang::Dockerfile => "Dockerfile",
            Lang::Markdown => "Markdown",
        }
    }
}

/// `#!/usr/bin/env python3` and the like.
fn shebang(first_line: &str) -> Option<Lang> {
    let command = first_line.strip_prefix("#!")?;
    let mut words = command.split_whitespace();
    let mut program = words.next()?.rsplit('/').next()?;
    if program == "env" {
        program = words.find(|w| !w.starts_with('-'))?;
    }
    match program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
        "python" => Some(Lang::Python),
        "node" | "deno" | "bun" => Some(Lang::JavaScript),
        "bash" | "sh" | "zsh" | "ksh" | "dash" => Some(Lang::Shell),
        _ => None,
    }
}

/// What one line leaves open for the next.
#[derive(Clone, Debug)]
pub struct State {
    /// Inside a block comment.
    comment: bool,
    /// Inside a string that runs on: how it closes, and whether a
    /// backslash escapes.
    string: Option<(String, bool)>,
    /// CSS: how deep in braces, and whether a declaration starts next.
    depth: u32,
    statement: bool,
    /// HTML: inside a tag, among its attributes.
    tag: bool,
    /// Dockerfile: the last line ended in a backslash, in a RUN or another
    /// instruction.
    continued: Option<bool>,
    /// Markdown: inside a fenced code block, in this language.
    fence: Option<(Option<Lang>, Box<State>)>,
}

impl Default for State {
    fn default() -> Self {
        Self { comment: false, string: None, depth: 0, statement: true, tag: false, continued: None, fence: None }
    }
}

/// Colours every line of a file.
pub fn highlight(lang: Lang, lines: &[String]) -> Vec<Vec<Span>> {
    let mut state = State::default();
    lines.iter().map(|line| line_spans(lang, line, &mut state)).collect()
}

pub fn line_spans(lang: Lang, line: &str, state: &mut State) -> Vec<Span> {
    let mut out = Vec::new();
    match lang {
        Lang::Rust => code(&RUST, line, 0, state, &mut out),
        Lang::Python => code(&PYTHON, line, 0, state, &mut out),
        Lang::Go => code(&GO, line, 0, state, &mut out),
        Lang::JavaScript => code(&JAVASCRIPT, line, 0, state, &mut out),
        Lang::TypeScript => code(&TYPESCRIPT, line, 0, state, &mut out),
        Lang::Shell => code(&SHELL, line, 0, state, &mut out),
        Lang::Json => code(&JSON, line, 0, state, &mut out),
        Lang::Css => css(line, state, &mut out),
        Lang::Html => html(line, state, &mut out),
        Lang::Yaml => yaml(line, &mut out),
        Lang::Toml => toml(line, state, &mut out),
        Lang::Dockerfile => dockerfile(line, state, &mut out),
        Lang::Markdown => markdown(line, state, &mut out),
    }
    out
}

// ── The pieces every lexer uses ─────────────────────────────────────────

fn push(out: &mut Vec<Span>, start: usize, end: usize, kind: Kind) {
    if start >= end {
        return;
    }
    match out.last_mut() {
        Some(last) if last.end == start && last.kind == kind => last.end = end,
        _ => out.push(Span { start, end, kind }),
    }
}

fn at(line: &str, i: usize) -> Option<char> {
    line.get(i..)?.chars().next()
}

fn before(line: &str, i: usize) -> Option<char> {
    line[..i].chars().next_back()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_end(line: &str, mut i: usize, dashes: bool) -> usize {
    while let Some(c) = at(line, i) {
        let dash = dashes && c == '-' && at(line, i + 1).is_some_and(is_word);
        if !is_word(c) && !dash {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// A number from `i`: digits, letters (0x1f, 1e9, 10u32, 12px), `_`, a
/// decimal point, and the `extra` characters (dates, percentages).
fn number_end(line: &str, mut i: usize, extra: &str) -> usize {
    let hex = line[i..].starts_with("0x") || line[i..].starts_with("0X");
    while let Some(c) = at(line, i) {
        let point = c == '.' && at(line, i + 1).is_some_and(|d| d.is_ascii_digit());
        let sign = matches!(c, '-' | '+') && !hex && matches!(before(line, i), Some('e' | 'E'));
        if !(c.is_ascii_alphanumeric() || c == '_' || point || sign || extra.contains(c)) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// How a string ends: its closing delimiter, whether a backslash escapes,
/// and whether it can run on over lines.
#[derive(Clone, Copy)]
struct Close<'a> {
    close: &'a str,
    escapes: bool,
    runs_on: bool,
}

/// Colours a string from `start` to where it closes after `body`, or to
/// the end of the line, where one that runs on stays open. Returns where
/// it ended.
fn string(line: &str, start: usize, body: usize, how: Close, state: &mut State, out: &mut Vec<Span>) -> usize {
    let Close { close, escapes, runs_on } = how;
    let mut i = body;
    while let Some(c) = at(line, i) {
        if escapes && c == '\\' {
            i += 1;
            i += at(line, i).map_or(0, char::len_utf8);
            continue;
        }
        if line[i..].starts_with(close) {
            push(out, start, i + close.len(), Kind::String);
            return i + close.len();
        }
        i += c.len_utf8();
    }
    push(out, start, line.len(), Kind::String);
    if runs_on {
        state.string = Some((close.to_owned(), escapes));
    }
    line.len()
}

/// A word's colour, from what it is and what follows it.
fn classify(syntax: &Syntax, word: &str, after: &str) -> Option<Kind> {
    if syntax.keywords.contains(&word) {
        return Some(Kind::Keyword);
    }
    if syntax.constants.contains(&word) {
        return Some(Kind::Constant);
    }
    if syntax.types.contains(&word) {
        return Some(Kind::Type);
    }
    let call = after.starts_with('(');
    let shouting = word.len() > 1 && word.chars().all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_');
    if shouting && word.chars().any(char::is_alphabetic) {
        return Some(Kind::Constant);
    }
    // Foo(…) builds a Foo, except in Go, where it calls an exported function.
    let capital = word.chars().next().is_some_and(char::is_uppercase);
    if syntax.capitals && capital && !(call && syntax.exported_calls) {
        return Some(Kind::Type);
    }
    call.then_some(Kind::Function)
}

// ── Programming languages ───────────────────────────────────────────────

/// How a C-like language looks, for `code`.
struct Syntax {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// String delimiters, longest first: open, close, whether a backslash
    /// escapes, whether the string can run over lines.
    quotes: &'static [(&'static str, &'static str, bool, bool)],
    /// Letters that may come right before a string: f"…", b'…'.
    string_prefixes: &'static str,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    constants: &'static [&'static str],
    /// Capitalized names are types.
    capitals: bool,
    /// Go: a capitalized name called like a function is one.
    exported_calls: bool,
    /// Rust: `name!` is a macro, `#[…]` an attribute, `'a` a lifetime and
    /// `r#"…"#` a raw string.
    rust: bool,
    /// `@name` is a decorator.
    decorators: bool,
    /// Shell: `$name` is a variable, `-x` an option, and names can have
    /// dashes.
    shell: bool,
    /// Shell: the first word of a command names a program.
    commands: bool,
    /// A string followed by `:` is a key (JSON).
    keys: bool,
}

const RUST: Syntax = Syntax {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &[("\"", "\"", true, true)],
    string_prefixes: "bc",
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for",
        "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
        "static", "struct", "super", "trait", "type", "union", "unsafe", "use", "where", "while", "yield",
    ],
    types: &[
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool",
        "char", "str",
    ],
    constants: &["true", "false"],
    capitals: true,
    exported_calls: false,
    rust: true,
    decorators: false,
    shell: false,
    commands: false,
    keys: false,
};

const PYTHON: Syntax = Syntax {
    line_comments: &["#"],
    block_comment: None,
    quotes: &[("\"\"\"", "\"\"\"", true, true), ("'''", "'''", true, true), ("\"", "\"", true, false), ("'", "'", true, false)],
    string_prefixes: "rbfuRBFU",
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal",
        "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "self", "cls",
    ],
    types: &["int", "str", "float", "bool", "bytes", "list", "dict", "set", "tuple", "object"],
    constants: &["True", "False", "None"],
    capitals: true,
    exported_calls: false,
    rust: false,
    decorators: true,
    shell: false,
    commands: false,
    keys: false,
};

const GO: Syntax = Syntax {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &[("\"", "\"", true, false), ("`", "`", false, true), ("'", "'", true, false)],
    string_prefixes: "",
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
        "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch",
        "type", "var",
    ],
    types: &[
        "any", "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8", "int16",
        "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr",
    ],
    constants: &["true", "false", "nil", "iota"],
    capitals: true,
    exported_calls: true,
    rust: false,
    decorators: false,
    shell: false,
    commands: false,
    keys: false,
};

const JS_KEYWORDS: [&str; 39] = [
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
    "else", "export", "extends", "finally", "for", "from", "function", "get", "if", "import", "in", "instanceof",
    "let", "new", "of", "return", "set", "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void",
    "while",
];

const JAVASCRIPT: Syntax = Syntax {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &[("`", "`", true, true), ("\"", "\"", true, false), ("'", "'", true, false)],
    string_prefixes: "",
    keywords: &JS_KEYWORDS,
    types: &[],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    capitals: true,
    exported_calls: false,
    rust: false,
    decorators: true,
    shell: false,
    commands: false,
    keys: false,
};

const TYPESCRIPT: Syntax = Syntax {
    keywords: &[
        "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
        "do", "else", "export", "extends", "finally", "for", "from", "function", "get", "if", "import", "in",
        "instanceof", "let", "new", "of", "return", "set", "static", "super", "switch", "this", "throw", "try",
        "typeof", "var", "void", "while", "abstract", "as", "declare", "enum", "implements", "infer", "interface",
        "is", "keyof", "namespace", "private", "protected", "public", "readonly", "satisfies", "type",
    ],
    types: &["string", "number", "boolean", "any", "unknown", "never", "object", "symbol", "bigint"],
    ..JAVASCRIPT
};

const SHELL: Syntax = Syntax {
    line_comments: &["#"],
    block_comment: None,
    quotes: &[("\"", "\"", true, true), ("'", "'", false, true)],
    string_prefixes: "",
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "function", "in",
        "select", "return", "exit", "export", "local", "readonly", "declare", "unset", "shift", "break", "continue",
        "source", "alias", "eval", "exec", "set", "trap", "time",
    ],
    types: &[],
    constants: &["true", "false"],
    capitals: false,
    exported_calls: false,
    rust: false,
    decorators: false,
    shell: true,
    commands: true,
    keys: false,
};

/// Shell words that aren't commands: Dockerfile instructions other than RUN.
const SHELL_WORDS: Syntax = Syntax { commands: false, ..SHELL };

const JSON: Syntax = Syntax {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &[("\"", "\"", true, false)],
    string_prefixes: "",
    keywords: &[],
    types: &[],
    constants: &["true", "false", "null"],
    capitals: false,
    exported_calls: false,
    rust: false,
    decorators: false,
    shell: false,
    commands: false,
    keys: true,
};

/// Values in YAML's `[…]` and `{…}`, and after TOML's `=`.
const VALUES: Syntax = Syntax {
    line_comments: &["#"],
    block_comment: None,
    quotes: &[("\"\"\"", "\"\"\"", true, true), ("'''", "'''", false, true), ("\"", "\"", true, false), ("'", "'", false, false)],
    string_prefixes: "",
    keywords: &[],
    types: &[],
    constants: &["true", "false", "null", "inf", "nan", "yes", "no", "on", "off"],
    capitals: false,
    exported_calls: false,
    rust: false,
    decorators: false,
    shell: false,
    commands: false,
    keys: true,
};

/// The general lexer, from byte `from` of the line.
fn code(syntax: &Syntax, line: &str, from: usize, state: &mut State, out: &mut Vec<Span>) {
    let mut i = from;
    if state.comment {
        let close = syntax.block_comment.map_or("*/", |(_, close)| close);
        match line[i..].find(close) {
            Some(end) => {
                push(out, i, i + end + close.len(), Kind::Comment);
                i += end + close.len();
                state.comment = false;
            }
            None => return push(out, i, line.len(), Kind::Comment),
        }
    }
    if let Some((close, escapes)) = state.string.take() {
        i = string(line, i, i, Close { close: &close, escapes, runs_on: true }, state, out);
    }
    // What the next word is, after `fn`, `class`, `|`…
    let mut next_word = None;
    let mut command = syntax.commands;
    while let Some(c) = at(line, i) {
        let rest = &line[i..];
        let prev = before(line, i);
        let starts_word = prev.is_none_or(|p| !is_word(p));
        let comment = syntax.line_comments.iter().any(|lc| rest.starts_with(lc));
        if comment && (!syntax.shell || prev.is_none_or(char::is_whitespace)) {
            return push(out, i, line.len(), Kind::Comment);
        }
        if let Some((open, close)) = syntax.block_comment
            && rest.starts_with(open)
        {
            match rest[open.len()..].find(close) {
                Some(end) => {
                    let end = i + open.len() + end + close.len();
                    push(out, i, end, Kind::Comment);
                    i = end;
                    continue;
                }
                None => {
                    state.comment = true;
                    return push(out, i, line.len(), Kind::Comment);
                }
            }
        }
        // Strings, with their prefixes (f"…", b"…", r#"…"#).
        let prefix = if starts_word { rest.chars().take_while(|p| syntax.string_prefixes.contains(*p)).count().min(2) } else { 0 };
        if syntax.rust && starts_word && let Some((body, close)) = raw_string(rest) {
            i = string(line, i, i + body, Close { close: &close, escapes: false, runs_on: true }, state, out);
            command = false;
            continue;
        }
        if let Some(&(open, close, escapes, runs_on)) =
            syntax.quotes.iter().find(|(open, ..)| rest[prefix..].starts_with(open)).filter(|_| prefix == 0 || starts_word)
        {
            let start = i;
            i = string(line, start, i + prefix + open.len(), Close { close, escapes, runs_on }, state, out);
            if syntax.keys && line[i..].trim_start().starts_with(':') && let Some(last) = out.last_mut() {
                last.kind = Kind::Key;
            }
            command = false;
            continue;
        }
        if syntax.rust && c == '\'' {
            i = rust_quote(line, i, out);
            continue;
        }
        if c.is_ascii_digit() && starts_word {
            let end = number_end(line, i, "");
            push(out, i, end, Kind::Number);
            i = end;
            command = false;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = word_end(line, i, syntax.shell);
            let word = &line[i..end];
            let after = &line[end..];
            // A member (x.match) is no keyword, and nor is a piece of a shell
            // word (/usr/local/bin, build-$target).
            let member = prev == Some('.') && !syntax.shell;
            let piece = syntax.shell && prev.is_some_and(|p| !p.is_whitespace() && !";|&(`!{".contains(p));
            let mut kind = if piece {
                None
            } else if member {
                after.starts_with('(').then_some(Kind::Function)
            } else {
                classify(syntax, word, after)
            };
            let mut end = end;
            if syntax.rust && after.starts_with('!') && !after.starts_with("!=") && kind.is_none_or(|k| k == Kind::Type) {
                kind = Some(Kind::Macro);
                end += 1;
            } else if kind.is_none() || kind == Some(Kind::Type) {
                kind = next_word.or(kind);
            }
            if syntax.keys && after.trim_start().starts_with('=') && !after.trim_start().starts_with("==") {
                kind = Some(Kind::Key);
            }
            // The first word of a shell command names a program.
            if command && kind.is_none() && !after.starts_with('=') {
                kind = Some(Kind::Function);
            }
            next_word = match word {
                "fn" | "def" | "func" | "function" => Some(Kind::Function),
                "class" | "struct" | "enum" | "trait" | "interface" | "type" | "union" => Some(Kind::Type),
                _ => None,
            };
            if let Some(kind) = kind {
                push(out, i, end, kind);
            }
            command = syntax.commands && matches!(word, "then" | "do" | "else" | "time" | "exec" | "sudo" | "if" | "while" | "until");
            i = end;
            continue;
        }
        if syntax.decorators && c == '@' && at(line, i + 1).is_some_and(char::is_alphabetic) {
            let end = word_end(line, i + 1, false);
            push(out, i, end, Kind::Macro);
            i = end;
            continue;
        }
        if syntax.rust && (rest.starts_with("#[") || rest.starts_with("#![")) {
            let end = rest.find(']').map_or(line.len(), |end| i + end + 1);
            push(out, i, end, Kind::Macro);
            i = end;
            continue;
        }
        if syntax.shell && c == '$' {
            let end = shell_variable(line, i);
            push(out, i, end, Kind::Variable);
            i = end.max(i + 1);
            command = false;
            continue;
        }
        if syntax.shell && c == '-' && prev.is_none_or(char::is_whitespace) && at(line, i + 1).is_some_and(|n| is_word(n) || n == '-') {
            let end = line[i..].find(|ch: char| ch.is_whitespace() || ch == '=').map_or(line.len(), |end| i + end);
            push(out, i, end, Kind::Macro);
            i = end;
            command = false;
            continue;
        }
        if syntax.commands {
            if matches!(c, '|' | ';' | '&' | '(' | '`') {
                command = true;
            } else if !c.is_whitespace() {
                command = false;
            }
        }
        if !c.is_whitespace() && !is_word(c) && c != '.' && c != ':' {
            next_word = None;
        }
        i += c.len_utf8();
    }
}

/// Rust's `r"…"`, `r#"…"#` and `br#"…"#`: where the body starts and the
/// closing delimiter.
fn raw_string(rest: &str) -> Option<(usize, String)> {
    let after_r = rest.strip_prefix("br").or_else(|| rest.strip_prefix('r'))?;
    let hashes = after_r.chars().take_while(|&c| c == '#').count();
    after_r[hashes..].starts_with('"').then(|| (rest.len() - after_r.len() + hashes + 1, format!("\"{}", "#".repeat(hashes))))
}

/// A Rust `'`: a character ('x', '\n') or a lifetime ('a).
fn rust_quote(line: &str, i: usize, out: &mut Vec<Span>) -> usize {
    let mut chars = line[i + 1..].char_indices();
    match (chars.next(), chars.next()) {
        (Some((_, '\\')), _) => {
            let end = line[i + 2..].find('\'').map_or(line.len(), |end| i + 2 + end + 1);
            push(out, i, end, Kind::String);
            end
        }
        (Some((_, c)), Some((at_quote, '\''))) if c != '\'' => {
            let end = i + 1 + at_quote + 1;
            push(out, i, end, Kind::String);
            end
        }
        (Some((_, c)), _) if c.is_alphabetic() || c == '_' => {
            let end = word_end(line, i + 1, false);
            push(out, i, end, Kind::Keyword);
            end
        }
        _ => i + 1,
    }
}

/// `$name`, `$1`, `$@`, `${…}`.
fn shell_variable(line: &str, i: usize) -> usize {
    match at(line, i + 1) {
        Some('{') => line[i..].find('}').map_or(line.len(), |end| i + end + 1),
        Some(c) if c.is_alphabetic() || c == '_' => word_end(line, i + 1, false),
        Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => i + 2,
        _ => i,
    }
}

// ── Markup and data ─────────────────────────────────────────────────────

fn css(line: &str, state: &mut State, out: &mut Vec<Span>) {
    let mut i = 0;
    if state.comment {
        match line.find("*/") {
            Some(end) => {
                push(out, 0, end + 2, Kind::Comment);
                i = end + 2;
                state.comment = false;
            }
            None => return push(out, 0, line.len(), Kind::Comment),
        }
    }
    while let Some(c) = at(line, i) {
        let rest = &line[i..];
        let inside = state.depth > 0;
        if let Some(body) = rest.strip_prefix("/*") {
            match body.find("*/") {
                Some(end) => {
                    push(out, i, i + 2 + end + 2, Kind::Comment);
                    i += end + 4;
                    continue;
                }
                None => {
                    state.comment = true;
                    return push(out, i, line.len(), Kind::Comment);
                }
            }
        }
        if rest.starts_with("//") && before(line, i).is_none_or(char::is_whitespace) {
            return push(out, i, line.len(), Kind::Comment);
        }
        match c {
            '"' | '\'' => {
                let close = &rest[..1];
                i = string(line, i, i + 1, Close { close, escapes: true, runs_on: false }, state, out);
                continue;
            }
            '{' => {
                state.depth += 1;
                state.statement = true;
            }
            '}' => {
                state.depth = state.depth.saturating_sub(1);
                state.statement = true;
            }
            ';' => state.statement = true,
            '@' => {
                let end = word_end(line, i + 1, true);
                push(out, i, end, Kind::Keyword);
                i = end;
                continue;
            }
            '!' if rest[1..].trim_start().starts_with("important") => {
                let end = i + rest.find("important").unwrap_or(0) + "important".len();
                push(out, i, end, Kind::Keyword);
                i = end;
                continue;
            }
            // Colours in values, ids in selectors.
            '#' if at(line, i + 1).is_some_and(|n| n.is_ascii_alphanumeric() || n == '-' || n == '_') => {
                let end = word_end(line, i + 1, true);
                push(out, i, end, if inside && !state.statement { Kind::Number } else { Kind::Type });
                i = end;
                continue;
            }
            '.' if at(line, i + 1).is_some_and(|n| n.is_alphabetic() || n == '_' || n == '-') && (!inside || state.statement) => {
                let end = word_end(line, i + 1, true);
                push(out, i, end, Kind::Type);
                i = end;
                continue;
            }
            ':' if inside && !state.statement => {}
            ':' if at(line, i + 1).is_some_and(|n| n.is_alphabetic() || n == ':') => {
                let start = i;
                i += line[i..].chars().take_while(|&ch| ch == ':').count();
                let end = word_end(line, i, true);
                push(out, start, end, Kind::Keyword);
                i = end;
                continue;
            }
            '-' | '.' | '0'..='9' if starts_number(line, i) => {
                let end = number_end(line, i + usize::from(c == '-'), "%");
                push(out, i, end, Kind::Number);
                i = end;
                continue;
            }
            c if c.is_alphabetic() || c == '_' || c == '-' => {
                let end = word_end(line, i, true);
                let after = line[end..].trim_start();
                let kind = if inside && state.statement && after.starts_with(':') && !after.starts_with("::") && !line[end..].contains('{') {
                    state.statement = false;
                    Some(Kind::Key)
                } else if inside && !state.statement {
                    after.starts_with('(').then_some(Kind::Function)
                } else {
                    Some(Kind::Tag)
                };
                if let Some(kind) = kind {
                    push(out, i, end, kind);
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += c.len_utf8();
    }
}

/// A number starts at `i`: a digit, or a sign or point before one.
fn starts_number(line: &str, i: usize) -> bool {
    let digit_at = |j| at(line, j).is_some_and(|c: char| c.is_ascii_digit());
    match at(line, i) {
        Some('0'..='9') => before(line, i).is_none_or(|p| !is_word(p) && p != '-' && p != '#'),
        Some('-' | '.') => digit_at(i + 1) && before(line, i).is_none_or(|p| !is_word(p)),
        _ => false,
    }
}

fn html(line: &str, state: &mut State, out: &mut Vec<Span>) {
    let mut i = 0;
    while i < line.len() {
        if state.comment {
            match line[i..].find("-->") {
                Some(end) => {
                    push(out, i, i + end + 3, Kind::Comment);
                    i += end + 3;
                    state.comment = false;
                }
                None => return push(out, i, line.len(), Kind::Comment),
            }
            continue;
        }
        if state.tag {
            i = attributes(line, i, state, out);
            continue;
        }
        let Some(open) = line[i..].find(['<', '&']).map(|at| i + at) else { break };
        let rest = &line[open..];
        if rest.starts_with('&') {
            let end = rest.find(';').filter(|&end| end < 12 && rest[1..end].chars().all(|c| c.is_alphanumeric() || c == '#'));
            match end {
                Some(end) => {
                    push(out, open, open + end + 1, Kind::Constant);
                    i = open + end + 1;
                }
                None => i = open + 1,
            }
        } else if rest.starts_with("<!--") {
            state.comment = true;
            i = open;
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = rest.find('>').map_or(line.len(), |end| open + end + 1);
            push(out, open, end, Kind::Keyword);
            i = end;
        } else if at(line, open + 1).is_some_and(|c| c.is_alphabetic() || c == '/') {
            let name = open + 1 + usize::from(rest.starts_with("</"));
            let end = word_end(line, name, true);
            let end = line[end..].find(|c: char| !(is_word(c) || c == ':' || c == '.' || c == '-')).map_or(line.len(), |e| end + e);
            push(out, open, end, Kind::Tag);
            state.tag = true;
            i = end;
        } else {
            i = open + 1;
        }
    }
}

/// A tag's attributes, up to and with its `>`.
fn attributes(line: &str, mut i: usize, state: &mut State, out: &mut Vec<Span>) -> usize {
    while let Some(c) = at(line, i) {
        if c == '>' || line[i..].starts_with("/>") {
            let end = i + if c == '>' { 1 } else { 2 };
            push(out, i, end, Kind::Tag);
            state.tag = false;
            return end;
        }
        if c == '"' || c == '\'' {
            let close = &line[i..i + 1];
            i = string(line, i, i + 1, Close { close, escapes: false, runs_on: false }, state, out);
            continue;
        }
        if !c.is_whitespace() && c != '=' {
            let end = line[i..].find(|ch: char| ch.is_whitespace() || "=>\"'/".contains(ch)).map_or(line.len(), |e| i + e).max(i + c.len_utf8());
            push(out, i, end, Kind::Key);
            i = end;
            continue;
        }
        i += c.len_utf8();
    }
    line.len()
}

fn yaml(line: &str, out: &mut Vec<Span>) {
    let mut i = line.len() - line.trim_start().len();
    let rest = &line[i..];
    if rest.starts_with('#') {
        return push(out, i, line.len(), Kind::Comment);
    }
    if rest == "---" || rest == "..." || rest.starts_with("--- ") {
        push(out, i, i + 3, Kind::Keyword);
        return yaml_value(line, i + 3, out);
    }
    // List items, then a key.
    while line[i..].starts_with("- ") || &line[i..] == "-" {
        i = (i + 1 + line[i + 1..].len() - line[i + 1..].trim_start().len()).min(line.len());
    }
    if let Some(colon) = yaml_key(line, i) {
        push(out, i, colon, Kind::Key);
        i = colon + 1;
    }
    yaml_value(line, i, out);
}

/// Where the `:` ends a key starting at `i`, if one does.
fn yaml_key(line: &str, i: usize) -> Option<usize> {
    let rest = &line[i..];
    let body = match rest.chars().next()? {
        quote @ ('"' | '\'') => i + 1 + rest[1..].find(quote)? + 1,
        '[' | '{' | '&' | '*' | '!' | '|' | '>' | '#' => return None,
        _ => i,
    };
    let mut j = body;
    while let Some(c) = at(line, j) {
        if c == ':' && at(line, j + 1).is_none_or(char::is_whitespace) {
            return Some(j);
        }
        if c == '#' && before(line, j).is_some_and(char::is_whitespace) {
            return None;
        }
        j += c.len_utf8();
    }
    None
}

fn yaml_value(line: &str, from: usize, out: &mut Vec<Span>) {
    let i = from + line[from..].len() - line[from..].trim_start().len();
    let rest = &line[i..];
    let Some(first) = rest.chars().next() else { return };
    match first {
        '#' => push(out, i, line.len(), Kind::Comment),
        '&' | '*' | '!' => {
            let end = rest.find(char::is_whitespace).map_or(line.len(), |e| i + e);
            push(out, i, end, Kind::Macro);
            yaml_value(line, end, out);
        }
        '|' | '>' => push(out, i, i + 1, Kind::Keyword),
        '"' | '\'' | '[' | '{' => code(&VALUES, line, i, &mut State::default(), out),
        _ => {
            // A plain value, up to a comment.
            let end = rest.find(" #").map_or(line.len(), |e| i + e);
            let value = line[i..end].trim_end();
            let kind = if VALUES.constants.contains(&value.to_ascii_lowercase().as_str()) || value == "~" {
                Kind::Constant
            } else if value.parse::<f64>().is_ok() {
                Kind::Number
            } else {
                Kind::String
            };
            push(out, i, i + value.len(), kind);
            if end < line.len() {
                push(out, end + 1, line.len(), Kind::Comment);
            }
        }
    }
}

fn toml(line: &str, state: &mut State, out: &mut Vec<Span>) {
    if state.string.is_some() {
        return code(&VALUES, line, 0, state, out);
    }
    let i = line.len() - line.trim_start().len();
    let rest = &line[i..];
    if rest.starts_with('#') {
        return push(out, i, line.len(), Kind::Comment);
    }
    if rest.starts_with('[') {
        let end = rest.find(']').map_or(line.len(), |end| i + end + 1 + usize::from(rest[end + 1..].starts_with(']')));
        push(out, i, end, Kind::Type);
        return code(&VALUES, line, end, state, out);
    }
    // key = value, the key maybe dotted or quoted.
    let mut j = i;
    let mut quote = None;
    while let Some(c) = at(line, j) {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            ('=', None) => {
                push(out, i, line[..j].trim_end().len(), Kind::Key);
                return toml_value(line, j + 1, state, out);
            }
            _ => {}
        }
        j += c.len_utf8();
    }
    code(&VALUES, line, i, state, out);
}

/// A TOML value: dates and times are numbers too.
fn toml_value(line: &str, from: usize, state: &mut State, out: &mut Vec<Span>) {
    let i = from + line[from..].len() - line[from..].trim_start().len();
    if line[i..].starts_with(|c: char| c.is_ascii_digit()) {
        let end = number_end(line, i, "-:.+");
        push(out, i, end, Kind::Number);
        return code(&VALUES, line, end, state, out);
    }
    code(&VALUES, line, i, state, out);
}

const INSTRUCTIONS: [&str; 18] = [
    "FROM", "RUN", "CMD", "LABEL", "EXPOSE", "ENV", "ADD", "COPY", "ENTRYPOINT", "VOLUME", "USER", "WORKDIR", "ARG",
    "ONBUILD", "STOPSIGNAL", "HEALTHCHECK", "SHELL", "MAINTAINER",
];

fn dockerfile(line: &str, state: &mut State, out: &mut Vec<Span>) {
    let continued = state.continued.take();
    let runs_on = line.trim_end().ends_with('\\');
    let i = line.len() - line.trim_start().len();
    if let Some(run) = continued {
        state.continued = runs_on.then_some(run);
        return code(if run { &SHELL } else { &SHELL_WORDS }, line, i, state, out);
    }
    if line[i..].starts_with('#') {
        return push(out, i, line.len(), Kind::Comment);
    }
    let end = word_end(line, i, false);
    let instruction = line[i..end].to_ascii_uppercase();
    let run = instruction == "RUN";
    state.continued = runs_on.then_some(run);
    if !INSTRUCTIONS.contains(&instruction.as_str()) {
        return code(&SHELL, line, i, state, out);
    }
    push(out, i, end, Kind::Keyword);
    if instruction != "FROM" {
        return code(if run { &SHELL } else { &SHELL_WORDS }, line, end, state, out);
    }
    // FROM [--platform=…] image [AS name]
    let mut stage = false;
    let mut j = end;
    for word in line[end..].split_whitespace() {
        let start = j + line[j..].find(word).unwrap_or(0);
        let kind = if word.starts_with("--") {
            Kind::Macro
        } else if word.eq_ignore_ascii_case("as") {
            stage = true;
            Kind::Keyword
        } else if stage {
            Kind::Type
        } else {
            Kind::String
        };
        push(out, start, start + word.len(), kind);
        j = start + word.len();
    }
}

fn markdown(line: &str, state: &mut State, out: &mut Vec<Span>) {
    let trimmed = line.trim_start();
    let i = line.len() - trimmed.len();
    let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
    if let Some((lang, inner)) = &mut state.fence {
        if fence {
            state.fence = None;
            return push(out, i, line.len(), Kind::Comment);
        }
        match lang {
            Some(lang) => out.extend(line_spans(*lang, line, inner)),
            None => push(out, 0, line.len(), Kind::String),
        }
        return;
    }
    if fence {
        let name = trimmed[3..].trim().split(|c: char| !c.is_alphanumeric()).next().unwrap_or_default();
        state.fence = Some((Lang::named(name), Box::default()));
        return push(out, i, line.len(), Kind::Comment);
    }
    if trimmed.starts_with('#') {
        return push(out, i, line.len(), Kind::Heading);
    }
    if trimmed.starts_with('>') {
        return push(out, i, line.len(), Kind::Comment);
    }
    let mut j = i;
    let marker = ["- ", "* ", "+ "].iter().any(|m| trimmed.starts_with(m));
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if marker {
        push(out, i, i + 1, Kind::Keyword);
        j += 2;
    } else if digits > 0 && trimmed[digits..].starts_with(". ") {
        push(out, i, i + digits + 1, Kind::Keyword);
        j += digits + 2;
    }
    // `code` and [links](…).
    while let Some(c) = at(line, j) {
        let rest = &line[j..];
        if c == '`' {
            let end = rest[1..].find('`').map_or(line.len(), |end| j + 1 + end + 1);
            push(out, j, end, Kind::String);
            j = end;
            continue;
        }
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(paren) = rest[close..].find(')')
        {
            push(out, j, j + close + 1, Kind::Function);
            push(out, j + close + 1, j + close + paren + 1, Kind::Comment);
            j += close + paren + 1;
            continue;
        }
        j += c.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coloured bits of `text`, line by line.
    fn colours(lang: Lang, text: &str) -> Vec<(String, Kind)> {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let spans = highlight(lang, &lines);
        lines.iter().zip(spans).flat_map(|(line, spans)| spans.into_iter().map(move |s| (line[s.start..s.end].to_owned(), s.kind))).collect()
    }

    fn has(found: &[(String, Kind)], text: &str, kind: Kind) -> bool {
        found.iter().any(|(t, k)| t == text && *k == kind)
    }

    #[test]
    fn languages_are_known_by_name_extension_or_shebang() {
        let detect = |name: &str, first: &str| Lang::detect(Path::new(name), first);
        assert_eq!(detect("src/main.rs", ""), Some(Lang::Rust));
        assert_eq!(detect("app.tsx", ""), Some(Lang::TypeScript));
        assert_eq!(detect("Dockerfile", ""), Some(Lang::Dockerfile));
        assert_eq!(detect("api.Dockerfile", ""), Some(Lang::Dockerfile));
        assert_eq!(detect("Cargo.lock", ""), Some(Lang::Toml));
        assert_eq!(detect(".zshrc", ""), Some(Lang::Shell));
        assert_eq!(detect("deploy", "#!/usr/bin/env bash"), Some(Lang::Shell));
        assert_eq!(detect("tool", "#!/usr/bin/python3.12"), Some(Lang::Python));
        assert_eq!(detect("notes.txt", ""), None);
    }

    #[test]
    fn rust() {
        let found = colours(
            Lang::Rust,
            "#[derive(Debug)]\npub fn main() -> Result<(), String> {\n    let x: u32 = 0x1f; // hex\n    println!(\"{x} \\\" done\");\n    let r = r#\"raw \"q\" \"#;\n    let c = '\\n'; f::<'a>();\n    /* block\n     still */ Some(1.5e-3)\n}",
        );
        for (text, kind) in [
            ("#[derive(Debug)]", Kind::Macro),
            ("pub", Kind::Keyword),
            ("main", Kind::Function),
            ("Result", Kind::Type),
            ("String", Kind::Type),
            ("u32", Kind::Type),
            ("0x1f", Kind::Number),
            ("// hex", Kind::Comment),
            ("println!", Kind::Macro),
            ("\"{x} \\\" done\"", Kind::String),
            ("r#\"raw \"q\" \"#", Kind::String),
            ("'\\n'", Kind::String),
            ("'a", Kind::Keyword),
            ("/* block", Kind::Comment),
            ("     still */", Kind::Comment),
            ("Some", Kind::Type),
            ("1.5e-3", Kind::Number),
        ] {
            assert!(has(&found, text, kind), "{text:?} as {kind:?} in {found:?}");
        }
    }

    #[test]
    fn python_go_and_javascript() {
        let python = colours(Lang::Python, "@cache\ndef load(path: str) -> None:\n    \"\"\"Reads\n    it.\"\"\"\n    return f'{path}' # done");
        for (text, kind) in [
            ("@cache", Kind::Macro),
            ("def", Kind::Keyword),
            ("load", Kind::Function),
            ("str", Kind::Type),
            ("None", Kind::Constant),
            ("\"\"\"Reads", Kind::String),
            ("    it.\"\"\"", Kind::String),
            ("f'{path}'", Kind::String),
            ("# done", Kind::Comment),
        ] {
            assert!(has(&python, text, kind), "{text:?} as {kind:?} in {python:?}");
        }
        let go = colours(Lang::Go, "func (s *Server) Run() error {\n\treturn fmt.Errorf(`raw\nmore`, nil)\n}");
        for (text, kind) in [("func", Kind::Keyword), ("Server", Kind::Type), ("Run", Kind::Function), ("error", Kind::Type), ("Errorf", Kind::Function), ("`raw", Kind::String), ("more`", Kind::String), ("nil", Kind::Constant)] {
            assert!(has(&go, text, kind), "{text:?} as {kind:?} in {go:?}");
        }
        let members = colours(Lang::JavaScript, "x.default.match(y)");
        assert_eq!(members, [("match".to_owned(), Kind::Function)]);
        let ts = colours(Lang::TypeScript, "export interface Props { name: string }\nconst MAX = `a ${b}`;");
        for (text, kind) in [("export", Kind::Keyword), ("interface", Kind::Keyword), ("Props", Kind::Type), ("string", Kind::Type), ("MAX", Kind::Constant), ("`a ${b}`", Kind::String)] {
            assert!(has(&ts, text, kind), "{text:?} as {kind:?} in {ts:?}");
        }
    }

    #[test]
    fn shell_and_dockerfile() {
        let shell = colours(Lang::Shell, "if [ -f \"$HOME/x\" ]; then\n  cargo build --release | tee log # build\nfi\necho ${PATH} $1");
        for (text, kind) in [
            ("if", Kind::Keyword),
            ("-f", Kind::Macro),
            ("\"$HOME/x\"", Kind::String),
            ("cargo", Kind::Function),
            ("--release", Kind::Macro),
            ("tee", Kind::Function),
            ("# build", Kind::Comment),
            ("echo", Kind::Function),
            ("${PATH}", Kind::Variable),
            ("$1", Kind::Variable),
        ] {
            assert!(has(&shell, text, kind), "{text:?} as {kind:?} in {shell:?}");
        }
        let docker = colours(Lang::Dockerfile, "# syntax\nFROM rust:1.80 AS build\nRUN apt-get update && \\\n    apt-get install -y git\nCOPY --from=build /app /usr/local/bin");
        assert!(!docker.iter().any(|(text, _)| text == "local"), "{docker:?}");
        for (text, kind) in [
            ("# syntax", Kind::Comment),
            ("FROM", Kind::Keyword),
            ("rust:1.80", Kind::String),
            ("AS", Kind::Keyword),
            ("build", Kind::Type),
            ("RUN", Kind::Keyword),
            ("apt-get", Kind::Function),
            ("-y", Kind::Macro),
            ("--from", Kind::Macro),
        ] {
            assert!(has(&docker, text, kind), "{text:?} as {kind:?} in {docker:?}");
        }
    }

    #[test]
    fn data_formats() {
        let json = colours(Lang::Json, "{\"name\": \"sasha\", \"n\": 2, \"ok\": true, \"x\": null}");
        for (text, kind) in [("\"name\"", Kind::Key), ("\"sasha\"", Kind::String), ("2", Kind::Number), ("true", Kind::Constant), ("null", Kind::Constant)] {
            assert!(has(&json, text, kind), "{text:?} as {kind:?} in {json:?}");
        }
        let yaml = colours(Lang::Yaml, "---\non:\n  push: # trigger\n    branches: [main, \"dev\"]\n- name: build\n  run: cargo test\n  retries: 3\n  base: &base\n  enabled: true");
        for (text, kind) in [
            ("---", Kind::Keyword),
            ("on", Kind::Key),
            ("push", Kind::Key),
            ("# trigger", Kind::Comment),
            ("\"dev\"", Kind::String),
            ("name", Kind::Key),
            ("build", Kind::String),
            ("cargo test", Kind::String),
            ("3", Kind::Number),
            ("&base", Kind::Macro),
            ("true", Kind::Constant),
        ] {
            assert!(has(&yaml, text, kind), "{text:?} as {kind:?} in {yaml:?}");
        }
        let toml = colours(Lang::Toml, "[package]\nname = \"sasha\" # it\nserde-json.version = '1'\nwhen = 1979-05-27T07:32:00Z\nflag = false\ndesc = \"\"\"\nlong\n\"\"\"\n[[bin]]");
        for (text, kind) in [
            ("[package]", Kind::Type),
            ("name", Kind::Key),
            ("\"sasha\"", Kind::String),
            ("# it", Kind::Comment),
            ("serde-json.version", Kind::Key),
            ("1979-05-27T07:32:00Z", Kind::Number),
            ("false", Kind::Constant),
            ("long", Kind::String),
            ("[[bin]]", Kind::Type),
        ] {
            assert!(has(&toml, text, kind), "{text:?} as {kind:?} in {toml:?}");
        }
    }

    #[test]
    fn css_and_html() {
        let css = colours(Lang::Css, "@media (max-width: 600px) {\n  .card:hover > a {\n    color: #fff !important;\n    margin: -4px 1.5em;\n    background: rgb(0 0 0); /* dark */\n  }\n}");
        for (text, kind) in [
            ("@media", Kind::Keyword),
            (".card", Kind::Type),
            (":hover", Kind::Keyword),
            ("a", Kind::Tag),
            ("color", Kind::Key),
            ("#fff", Kind::Number),
            ("!important", Kind::Keyword),
            ("-4px", Kind::Number),
            ("1.5em", Kind::Number),
            ("rgb", Kind::Function),
            ("/* dark */", Kind::Comment),
        ] {
            assert!(has(&css, text, kind), "{text:?} as {kind:?} in {css:?}");
        }
        let html = colours(Lang::Html, "<!DOCTYPE html>\n<a href=\"/x\" class='y'>Tom &amp; me</a>\n<!-- note\n-->\n<img\n  src=\"z.png\" />");
        for (text, kind) in [
            ("<!DOCTYPE html>", Kind::Keyword),
            ("<a", Kind::Tag),
            ("href", Kind::Key),
            ("\"/x\"", Kind::String),
            ("'y'", Kind::String),
            ("&amp;", Kind::Constant),
            ("</a>", Kind::Tag),
            ("<!-- note", Kind::Comment),
            ("src", Kind::Key),
            ("/>", Kind::Tag),
        ] {
            assert!(has(&html, text, kind), "{text:?} as {kind:?} in {html:?}");
        }
    }

    #[test]
    fn markdown_colours_code_fences_in_their_language() {
        let md = colours(Lang::Markdown, "# Title\n\n- run `sasha` and [docs](http://x)\n\n```rust\nfn main() {}\n```\n> quote");
        for (text, kind) in [
            ("# Title", Kind::Heading),
            ("-", Kind::Keyword),
            ("`sasha`", Kind::String),
            ("[docs]", Kind::Function),
            ("(http://x)", Kind::Comment),
            ("```rust", Kind::Comment),
            ("fn", Kind::Keyword),
            ("main", Kind::Function),
            ("> quote", Kind::Comment),
        ] {
            assert!(has(&md, text, kind), "{text:?} as {kind:?} in {md:?}");
        }
    }
}
