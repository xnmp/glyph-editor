//! Colors, cell styles and SGR encoding.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub fn rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        }
    }
    pub fn hex(h: &str) -> Color {
        let h = h.trim_start_matches('#');
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
        Color::Rgb(p(0), p(2), p(4))
    }
    /// Mix `t` of `other` into self (falls back to `self` when not both RGB).
    pub fn blend(self, other: Color, t: f32) -> Color {
        match (self, other) {
            (Color::Rgb(a, b, c), Color::Rgb(x, y, z)) => {
                let m = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t).round().clamp(0.0, 255.0) as u8;
                Color::Rgb(m(a, x), m(b, y), m(c, z))
            }
            _ => self,
        }
    }
    pub fn is_set(self) -> bool {
        self != Color::Default
    }
}

pub const UL_NONE: u8 = 0;
pub const UL_SINGLE: u8 = 1;
pub const UL_CURLY: u8 = 3;
pub const UL_DOTTED: u8 = 4;
pub const UL_DASHED: u8 = 5;

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const STRIKE: u8 = 8;
pub const REVERSE: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub ul_color: Color,
    pub attrs: u8,
    pub underline: u8,
    /// index into the screen's hyperlink table (0 = none)
    pub link: u32,
}

impl Style {
    pub const fn new() -> Style {
        Style { fg: Color::Default, bg: Color::Default, ul_color: Color::Default, attrs: 0, underline: 0, link: 0 }
    }
    pub fn fg(c: Color) -> Style {
        Style { fg: c, ..Style::new() }
    }
    pub fn bg(c: Color) -> Style {
        Style { bg: c, ..Style::new() }
    }
    pub fn with_fg(mut self, c: Color) -> Style {
        self.fg = c;
        self
    }
    pub fn with_bg(mut self, c: Color) -> Style {
        self.bg = c;
        self
    }
    pub fn bold(mut self) -> Style {
        self.attrs |= BOLD;
        self
    }
    pub fn italic(mut self) -> Style {
        self.attrs |= ITALIC;
        self
    }
    pub fn dim(mut self) -> Style {
        self.attrs |= DIM;
        self
    }
    pub fn strike(mut self) -> Style {
        self.attrs |= STRIKE;
        self
    }
    pub fn reverse(mut self) -> Style {
        self.attrs |= REVERSE;
        self
    }
    pub fn underline(mut self, kind: u8, color: Color) -> Style {
        self.underline = kind;
        self.ul_color = color;
        self
    }
    pub fn has(&self, a: u8) -> bool {
        self.attrs & a != 0
    }
    /// Layer `o` on top of self: o's set fields win, attributes accumulate.
    pub fn over(self, o: Style) -> Style {
        Style {
            fg: if o.fg.is_set() { o.fg } else { self.fg },
            bg: if o.bg.is_set() { o.bg } else { self.bg },
            ul_color: if o.ul_color.is_set() { o.ul_color } else { self.ul_color },
            attrs: self.attrs | o.attrs,
            underline: if o.underline != 0 { o.underline } else { self.underline },
            link: if o.link != 0 { o.link } else { self.link },
        }
    }
}

fn push_color(out: &mut String, c: Color, base: u8) {
    use std::fmt::Write;
    match c {
        Color::Default => {}
        Color::Idx(i) if i < 8 => {
            let _ = write!(out, ";{}", base as u16 + i as u16);
        }
        Color::Idx(i) if i < 16 && base != 58 => {
            let _ = write!(out, ";{}", base as u16 + 60 + (i - 8) as u16);
        }
        Color::Idx(i) => {
            let _ = write!(out, ";{};5;{}", base + 8, i);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, r, g, b);
        }
    }
}

/// Full SGR sequence for `s`, starting from a reset.
pub fn sgr(s: &Style, out: &mut String) {
    out.push_str("\x1b[0");
    if s.has(BOLD) {
        out.push_str(";1");
    }
    if s.has(DIM) {
        out.push_str(";2");
    }
    if s.has(ITALIC) {
        out.push_str(";3");
    }
    match s.underline {
        0 => {}
        1 => out.push_str(";4"),
        k => {
            out.push_str(";4:");
            out.push((b'0' + k) as char);
        }
    }
    if s.has(REVERSE) {
        out.push_str(";7");
    }
    if s.has(STRIKE) {
        out.push_str(";9");
    }
    push_color(out, s.fg, 30);
    push_color(out, s.bg, 40);
    if s.underline != 0
        && let Color::Rgb(r, g, b) = s.ul_color
    {
        use std::fmt::Write;
        let _ = write!(out, ";58:2::{}:{}:{}", r, g, b);
    }
    out.push('m');
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sgr_encoding() {
        let mut o = String::new();
        sgr(&Style::fg(Color::Rgb(1, 2, 3)).bold().underline(UL_CURLY, Color::Rgb(9, 9, 9)), &mut o);
        assert_eq!(o, "\x1b[0;1;4:3;38;2;1;2;3;58:2::9:9:9m");
        o.clear();
        sgr(&Style::fg(Color::Idx(4)).with_bg(Color::Idx(9)), &mut o);
        assert_eq!(o, "\x1b[0;34;101m");
    }
    #[test]
    fn blend() {
        assert_eq!(Color::Rgb(0, 0, 0).blend(Color::Rgb(100, 200, 50), 0.5), Color::Rgb(50, 100, 25));
    }
}
