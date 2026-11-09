//! Code highlighting via syntect, mapped onto the theme's palette (so code
//! blocks follow the terminal's colors rather than a fixed TextMate theme).

use std::sync::OnceLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    Constant,
    Punct,
}

static SYNTAX: OnceLock<SyntaxSet> = OnceLock::new();

pub fn syntaxes() -> &'static SyntaxSet {
    SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Load the syntax set off the main thread so the first code block is instant.
pub fn prewarm() {
    std::thread::spawn(|| {
        syntaxes();
    });
}

fn alias(lang: &str) -> &str {
    match lang {
        "py" | "python3" => "python",
        "js" | "javascript" | "jsx" | "mjs" | "ts" | "typescript" | "tsx" => "js",
        "sh" | "bash" | "zsh" | "shell" | "console" => "sh",
        "rs" => "rust",
        "yml" => "yaml",
        "md" | "markdown" => "md",
        "c++" | "cc" | "hpp" => "cpp",
        "golang" => "go",
        "rb" => "ruby",
        "kt" => "kotlin",
        "cs" | "csharp" => "cs",
        "htm" => "html",
        "dockerfile" => "Dockerfile",
        "make" | "makefile" => "Makefile",
        "tex" | "latex" => "tex",
        "jsonc" | "json5" => "json",
        "toml" => "ini",
        other => other,
    }
}

fn classify(stack: &[Scope], names: &mut ScopeNames) -> Tok {
    for s in stack.iter().rev() {
        let n = names.name(*s);
        let t = if n.starts_with("comment") {
            Tok::Comment
        } else if n.starts_with("string") || n.starts_with("markup.raw") {
            Tok::String
        } else if n.starts_with("constant.numeric") {
            Tok::Number
        } else if n.starts_with("constant.character.escape") {
            Tok::Constant
        } else if n.starts_with("constant") || n.starts_with("support.constant") || n.starts_with("variable.language") {
            Tok::Constant
        } else if n.starts_with("entity.name.function") || n.starts_with("support.function") || n.starts_with("variable.function") || n.starts_with("meta.function-call") && n.contains("name") {
            Tok::Function
        } else if n.starts_with("entity.name.type") || n.starts_with("entity.name.class") || n.starts_with("support.type") || n.starts_with("support.class") || n.starts_with("entity.other.inherited-class") || n.starts_with("entity.other.attribute-name") {
            Tok::Type
        } else if n.starts_with("keyword.operator") || n.starts_with("punctuation") {
            Tok::Punct
        } else if n.starts_with("keyword") || n.starts_with("storage") || n.starts_with("entity.name.tag") {
            Tok::Keyword
        } else if n.starts_with("markup.heading") || n.starts_with("markup.bold") {
            Tok::Keyword
        } else {
            continue;
        };
        return t;
    }
    Tok::Plain
}

struct ScopeNames(std::collections::HashMap<Scope, String>);
impl ScopeNames {
    fn name(&mut self, s: Scope) -> &str {
        self.0.entry(s).or_insert_with(|| s.build_string())
    }
}

/// Per-line token ranges (byte offsets) for `code` in language `lang`.
pub fn highlight(lang: &str, code: &str) -> Vec<Vec<(usize, usize, Tok)>> {
    let ss = syntaxes();
    let lang = alias(lang.trim());
    let syn = if lang.is_empty() { None } else { ss.find_syntax_by_token(lang).or_else(|| ss.find_syntax_by_extension(lang)) };
    let Some(syn) = syn else {
        return code.split('\n').map(|_| vec![]).collect();
    };
    let mut state = ParseState::new(syn);
    let mut stack = ScopeStack::new();
    let mut names = ScopeNames(Default::default());
    let mut out = vec![];
    for line in code.split('\n') {
        let mut l = String::with_capacity(line.len() + 1);
        l.push_str(line);
        l.push('\n');
        let ops = state.parse_line(&l, ss).unwrap_or_default();
        let mut toks = vec![];
        let mut pos = 0usize;
        let push = |a: usize, b: usize, t: Tok, toks: &mut Vec<(usize, usize, Tok)>| {
            let b = b.min(line.len());
            if a < b && t != Tok::Plain {
                toks.push((a, b, t));
            }
        };
        for (off, op) in ops {
            let t = classify(stack.as_slice(), &mut names);
            push(pos, off, t, &mut toks);
            pos = off;
            let _ = stack.apply(&op);
        }
        let t = classify(stack.as_slice(), &mut names);
        push(pos, line.len(), t, &mut toks);
        out.push(toks);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rust_and_python() {
        let h = highlight("rust", "fn main() { let x = 42; // hi\n}");
        let l0 = &h[0];
        assert!(l0.iter().any(|t| t.2 == Tok::Keyword));
        assert!(l0.iter().any(|t| t.2 == Tok::Number));
        assert!(l0.iter().any(|t| t.2 == Tok::Comment));
        let h = highlight("py", "def f(x):\n    return 'a'");
        assert!(h[0].iter().any(|t| t.2 == Tok::Function));
        assert!(h[1].iter().any(|t| t.2 == Tok::String));
        assert_eq!(highlight("nonexistent-lang", "a\nb").len(), 2);
    }
}
