//! Cell canvas + diffing renderer. Emits only changed rows, wrapped in
//! synchronized-update markers. Understands wide graphemes, OSC 8 links,
//! OSC 66 multicell (scaled) text, kitty Unicode image placeholders, and
//! classic kitty placements (for terminals without placeholder support).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graphics as gfx;
use crate::style::{Style, sgr};
use crate::text::grapheme_width;
use unicode_segmentation::UnicodeSegmentation;

pub const NORMAL: u8 = 0;
pub const WIDE_TAIL: u8 = 1;
pub const COVERED: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    buf: [u8; 22],
    len: u8,
    pub flag: u8,
    pub scale: u8,
    /// OSC 66 `w` (block width in scaled cells; 0 = one block per grapheme)
    pub units: u8,
    /// fractional font scale n/d (0 = none)
    pub n: u8,
    pub d: u8,
    pub style: Style,
}

impl Cell {
    pub fn blank(style: Style) -> Cell {
        let mut buf = [0u8; 22];
        buf[0] = b' ';
        Cell { buf, len: 1, flag: NORMAL, scale: 1, units: 0, n: 0, d: 0, style }
    }
    pub fn new(text: &str, style: Style) -> Cell {
        let mut c = Cell::blank(style);
        c.set_text(text);
        c
    }
    pub fn set_text(&mut self, t: &str) {
        let mut t = t;
        if t.len() > self.buf.len() {
            // overly long cluster: keep its first char
            let end = t.char_indices().nth(1).map_or(t.len(), |(i, _)| i);
            t = &t[..end];
        }
        self.buf[..t.len()].copy_from_slice(t.as_bytes());
        self.len = t.len() as u8;
    }
    pub fn text(&self) -> &str {
        // SAFETY-free: we only ever store valid UTF-8 slices
        std::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("?")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Placement {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub cols: u32,
    pub rows: u32,
    pub src_row: u32,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// Image data that must be transmitted before it is shown.
pub struct ImageData {
    pub id: u32,
    pub png: Vec<u8>,
    pub cols: u32,
    pub rows: u32,
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    /// theme defaults: unset fg/bg in written styles inherit these / the cell's bg
    pub base: Style,
    pub cells: Vec<Cell>,
    pub placements: Vec<Placement>,
    pub images: Vec<Arc<ImageData>>,
    pub links: Vec<String>,
    pub cursor: Option<(usize, usize)>,
    /// cursor position is known but drawn by the app (typeset text)
    pub cursor_hidden: bool,
    pub cursor_shape: u8, // 2 block, 4 underline, 6 bar
    pub title: Option<String>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, bg: Style) -> Canvas {
        Canvas {
            w,
            h,
            base: bg,
            cells: vec![Cell::blank(bg); w * h],
            placements: vec![],
            images: vec![],
            links: vec![String::new()],
            cursor: None,
            cursor_hidden: false,
            cursor_shape: 6,
            title: None,
        }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> &Cell {
        &self.cells[y * self.w + x]
    }
    #[inline]
    pub fn at_mut(&mut self, x: usize, y: usize) -> &mut Cell {
        &mut self.cells[y * self.w + x]
    }

    pub fn link(&mut self, url: &str) -> u32 {
        if let Some(i) = self.links.iter().position(|l| l == url) {
            return i as u32;
        }
        self.links.push(url.to_string());
        (self.links.len() - 1) as u32
    }

    /// Width in cells of the multicell block whose owner is `c`.
    fn block_w(c: &Cell) -> usize {
        if c.units > 0 { c.scale as usize * c.units as usize } else { grapheme_width(c.text()).max(1) * c.scale as usize }
    }

    /// Writing into any cell of a multicell block destroys the whole block
    /// (that is what terminals do); blank out the rest of it so the canvas and
    /// the terminal agree.
    fn break_multicell(&mut self, x: usize, y: usize) {
        let c = self.cells[y * self.w + x];
        if c.flag != COVERED && !(c.flag == NORMAL && c.scale > 1) {
            return;
        }
        let (mut ox, mut oy) = (x, y);
        if c.flag == COVERED {
            let mut found = None;
            'search: for yy in (y.saturating_sub(7)..=y).rev() {
                for xx in (x.saturating_sub(64)..=x).rev() {
                    let o = &self.cells[yy * self.w + xx];
                    if o.flag == NORMAL && o.scale > 1 && xx + Self::block_w(o) > x && yy + o.scale as usize > y {
                        found = Some((xx, yy));
                        break 'search;
                    }
                }
            }
            match found {
                Some(p) => (ox, oy) = p,
                None => {
                    // orphaned covered cell: make it a plain blank
                    let st = c.style;
                    self.cells[y * self.w + x] = Cell::blank(st);
                    return;
                }
            }
        }
        let o = self.cells[oy * self.w + ox];
        let (bw, s) = (Self::block_w(&o), o.scale as usize);
        for yy in oy..(oy + s).min(self.h) {
            for xx in ox..(ox + bw).min(self.w) {
                let cell = &mut self.cells[yy * self.w + xx];
                if cell.flag == COVERED || (xx == ox && yy == oy) {
                    *cell = Cell::blank(Style { fg: o.style.fg, bg: o.style.bg, ..Style::new() });
                }
            }
        }
    }

    #[inline]
    fn inherit(&self, style: Style, x: usize, y: usize) -> Style {
        let mut s = style;
        if !s.bg.is_set() {
            s.bg = self.cells[y * self.w + x].style.bg;
        }
        if !s.fg.is_set() {
            s.fg = self.base.fg;
        }
        s
    }

    /// Write text at (x, y), clipped to [clip0, clip1). Returns x after text.
    pub fn put(&mut self, mut x: i32, y: i32, text: &str, style: Style, clip1: usize) -> i32 {
        if y < 0 || y as usize >= self.h {
            return x + crate::text::str_width(text) as i32;
        }
        let limit = clip1.min(self.w) as i32;
        let y = y as usize;
        for g in text.graphemes(true) {
            let mut cw = grapheme_width(g) as i32;
            let mut g = g;
            if g == "\t" {
                g = " ";
            }
            if cw == 0 {
                if g.starts_with(gfx::PLACEHOLDER) {
                    cw = 1;
                } else {
                    continue;
                }
            }
            if x < 0 {
                x += cw;
                continue;
            }
            if x + cw > limit {
                if cw == 2 && x < limit {
                    let st = self.inherit(style, x as usize, y);
                    *self.at_mut(x as usize, y) = Cell::blank(st);
                }
                return x + cw;
            }
            self.break_multicell(x as usize, y);
            if cw == 2 {
                self.break_multicell(x as usize + 1, y);
            }
            let st = self.inherit(style, x as usize, y);
            let c = self.at_mut(x as usize, y);
            *c = Cell::new(g, st);
            if cw == 2 {
                let t = self.at_mut(x as usize + 1, y);
                *t = Cell::blank(st);
                t.flag = WIDE_TAIL;
                t.len = 0;
            }
            x += cw;
        }
        x
    }

    /// Multicell text (OSC 66) at `scale`; each grapheme occupies scale×(w·scale) cells.
    pub fn put_scaled(&mut self, mut x: i32, y: i32, text: &str, style: Style, scale: u8, clip1: usize) -> i32 {
        let limit = clip1.min(self.w) as i32;
        let s = scale as i32;
        for g in text.graphemes(true) {
            let cw = grapheme_width(g).max(1) as i32 * s;
            if x < 0 || x + cw > limit || y < 0 || (y + s) as usize > self.h {
                x += cw;
                continue;
            }
            for dy in 0..s {
                for dx in 0..cw {
                    self.break_multicell((x + dx) as usize, (y + dy) as usize);
                }
            }
            let style = self.inherit(style, x as usize, y as usize);
            for dy in 0..s {
                for dx in 0..cw {
                    let c = self.at_mut((x + dx) as usize, (y + dy) as usize);
                    *c = Cell::blank(style);
                    c.flag = COVERED;
                    c.len = 0;
                }
            }
            let c = self.at_mut(x as usize, y as usize);
            *c = Cell::new(if g == "\t" { " " } else { g }, style);
            c.scale = scale;
            x += cw;
        }
        x
    }

    /// One OSC 66 block: `text` rendered in `scale*units` × `scale` cells at
    /// font scale `scale * n/d` (bottom aligned). Returns x after the block.
    pub fn put_multicell(&mut self, x: i32, y: i32, text: &str, style: Style, scale: u8, units: u8, n: u8, d: u8, clip1: usize) -> i32 {
        let limit = clip1.min(self.w) as i32;
        let s = scale as i32;
        let bw = s * units as i32;
        if x < 0 || x + bw > limit || y < 0 || (y + s) as usize > self.h || bw == 0 {
            return x + bw;
        }
        for dy in 0..s {
            for dx in 0..bw {
                self.break_multicell((x + dx) as usize, (y + dy) as usize);
            }
        }
        let style = self.inherit(style, x as usize, y as usize);
        for dy in 0..s {
            for dx in 0..bw {
                let c = self.at_mut((x + dx) as usize, (y + dy) as usize);
                *c = Cell::blank(style);
                c.flag = COVERED;
                c.len = 0;
            }
        }
        let c = self.at_mut(x as usize, y as usize);
        *c = Cell::new(text, style);
        c.scale = scale;
        c.units = units;
        c.n = n;
        c.d = d;
        x + bw
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, style: Style) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.break_multicell(xx, yy);
                let st = self.inherit(style, xx, yy);
                *self.at_mut(xx, yy) = Cell::blank(st);
            }
        }
    }

    /// Apply `f` to the style of cells in a horizontal run.
    pub fn restyle(&mut self, x: usize, y: usize, w: usize, f: impl Fn(Style) -> Style) {
        if y >= self.h {
            return;
        }
        for xx in x..(x + w).min(self.w) {
            let c = self.at_mut(xx, y);
            c.style = f(c.style);
        }
    }

    /// Text content of a row (covered/tail cells skipped) — for tests.
    pub fn row_text(&self, y: usize) -> String {
        let mut s = String::new();
        for x in 0..self.w {
            let c = self.at(x, y);
            if c.flag == NORMAL {
                s.push_str(c.text());
            }
        }
        s
    }
}

pub struct Screen {
    prev: Option<Canvas>,
    pub placeholders: bool,
    pub transmitted: HashSet<u32>,
    live: HashMap<Placement, u32>,
    next_pid: u32,
    cursor_shape: u8,
    title: Option<String>,
    frame: u64,
    last_used: HashMap<u32, u64>,
}

impl Screen {
    pub fn new(placeholders: bool) -> Screen {
        Screen {
            prev: None,
            placeholders,
            transmitted: HashSet::new(),
            live: HashMap::new(),
            next_pid: 1,
            cursor_shape: 0,
            title: None,
            frame: 0,
            last_used: HashMap::new(),
        }
    }

    pub fn invalidate(&mut self) {
        self.prev = None;
    }

    pub fn forget_images(&mut self) {
        self.last_used.clear();
        self.transmitted.clear();
        self.live.clear();
    }

    pub fn render(&mut self, cv: Canvas) -> String {
        let mut out = String::with_capacity(16 * 1024);
        out.push_str("\x1b[?2026h\x1b[?25l");
        let full = match &self.prev {
            None => true,
            Some(p) => p.w != cv.w || p.h != cv.h,
        };
        if full {
            out.push_str("\x1b[0m\x1b[2J");
            if !self.placeholders {
                gfx::delete_all(&mut out);
                self.live.clear();
            }
        }
        if cv.title != self.title {
            if let Some(t) = &cv.title {
                out.push_str("\x1b]2;");
                out.push_str(t);
                out.push_str("\x1b\\");
            }
            self.title = cv.title.clone();
        }
        for img in &cv.images {
            if self.transmitted.insert(img.id) {
                gfx::transmit_png(&mut out, img.id, &img.png, img.cols, img.rows, self.placeholders);
            }
            self.last_used.insert(img.id, self.frame);
        }
        // free images not shown for a while (they are re-sent if needed again)
        self.frame += 1;
        if self.transmitted.len() > 48 {
            let frame = self.frame;
            // typeset lines are re-rendered as you type: free unused images quickly
            let age = if self.transmitted.len() > 400 { 2 } else { 120 };
            let stale: Vec<u32> = self.transmitted.iter().copied().filter(|id| frame - self.last_used.get(id).copied().unwrap_or(0) > age).collect();
            for id in stale {
                self.transmitted.remove(&id);
                self.last_used.remove(&id);
                gfx::delete_image(&mut out, id);
            }
        }
        let h = cv.h;
        let w = cv.w;
        let mut changed = vec![true; h];
        if !full {
            let prev = self.prev.as_ref().unwrap();
            for y in 0..h {
                changed[y] = cv.cells[y * w..(y + 1) * w] != prev.cells[y * w..(y + 1) * w];
            }
            // multicell characters span rows: redraw partners together
            let has_cover = |c: &Canvas, y: usize| c.cells[y * w..(y + 1) * w].iter().any(|c| c.flag == COVERED);
            let max_scale = |c: &Canvas, y: usize| c.cells[y * w..(y + 1) * w].iter().map(|c| if c.flag == NORMAL { c.scale } else { 1 }).max().unwrap_or(1);
            let mut again = true;
            while again {
                again = false;
                for y in 0..h {
                    if !changed[y] {
                        continue;
                    }
                    for g in [&cv, prev] {
                        if has_cover(g, y) && y > 0 && !changed[y - 1] {
                            changed[y - 1] = true;
                            again = true;
                        }
                        let sc = max_scale(g, y) as usize;
                        for dy in 1..sc {
                            if y + dy < h && !changed[y + dy] {
                                changed[y + dy] = true;
                                again = true;
                            }
                        }
                    }
                }
            }
        }
        let mut cur: Option<Style> = None;
        let mut cur_link = 0u32;
        for y in 0..h {
            if !changed[y] {
                continue;
            }
            push_cup(&mut out, 0, y);
            let mut pos = 0usize;
            let mut x = 0usize;
            while x < w {
                let c = cv.at(x, y);
                if c.flag != NORMAL {
                    x += 1;
                    continue;
                }
                if pos != x {
                    push_cup(&mut out, x, y);
                    pos = x;
                }
                if cur != Some(c.style) {
                    sgr(&c.style, &mut out);
                    cur = Some(c.style);
                }
                if c.style.link != cur_link {
                    out.push_str("\x1b]8;;");
                    if c.style.link != 0 {
                        out.push_str(cv.links.get(c.style.link as usize).map_or("", |s| s.as_str()));
                    }
                    out.push_str("\x1b\\");
                    cur_link = c.style.link;
                }
                if c.units > 0 {
                    gfx::text_sized_block(&mut out, c.text(), c.scale, c.units, c.n, c.d);
                    let bw = c.scale as usize * c.units as usize;
                    pos = x + bw;
                    x += bw;
                    continue;
                }
                if c.scale > 1 {
                    let sc = c.scale;
                    let mut run = String::from(c.text());
                    let mut x2 = x + grapheme_width(c.text()).max(1) * sc as usize;
                    while x2 < w {
                        let c2 = cv.at(x2, y);
                        if c2.flag != NORMAL || c2.scale != sc || c2.style != c.style || c2.units != 0 {
                            break;
                        }
                        run.push_str(c2.text());
                        x2 += grapheme_width(c2.text()).max(1) * sc as usize;
                    }
                    gfx::text_sized(&mut out, &run, sc);
                    pos = x2;
                    x = x2;
                    continue;
                }
                out.push_str(c.text());
                let cw = if x + 1 < w && cv.at(x + 1, y).flag == WIDE_TAIL { 2 } else { 1 };
                x += cw;
                pos += cw;
            }
            if cur_link != 0 {
                out.push_str("\x1b]8;;\x1b\\");
                cur_link = 0;
            }
        }
        out.push_str("\x1b[0m");
        if !self.placeholders {
            let want: HashSet<&Placement> = cv.placements.iter().collect();
            let stale: Vec<Placement> = self.live.keys().filter(|p| !want.contains(p)).cloned().collect();
            for p in stale {
                let pid = self.live.remove(&p).unwrap();
                gfx::delete_placement(&mut out, p.id, pid);
            }
            for p in &cv.placements {
                if self.live.contains_key(p) {
                    continue;
                }
                let pid = self.next_pid;
                self.next_pid = self.next_pid % 0xFF_FFFF + 1;
                push_cup(&mut out, p.x as usize, p.y as usize);
                gfx::place(&mut out, p.id, pid, (0, p.src_row * p.cell_h, p.cols * p.cell_w, p.rows * p.cell_h));
                self.live.insert(p.clone(), pid);
            }
        }
        if let Some((cx, cy)) = cv.cursor.filter(|_| !cv.cursor_hidden) {
            if cv.cursor_shape != self.cursor_shape {
                out.push_str(&format!("\x1b[{} q", cv.cursor_shape));
                self.cursor_shape = cv.cursor_shape;
            }
            push_cup(&mut out, cx, cy);
            out.push_str("\x1b[?25h");
        }
        out.push_str("\x1b[?2026l");
        self.prev = Some(cv);
        out
    }
}

fn push_cup(out: &mut String, x: usize, y: usize) {
    use std::fmt::Write;
    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_only_changed_rows() {
        let mut s = Screen::new(true);
        let mut c = Canvas::new(10, 3, Style::new());
        c.put(0, 1, "hello", Style::new(), 10);
        let first = s.render(c);
        assert!(first.contains("hello"));
        let mut c = Canvas::new(10, 3, Style::new());
        c.put(0, 1, "hello", Style::new(), 10);
        c.put(0, 2, "x", Style::new(), 10);
        let second = s.render(c);
        assert!(!second.contains("hello"));
        assert!(second.contains("\x1b[3;1H"));
    }

    #[test]
    fn wide_and_scaled() {
        let mut c = Canvas::new(10, 3, Style::new());
        let x = c.put(0, 0, "日本x", Style::new(), 10);
        assert_eq!(x, 5);
        assert_eq!(c.row_text(0).trim_end(), "日本x");
        let x = c.put_scaled(0, 1, "Hi", Style::new(), 2, 10);
        assert_eq!(x, 4);
        assert_eq!(c.at(1, 1).flag, COVERED);
        assert_eq!(c.at(0, 2).flag, COVERED);
        let mut s = Screen::new(true);
        let out = s.render(c);
        assert!(out.contains("\x1b]66;s=2;Hi\x07"));
    }

    #[test]
    fn overwriting_part_of_a_block_erases_it() {
        let mut c = Canvas::new(20, 3, Style::new());
        c.put_multicell(2, 0, "forma", Style::new(), 2, 4, 4, 5, 20);
        // an overlay writes into the block's covered area
        c.put(5, 0, "To", Style::new(), 20);
        assert_eq!(c.at(2, 0).scale, 1, "block destroyed");
        assert_eq!(c.row_text(0).trim_end(), "     To");
        assert!(c.cells.iter().all(|x| x.flag != COVERED), "no orphaned covered cells");
        let mut s = Screen::new(true);
        let out = s.render(c);
        assert!(out.contains("To") && !out.contains("]66;"));
    }

    #[test]
    fn clipping() {
        let mut c = Canvas::new(5, 1, Style::new());
        let x = c.put(3, 0, "日本", Style::new(), 5);
        assert_eq!(x, 7); // logical end, clipped output
        assert_eq!(c.row_text(0), "   日");
    }
}
