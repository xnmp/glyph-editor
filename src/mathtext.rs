//! LaTeX → Unicode for inline math: `$\alpha^2 \to \mathbb{R}$` → `α² → ℝ`.

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ", "epsilon" => "ϵ", "varepsilon" => "ε",
        "zeta" => "ζ", "eta" => "η", "theta" => "θ", "vartheta" => "ϑ", "iota" => "ι", "kappa" => "κ",
        "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ", "pi" => "π", "varpi" => "ϖ", "rho" => "ρ",
        "varrho" => "ϱ", "sigma" => "σ", "varsigma" => "ς", "tau" => "τ", "upsilon" => "υ", "phi" => "ϕ",
        "varphi" => "φ", "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ", "Xi" => "Ξ", "Pi" => "Π",
        "Sigma" => "Σ", "Upsilon" => "Υ", "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        "sum" => "∑", "prod" => "∏", "int" => "∫", "iint" => "∬", "oint" => "∮", "infty" => "∞",
        "partial" => "∂", "nabla" => "∇", "pm" => "±", "mp" => "∓", "times" => "×", "div" => "÷",
        "cdot" => "·", "cdots" => "⋯", "ldots" | "dots" => "…", "vdots" => "⋮", "ast" => "∗", "star" => "⋆",
        "circ" => "∘", "bullet" => "∙", "oplus" => "⊕", "otimes" => "⊗", "leq" | "le" => "≤", "geq" | "ge" => "≥",
        "neq" | "ne" => "≠", "approx" => "≈", "equiv" => "≡", "sim" => "∼", "simeq" => "≃", "cong" => "≅",
        "propto" => "∝", "ll" => "≪", "gg" => "≫", "in" => "∈", "notin" => "∉", "ni" => "∋", "subset" => "⊂",
        "supset" => "⊃", "subseteq" => "⊆", "supseteq" => "⊇", "cup" => "∪", "cap" => "∩", "setminus" => "∖",
        "emptyset" | "varnothing" => "∅", "forall" => "∀", "exists" => "∃", "neg" | "lnot" => "¬",
        "land" | "wedge" => "∧", "lor" | "vee" => "∨", "to" | "rightarrow" => "→", "leftarrow" | "gets" => "←",
        "Rightarrow" => "⇒", "Leftarrow" => "⇐", "leftrightarrow" => "↔", "Leftrightarrow" => "⇔",
        "iff" => "⟺", "implies" => "⟹", "mapsto" => "↦", "uparrow" => "↑", "downarrow" => "↓", "perp" => "⊥",
        "parallel" => "∥", "angle" => "∠", "prime" => "′", "hbar" => "ℏ", "ell" => "ℓ", "aleph" => "ℵ",
        "langle" => "⟨", "rangle" => "⟩", "lceil" => "⌈", "rceil" => "⌉", "lfloor" => "⌊", "rfloor" => "⌋",
        "quad" => "  ", "qquad" => "    ", "lim" => "lim", "sin" => "sin", "cos" => "cos", "tan" => "tan",
        "log" => "log", "ln" => "ln", "exp" => "exp", "max" => "max", "min" => "min", "sup" => "sup",
        "inf" => "inf", "det" => "det", "gcd" => "gcd", "mod" | "bmod" => "mod",
        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "limits" => "",
        _ => return None,
    })
}

fn sup(c: char) -> Option<char> {
    let (a, b) = ("0123456789+-=()nijkxabcdefghlmoprstuvwyzT∞*′", "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ⁿⁱʲᵏˣᵃᵇᶜᵈᵉᶠᵍʰˡᵐᵒᵖʳˢᵗᵘᵛʷʸᶻᵀ∞*′");
    a.chars().position(|x| x == c).and_then(|i| b.chars().nth(i))
}

pub fn sub_char(c: char) -> Option<char> {
    let (a, b) = ("0123456789+-=()aeoxhklmnpstijruvβγρφχ", "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₒₓₕₖₗₘₙₚₛₜᵢⱼᵣᵤᵥᵦᵧᵨᵩᵪ");
    a.chars().position(|x| x == c).and_then(|i| b.chars().nth(i))
}

pub fn superscript(s: &str) -> Option<String> {
    s.chars().map(sup).collect()
}

struct P<'a> {
    s: &'a [char],
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn group(&mut self) -> String {
        while self.peek() == Some(' ') {
            self.i += 1;
        }
        match self.peek() {
            Some('{') => {
                let mut depth = 0;
                let start = self.i + 1;
                let mut j = self.i;
                while j < self.s.len() {
                    match self.s[j] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner: String = self.s[start..j.min(self.s.len())].iter().collect();
                self.i = j + 1;
                convert(&inner)
            }
            Some('\\') => self.command(),
            Some(c) => {
                self.i += 1;
                c.to_string()
            }
            None => String::new(),
        }
    }
    fn command(&mut self) -> String {
        self.i += 1;
        let start = self.i;
        if let Some(c) = self.peek() {
            if !c.is_ascii_alphabetic() {
                self.i += 1;
                return match c {
                    ',' | ';' | ':' | ' ' => " ".into(),
                    '!' => String::new(),
                    '|' => "‖".into(),
                    '\\' => "; ".into(),
                    c => c.to_string(),
                };
            }
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        let name: String = self.s[start..self.i].iter().collect();
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" => {
                let (a, b) = (self.group(), self.group());
                let wrap = |x: String| if x.chars().count() == 1 || x.chars().all(|c| c.is_alphanumeric()) { x } else { format!("({x})") };
                let (a, b) = (wrap(a), wrap(b));
                if a.chars().count() == 1 && b.chars().count() == 1 { format!("{a}⁄{b}") } else { format!("{a}/{b}") }
            }
            "sqrt" => {
                let mut root = "√";
                if self.peek() == Some('[') {
                    let close = self.s[self.i..].iter().position(|&c| c == ']').map(|p| self.i + p).unwrap_or(self.s.len());
                    let idx: String = self.s[self.i + 1..close].iter().collect();
                    root = match idx.as_str() {
                        "3" => "∛",
                        "4" => "∜",
                        _ => "√",
                    };
                    self.i = close + 1;
                }
                let a = self.group();
                if a.chars().count() == 1 { format!("{root}{a}") } else { format!("{root}({a})") }
            }
            "mathbb" | "Bbb" => self
                .group()
                .chars()
                .map(|c| match c {
                    'R' => 'ℝ', 'N' => 'ℕ', 'Z' => 'ℤ', 'Q' => 'ℚ', 'C' => 'ℂ', 'P' => 'ℙ', 'H' => 'ℍ',
                    c => c,
                })
                .collect(),
            "mathcal" => self.group().chars().map(|c| if c.is_ascii_uppercase() { char::from_u32(0x1D4D0 + (c as u32 - 'A' as u32)).unwrap_or(c) } else { c }).collect(),
            "text" | "mathrm" | "mathbf" | "mathit" | "operatorname" | "textbf" | "textit" | "mathsf" | "mathtt" | "boldsymbol" => self.group(),
            "hat" | "bar" | "vec" | "dot" | "ddot" | "tilde" | "overline" => {
                let mark = match name.as_str() {
                    "hat" => '\u{302}', "bar" => '\u{304}', "vec" => '\u{20d7}', "dot" => '\u{307}',
                    "ddot" => '\u{308}', "tilde" => '\u{303}', _ => '\u{305}',
                };
                self.group().chars().flat_map(|c| [c, mark]).collect()
            }
            "begin" | "end" => {
                self.group();
                String::new()
            }
            _ => symbol(&name).map(String::from).unwrap_or_else(|| format!("\\{name}")),
        }
    }
}

fn script(text: String, f: fn(char) -> Option<char>, marker: char) -> String {
    if !text.is_empty() {
        if let Some(s) = text.chars().map(f).collect::<Option<String>>() {
            return s;
        }
    }
    if text.chars().count() == 1 { format!("{marker}{text}") } else { format!("{marker}({text})") }
}

pub fn convert(tex: &str) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut p = P { s: &chars, i: 0 };
    let mut out = String::new();
    while let Some(c) = p.peek() {
        match c {
            '\\' => {
                let r = p.command();
                out.push_str(&r);
            }
            '^' => {
                p.i += 1;
                let g = p.group();
                out.push_str(&script(g, sup, '^'));
            }
            '_' => {
                p.i += 1;
                let g = p.group();
                out.push_str(&script(g, sub_char, '_'));
            }
            '{' | '}' => p.i += 1,
            '&' | '~' => {
                p.i += 1;
                out.push(' ');
            }
            c => {
                out.push(c);
                p.i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::convert;
    #[test]
    fn conversions() {
        assert_eq!(convert(r"\alpha^2 + \beta_i"), "α² + βᵢ");
        assert_eq!(convert(r"\frac{a+b}{2}"), "(a+b)/2");
        assert_eq!(convert(r"\sum_{i=1}^{n} x_i"), "∑ᵢ₌₁ⁿ xᵢ");
        assert_eq!(convert(r"\sqrt{x^2+1}"), "√(x²+1)");
        assert_eq!(convert(r"\mathbb{R}^n \to \mathbb{R}"), "ℝⁿ → ℝ");
        assert_eq!(convert(r"e^{i\pi} = -1"), "e^(iπ) = -1");
    }
}
