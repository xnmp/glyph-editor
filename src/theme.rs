//! Themes. The default "terminal" theme uses the terminal's own default
//! foreground/background and its ANSI palette; when the terminal reports its
//! colors (OSC 10/11/4) we derive subtle tints from them, so panels and
//! highlights look native in any color scheme.

use crate::style::{Color, Style, UL_DOTTED};

#[derive(Clone, Debug, Default)]
pub struct Palette {
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub ansi: [Option<(u8, u8, u8)>; 16],
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    /// Background painted for the whole screen (Default = terminal's own).
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub faint: Color,
    pub border: Color,
    pub accent: Color,
    pub panel: Color,
    pub sidebar: Color,
    pub selection: Color,
    /// neutral highlight for hovered/selected list items (Obsidian's modifier-hover)
    pub hover: Color,
    pub link: Color,
    pub link_unresolved: Color,
    pub tag_fg: Color,
    pub tag_bg: Color,
    pub code_fg: Color,
    pub mark_fg: Color,
    pub mark_bg: Color,
    pub search_bg: Color,
    pub search_cur_bg: Color,
    pub heading: [Color; 6],
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub syn_keyword: Color,
    pub syn_string: Color,
    pub syn_comment: Color,
    pub syn_number: Color,
    pub syn_function: Color,
    pub syn_type: Color,
    pub syn_constant: Color,
    pub syn_punct: Color,
    pub dark: bool,
    /// the real background color when known (for blending), else Default
    pub canvas: Color,
    /// true when tints are real RGB (can blend)
    pub rgb: bool,
}

fn rgbc(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

fn luminance(c: (u8, u8, u8)) -> f32 {
    0.2126 * c.0 as f32 + 0.7152 * c.1 as f32 + 0.0722 * c.2 as f32
}

impl Theme {
    pub fn terminal(p: &Palette) -> Theme {
        let a = |i: usize| p.ansi[i].map(rgbc).unwrap_or(Color::Idx(i as u8));
        match (p.fg, p.bg) {
            (Some(fg), Some(bg)) => {
                let (fgc, bgc) = (rgbc(fg), rgbc(bg));
                let dark = luminance(bg) < luminance(fg);
                let mix = |c: Color, t: f32| bgc.blend(c, t);
                let magenta = a(5);
                let accent = if magenta.rgb().is_some() { magenta } else { Color::Rgb(138, 92, 245) };
                let blue = a(4);
                let link = if dark { accent.blend(fgc, 0.25) } else { accent };
                Theme {
                    name: "terminal".into(),
                    bg: Color::Default,
                    fg: Color::Default,
                    muted: mix(fgc, 0.68),
                    faint: mix(fgc, 0.38),
                    border: mix(fgc, 0.18),
                    accent,
                    panel: mix(fgc, if dark { 0.06 } else { 0.045 }),
                    sidebar: mix(fgc, if dark { 0.03 } else { 0.025 }),
                    selection: mix(accent, if dark { 0.32 } else { 0.22 }),
                    hover: mix(fgc, if dark { 0.13 } else { 0.08 }),
                    link,
                    link_unresolved: mix(link, 0.55),
                    tag_fg: if dark { accent.blend(fgc, 0.45) } else { accent },
                    tag_bg: mix(accent, 0.16),
                    code_fg: a(1).blend(fgc, 0.2),
                    mark_fg: fgc,
                    mark_bg: mix(a(3), 0.35),
                    search_bg: mix(a(3), 0.30),
                    search_cur_bg: mix(a(3), 0.65),
                    // Obsidian's default: headings in the text colour, size carries the hierarchy
                    heading: [fgc, fgc, fgc, fgc, fgc, mix(fgc, 0.75)],
                    red: a(1),
                    green: a(2),
                    yellow: a(3),
                    blue,
                    magenta,
                    cyan: a(6),
                    syn_keyword: magenta,
                    syn_string: a(2),
                    syn_comment: mix(fgc, 0.45),
                    syn_number: a(3),
                    syn_function: blue,
                    syn_type: a(3).blend(fgc, 0.2),
                    syn_constant: a(6),
                    syn_punct: mix(fgc, 0.7),
                    dark,
                    canvas: bgc,
                    rgb: true,
                }
            }
            _ => Theme::ansi(),
        }
    }

    /// No color knowledge at all: ANSI indices only.
    pub fn ansi() -> Theme {
        let i = Color::Idx;
        Theme {
            name: "terminal".into(),
            bg: Color::Default,
            fg: Color::Default,
            muted: Color::Default,
            faint: i(8),
            border: i(8),
            accent: i(5),
            panel: Color::Default,
            sidebar: Color::Default,
            selection: i(8),
            hover: i(8),
            link: i(4),
            link_unresolved: i(8),
            tag_fg: i(5),
            tag_bg: Color::Default,
            code_fg: i(1),
            mark_fg: i(0),
            mark_bg: i(3),
            search_bg: i(3),
            search_cur_bg: i(11),
            heading: [Color::Default, Color::Default, Color::Default, Color::Default, Color::Default, i(8)],
            red: i(1),
            green: i(2),
            yellow: i(3),
            blue: i(4),
            magenta: i(5),
            cyan: i(6),
            syn_keyword: i(5),
            syn_string: i(2),
            syn_comment: i(8),
            syn_number: i(3),
            syn_function: i(4),
            syn_type: i(3),
            syn_constant: i(6),
            syn_punct: Color::Default,
            dark: true,
            canvas: Color::Default,
            rgb: false,
        }
    }

    /// Fixed palettes that paint their own background.
    pub fn named(name: &str) -> Option<Theme> {
        let (fg, bg, ansi): ((u8, u8, u8), (u8, u8, u8), [(u8, u8, u8); 8]) = match name {
            "dark" => (
                (218, 218, 218),
                (30, 30, 30),
                [(30, 30, 30), (233, 49, 71), (8, 185, 78), (224, 172, 0), (2, 122, 255), (138, 92, 245), (0, 191, 188), (218, 218, 218)],
            ),
            "light" => (
                (34, 34, 34),
                (255, 255, 255),
                [(0, 0, 0), (228, 55, 75), (12, 181, 79), (224, 172, 0), (8, 109, 221), (112, 93, 207), (0, 191, 188), (34, 34, 34)],
            ),
            "nord" => (
                (216, 222, 233),
                (46, 52, 64),
                [(59, 66, 82), (191, 97, 106), (163, 190, 140), (235, 203, 139), (129, 161, 193), (180, 142, 173), (136, 192, 208), (229, 233, 240)],
            ),
            "gruvbox" => (
                (235, 219, 178),
                (40, 40, 40),
                [(40, 40, 40), (251, 73, 52), (184, 187, 38), (250, 189, 47), (131, 165, 152), (211, 134, 155), (142, 192, 124), (235, 219, 178)],
            ),
            _ => return None,
        };
        let mut p = Palette { fg: Some(fg), bg: Some(bg), ..Default::default() };
        for (k, c) in ansi.iter().enumerate() {
            p.ansi[k] = Some(*c);
        }
        let mut t = Theme::terminal(&p);
        t.name = name.into();
        t.bg = rgbc(bg);
        t.fg = rgbc(fg);
        Some(t)
    }

    /// The real background if known (for "active" surfaces), else `fallback`.
    pub fn canvas_or(&self, fallback: Color) -> Color {
        if self.bg.is_set() {
            self.bg
        } else if self.canvas.is_set() {
            self.canvas
        } else {
            fallback
        }
    }
    pub fn fg_or(&self, fallback: Color) -> Color {
        if self.fg.is_set() { self.fg } else { fallback }
    }

    pub fn base(&self) -> Style {
        Style { fg: self.fg, bg: self.bg, ..Style::new() }
    }
    pub fn marker(&self) -> Style {
        Style::fg(self.faint)
    }
    pub fn heading_style(&self, level: u8) -> Style {
        Style::fg(self.heading[(level.clamp(1, 6) - 1) as usize]).bold()
    }
    pub fn link_style(&self) -> Style {
        Style::fg(self.link).underline(UL_DOTTED, self.canvas.blend(self.link, 0.6))
    }
    pub fn unresolved_style(&self) -> Style {
        Style::fg(self.link_unresolved).underline(UL_DOTTED, self.link_unresolved)
    }
    /// A soft background tint of `c`; Default when we cannot blend.
    pub fn tint(&self, c: Color, amount: f32) -> Color {
        if !self.rgb {
            return Color::Default;
        }
        self.canvas.blend(c, amount)
    }
    pub fn callout_color(&self, kind: &str) -> Color {
        match kind {
            "abstract" | "summary" | "tldr" | "tip" | "hint" | "important" => self.cyan,
            "success" | "check" | "done" => self.green,
            "question" | "help" | "faq" | "warning" | "caution" | "attention" => self.yellow,
            "failure" | "fail" | "missing" | "danger" | "error" | "bug" => self.red,
            "example" => self.magenta,
            "quote" | "cite" => self.muted,
            _ => self.blue,
        }
    }
}

pub fn callout_icon(kind: &str) -> &'static str {
    match kind {
        "note" => "✎",
        "info" => "ℹ",
        "todo" => "☑",
        "abstract" | "summary" | "tldr" => "≣",
        "tip" | "hint" | "important" => "✦",
        "success" | "check" | "done" => "✔",
        "question" | "help" | "faq" => "?",
        "warning" | "caution" | "attention" => "⚠",
        "failure" | "fail" | "missing" => "✘",
        "danger" | "error" => "⚡",
        "bug" => "✱",
        "example" => "☰",
        "quote" | "cite" => "❝",
        _ => "✎",
    }
}
