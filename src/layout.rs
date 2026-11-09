//! Layout: source line → visual rows. Hides syntax that the cursor is not
//! touching (Obsidian "live preview"), wraps, draws widgets (tables, code
//! panels, callouts, properties) and attaches images. Every segment keeps its
//! source byte range so cursor ↔ screen mapping is exact.
//!
//! Results are cached by a hash of everything that affects them (but *not*
//! the line index), so inserting lines doesn't invalidate the cache and the
//! cost of a frame is proportional to what's visible.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use unicode_segmentation::UnicodeSegmentation;

use crate::buffer::Pos;
use crate::highlight::{self, Tok};
use crate::images::{ImageStore, Img, ImgRef};
use crate::markdown::{self as md, BlockKind, Document, LineKind, Span, SpanKind};
use crate::mathtext;
use crate::style::Style;
use crate::text::{TAB_WIDTH, grapheme_width, str_width};
use crate::theme::{Theme, callout_icon};

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tif", "tiff", "ico", "avif"];

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub x: u16,
    pub w: u16,
    pub text: String,
    pub style: Style,
    /// source byte range; meaningful when `src`
    pub a: usize,
    pub b: usize,
    pub src: bool,
    /// text is exactly line[a..b] (1:1 graphemes)
    pub run: bool,
    /// OSC 66 multicell rendering: overall scale `s`; `units` = the `w` key
    /// (block width in scaled cells, 0 = per grapheme); fractional font scale n/d
    pub scale: u8,
    pub units: u8,
    pub n: u8,
    pub d: u8,
    /// drawn as an inline image (kitty graphics) instead of `text`
    pub icon: Option<crate::images::IconRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Text,
    Image,
    Deco,
}

#[derive(Clone)]
pub struct VRow {
    pub a: usize,
    pub b: usize,
    pub segs: Vec<Seg>,
    pub scale: u8,
    pub kind: RowKind,
    pub cursorable: bool,
    pub fill: Option<Style>,
    pub fill_x0: u16,
    pub fill_x1: Option<u16>,
    pub image: Option<(Arc<Img>, u32, u16)>,
    pub last: bool,
    pub end_x: u16,
    /// typeset (proportional) rows: column ↔ pixel map, and this row's index in the image
    pub tmap: Option<Rc<crate::typeset::TypeMap>>,
    pub trow: u16,
    /// grid-table rows: caret stops per cell. Cells interleave on a row (and a
    /// cell's text wraps over several rows), so the row's source range alone
    /// can't place the caret; when non-empty these are the only caret stops.
    pub cells: Vec<CellStops>,
}

/// Caret stops of one table cell on one visual row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellStops {
    /// x of the cell's first content column and of its last caret position
    pub x0: u16,
    pub x1: u16,
    /// (source byte, x), in source order
    pub stops: Vec<(usize, u16)>,
}

impl CellStops {
    fn push(&mut self, col: usize, x: u16) {
        if !self.stops.iter().any(|&(c, _)| c == col) {
            self.stops.push((col, x));
        }
    }
}

impl VRow {
    fn new(a: usize, b: usize) -> VRow {
        VRow {
            a,
            b,
            segs: vec![],
            scale: 1,
            kind: RowKind::Text,
            cursorable: true,
            fill: None,
            fill_x0: 0,
            fill_x1: None,
            image: None,
            last: false,
            end_x: 0,
            tmap: None,
            trow: 0,
            cells: vec![],
        }
    }
    fn deco() -> VRow {
        VRow { cursorable: false, kind: RowKind::Deco, ..VRow::new(0, 0) }
    }
    pub fn height(&self) -> usize {
        self.scale as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Live,
    Source,
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeadingMode {
    Plain,
    Osc66,
    Image,
}

pub struct Ctx<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    pub mode: Mode,
    pub heading_mode: HeadingMode,
    pub images: Option<&'a ImageStore>,
    pub link_exists: &'a dyn Fn(&str) -> bool,
    /// lines of an embedded note (target, heading anchor), for `![[note]]`
    pub embed: &'a dyn Fn(&str, &str) -> Option<Vec<String>>,
    pub max_image_rows: u32,
    pub epoch: u64,
    /// Obsidian's "inline title": the note name shown large above line 0
    pub title: Option<&'a str>,
    /// proportional typesetting of prose (None = terminal text)
    pub typo: Option<Typo>,
    /// where to draw the caret (typeset lines draw it into the image)
    pub caret: Option<crate::buffer::Pos>,
    pub sel: Option<(crate::buffer::Pos, crate::buffer::Pos)>,
    pub hits: &'a [(crate::buffer::Pos, crate::buffer::Pos)],
    pub cur_hit: Option<usize>,
}

/// Typesetting parameters (all colours must be known RGB).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typo {
    pub font_px: f32,
    pub line_height: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub fg: (u8, u8, u8),
    pub caret: (u8, u8, u8),
    pub selection: (u8, u8, u8),
    pub hit: (u8, u8, u8),
    pub hit_cur: (u8, u8, u8),
    pub panel: (u8, u8, u8),
}

impl Ctx<'_> {
    /// Caret, selection and search hits that fall on line `i`.
    pub fn marks_on(&self, i: usize, len: usize) -> (Option<usize>, Option<(usize, usize)>, Vec<(usize, usize, bool)>) {
        let caret = self.caret.filter(|c| c.line == i).map(|c| c.col);
        let sel = self.sel.and_then(|(a, b)| {
            if a.line <= i && i <= b.line {
                let sa = if a.line == i { a.col } else { 0 };
                let sb = if b.line == i { b.col } else { len };
                (sb > sa).then_some((sa, sb))
            } else {
                None
            }
        });
        let start = self.hits.partition_point(|h| h.0.line < i);
        let hits = self.hits[start..]
            .iter()
            .enumerate()
            .take_while(|(_, h)| h.0.line == i)
            .map(|(k, h)| (h.0.col, h.1.col, Some(start + k) == self.cur_hit))
            .collect();
        (caret, sel, hits)
    }
}

#[derive(Clone, Debug)]
pub struct Atom {
    pub a: usize,
    pub b: usize,
    pub w: u16,
    pub style: Style,
    pub repl: Option<String>,
    pub hidden: bool,
    pub space: bool,
    pub tab: bool,
    pub icon: Option<crate::images::IconRef>,
    /// typeset: start this atom at x = tab_to px (column alignment)
    pub tab_to: Option<f32>,
}

impl Atom {
    fn src(a: usize, b: usize, w: u16, style: Style, space: bool) -> Atom {
        Atom { a, b, w, style, repl: None, hidden: false, space, tab: false, icon: None, tab_to: None }
    }
    fn repl(a: usize, b: usize, text: String, style: Style) -> Atom {
        let w = str_width(&text) as u16;
        Atom { a, b, w, style, repl: Some(text), hidden: false, space: false, tab: false, icon: None, tab_to: None }
    }
    fn hidden(a: usize, b: usize) -> Atom {
        Atom { a, b, w: 0, style: Style::new(), repl: None, hidden: true, space: false, tab: false, icon: None, tab_to: None }
    }
}

/// Greedy word wrap of atoms into rows of segments.
pub fn wrap_atoms(line: &str, atoms: &[Atom], width: usize, x0: usize, cont_x: usize, scale: u8, char_wrap: bool) -> Vec<Vec<Seg>> {
    let width = width as i64;
    let sc = scale as i64;
    let mut rows: Vec<Vec<Atom>> = vec![];
    let mut xs: Vec<Vec<i64>> = vec![];
    let mut cur: Vec<Atom> = vec![];
    let mut curx: Vec<i64> = vec![];
    let mut x = x0 as i64;
    let mut last_break: Option<usize> = None;
    for at in atoms {
        let mut w = at.w as i64 * sc;
        if at.tab {
            let rel = (x - x0 as i64) / sc;
            w = (TAB_WIDTH as i64 - rel.rem_euclid(TAB_WIDTH as i64)) * sc;
        }
        if x + w > width && !cur.is_empty() && w > 0 {
            if at.space {
                // trailing whitespace hangs off the edge
                let mut h = at.clone();
                h.w = 0;
                cur.push(h);
                curx.push(x);
                rows.push(std::mem::take(&mut cur));
                xs.push(std::mem::take(&mut curx));
                x = cont_x as i64;
                last_break = None;
                continue;
            }
            match last_break {
                Some(lb) if !char_wrap && lb < cur.len() && lb > 0 => {
                    let carry: Vec<Atom> = cur.drain(lb..).collect();
                    curx.truncate(lb);
                    rows.push(std::mem::take(&mut cur));
                    xs.push(std::mem::take(&mut curx));
                    x = cont_x as i64;
                    for c in carry {
                        let cw = if c.tab { TAB_WIDTH as i64 * sc } else { c.w as i64 * sc };
                        curx.push(x);
                        x += cw;
                        cur.push(c);
                    }
                }
                _ => {
                    rows.push(std::mem::take(&mut cur));
                    xs.push(std::mem::take(&mut curx));
                    x = cont_x as i64;
                }
            }
            last_break = None;
            if at.tab {
                let rel = (x - x0 as i64) / sc;
                w = (TAB_WIDTH as i64 - rel.rem_euclid(TAB_WIDTH as i64)) * sc;
            }
        }
        let mut a2 = at.clone();
        if at.tab {
            a2.w = (w / sc) as u16;
        }
        cur.push(a2);
        curx.push(x);
        x += w;
        if at.space || at.tab {
            last_break = Some(cur.len());
        }
    }
    rows.push(cur);
    xs.push(curx);
    // coalesce into segments
    rows.into_iter()
        .zip(xs)
        .map(|(row, xs)| {
            let mut segs: Vec<Seg> = vec![];
            for (at, x) in row.into_iter().zip(xs) {
                let w = at.w * scale as u16;
                if at.hidden || (at.w == 0 && at.repl.is_none()) {
                    segs.push(Seg {
                        x: x as u16,
                        w: 0,
                        text: String::new(),
                        style: at.style,
                        a: at.a,
                        b: at.b,
                        src: true,
                        run: false,
                        scale,
                        units: 0,
                        n: 0,
                        d: 0,
                        icon: None,
                    });
                    continue;
                }
                if let Some(r) = at.repl {
                    segs.push(Seg {
                        x: x as u16,
                        w,
                        text: r,
                        style: at.style,
                        a: at.a,
                        b: at.b,
                        src: true,
                        run: false,
                        scale,
                        units: 0,
                        n: 0,
                        d: 0,
                        icon: at.icon,
                    });
                    continue;
                }
                if at.tab {
                    segs.push(Seg {
                        x: x as u16,
                        w,
                        text: " ".repeat(at.w as usize),
                        style: at.style,
                        a: at.a,
                        b: at.b,
                        src: true,
                        run: false,
                        scale,
                        units: 0,
                        n: 0,
                        d: 0,
                        icon: None,
                    });
                    continue;
                }
                if let Some(last) = segs.last_mut()
                    && last.run
                    && last.style == at.style
                    && last.b == at.a
                    && (last.x + last.w) as i64 == x
                {
                    last.b = at.b;
                    last.w += w;
                    last.text.push_str(&line[at.a..at.b]);
                    continue;
                }
                segs.push(Seg {
                    x: x as u16,
                    w,
                    text: line[at.a..at.b].to_string(),
                    style: at.style,
                    a: at.a,
                    b: at.b,
                    src: true,
                    run: true,
                    scale,
                    units: 0,
                    n: 0,
                    d: 0,
                    icon: None,
                });
            }
            segs
        })
        .collect()
}

const TASK_GLYPHS: &[(char, &str)] = &[(' ', "☐"), ('x', "☑"), ('X', "☑"), ('/', "◐"), ('-', "⊘"), ('!', "!"), ('?', "?"), ('>', "➜"), ('<', "⇠"), ('*', "★")];
const BULLETS: &[&str] = &["•", "•", "•", "•"];

/// A half-height panel edge drawn with block elements; the quadrant corners
/// read as rounded corners: `▗▄▄▄▖` (top) / `▝▀▀▀▘` (bottom).
pub fn edge_row(width: usize, color: crate::style::Color, top: bool) -> VRow {
    let w = width.max(2);
    let t = if top { format!("▗{}▖", "▄".repeat(w - 2)) } else { format!("▝{}▘", "▀".repeat(w - 2)) };
    let mut r = VRow::deco();
    r.segs.push(Seg { x: 0, w: w as u16, text: t, style: Style::fg(color), a: 0, b: 0, src: false, run: false, scale: 1, units: 0, n: 0, d: 0, icon: None });
    r
}

/// (s, n, d) for OSC 66 headings: H1 2×, H2 2·4/5 = 1.6×, H3 2·2/3 ≈ 1.33×
/// (close to Obsidian's 1.8/1.6/1.4 ramp while keeping whole-cell chunks).
pub fn heading_scale(level: u8) -> (u8, u8, u8) {
    match level {
        1 => (2, 0, 0),
        2 => (2, 4, 5),
        3 => (2, 2, 3),
        _ => (1, 0, 0),
    }
}

/// Lay out atoms as OSC 66 multicell text. Fractional sizes are split into
/// chunks whose width is a whole number of scaled cells; a chunk also ends at
/// style changes and before the cursor, so the cursor always sits on a block
/// boundary where kitty draws a full-height cursor.
pub fn multicell_rows(text: &str, atoms: &[Atom], spec: (u8, u8, u8), cursor: Option<usize>, width: usize) -> Vec<VRow> {
    let (s, n, d) = (spec.0 as usize, spec.1 as usize, spec.2 as usize);
    let frac = n > 0 && d > n;
    let (fnum, fden) = if frac { (n, d) } else { (1, 1) };
    let aw = |a: &Atom| -> usize {
        if a.hidden {
            0
        } else if a.tab {
            1
        } else {
            a.w as usize
        }
    };
    // capacity of a row in natural (unscaled) cells, with slack for chunk rounding
    let cap = (width.saturating_sub(2 * s) * fden / (s * fnum)).max(1);
    let mut lines: Vec<Vec<&Atom>> = vec![vec![]];
    let mut w = 0usize;
    let mut last_break: Option<usize> = None;
    for at in atoms {
        let a_w = aw(at);
        let cur = lines.last_mut().unwrap();
        if w + a_w > cap && a_w > 0 && !cur.is_empty() {
            if at.space {
                cur.push(at);
                lines.push(vec![]);
                w = 0;
                last_break = None;
                continue;
            }
            match last_break {
                Some(lb) if lb > 0 && lb < cur.len() => {
                    let carry = cur.split_off(lb);
                    w = carry.iter().map(|a| aw(a)).sum();
                    lines.push(carry);
                }
                _ => {
                    lines.push(vec![]);
                    w = 0;
                }
            }
            last_break = None;
        }
        let cur = lines.last_mut().unwrap();
        cur.push(at);
        w += a_w;
        if at.space {
            last_break = Some(cur.len());
        }
    }
    let nlines = lines.len();
    let mut rows = vec![];
    for (li, line) in lines.into_iter().enumerate() {
        let mut segs: Vec<Seg> = vec![];
        let mut x = 0usize;
        let mut chunk: Vec<&Atom> = vec![];
        let mut cw = 0usize;
        let mut bytes = 0usize;
        let flush = |chunk: &mut Vec<&Atom>, cw: &mut usize, bytes: &mut usize, x: &mut usize, segs: &mut Vec<Seg>| {
            if chunk.is_empty() {
                return;
            }
            let t: String = chunk.iter().map(|a| a.repl.clone().unwrap_or_else(|| if a.tab { " ".into() } else { text[a.a..a.b].to_string() })).collect();
            let run = chunk.iter().all(|a| a.repl.is_none() && !a.tab) && chunk.windows(2).all(|p| p[0].b == p[1].a);
            let (units, wcells) = if frac {
                let u = (*cw * fnum).div_ceil(fden).max(1);
                (u, s * u)
            } else {
                (0, s * *cw)
            };
            segs.push(Seg {
                x: *x as u16,
                w: wcells as u16,
                text: t,
                style: chunk[0].style,
                a: chunk[0].a,
                b: chunk.last().unwrap().b,
                src: true,
                run,
                scale: s as u8,
                units: units as u8,
                n: if frac { n as u8 } else { 0 },
                d: if frac { d as u8 } else { 0 },
                icon: None,
            });
            *x += wcells;
            chunk.clear();
            *cw = 0;
            *bytes = 0;
        };
        for at in &line {
            let a_w = aw(at);
            if a_w == 0 {
                flush(&mut chunk, &mut cw, &mut bytes, &mut x, &mut segs);
                segs.push(Seg {
                    x: x as u16,
                    w: 0,
                    text: String::new(),
                    style: at.style,
                    a: at.a,
                    b: at.b,
                    src: true,
                    run: false,
                    scale: 1,
                    units: 0,
                    n: 0,
                    d: 0,
                    icon: None,
                });
                continue;
            }
            let len = at.repl.as_ref().map_or(at.b - at.a, |r| r.len());
            if !chunk.is_empty() && (chunk[0].style != at.style || (frac && cursor == Some(at.a)) || bytes + len > 20) {
                flush(&mut chunk, &mut cw, &mut bytes, &mut x, &mut segs);
            }
            chunk.push(at);
            cw += a_w;
            bytes += len;
            if frac && (cw * fnum).is_multiple_of(fden) {
                flush(&mut chunk, &mut cw, &mut bytes, &mut x, &mut segs);
            }
        }
        flush(&mut chunk, &mut cw, &mut bytes, &mut x, &mut segs);
        let src: Vec<&Seg> = segs.iter().filter(|s| s.src).collect();
        let ra = if li == 0 { 0 } else { src.first().map_or(0, |s| s.a) };
        let rb = if li + 1 == nlines { text.len() } else { src.last().map_or(ra, |s| s.b) };
        let mut row = VRow::new(ra, rb);
        row.segs = segs;
        row.scale = s as u8;
        row.end_x = x as u16;
        rows.push(row);
    }
    // a row's start must continue where the previous ended
    for k in 1..rows.len() {
        if rows[k].segs.iter().all(|s| !s.src) {
            rows[k].a = rows[k - 1].b;
        }
    }
    rows
}

/// Horizontal advance (cells, fractional) of `gw` natural cells inside `s`.
fn seg_adv(s: &Seg, gw: usize) -> f32 {
    let f = if s.units > 0 && s.n > 0 && s.d > s.n { s.n as f32 / s.d as f32 } else { 1.0 };
    gw as f32 * s.scale as f32 * f
}

pub fn is_image_target(t: &str) -> bool {
    let t = t.split(['?', '#']).next().unwrap_or(t);
    match t.rsplit_once('.') {
        Some((_, ext)) => IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

#[derive(Default)]
pub struct Layout {
    cache: RefCell<HashMap<u64, Rc<Vec<VRow>>>>,
    spans: RefCell<HashMap<u64, Rc<Vec<Span>>>>,
    hl: RefCell<HashMap<u64, Rc<Vec<Vec<(usize, usize, Tok)>>>>>,
    tables: RefCell<HashMap<u64, Rc<TableModel>>>,
}

pub struct TableModel {
    widths: Vec<usize>,
    aligns: Vec<u8>,
    cells: HashMap<usize, Vec<Vec<Atom>>>,
    /// source byte range of each cell's content, by row offset
    spans: HashMap<usize, Vec<(usize, usize)>>,
}

fn h64<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

impl Layout {
    pub fn new() -> Layout {
        Layout::default()
    }

    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
        self.tables.borrow_mut().clear();
    }

    pub fn spans(&self, text: &str) -> Rc<Vec<Span>> {
        let k = h64(&text);
        if let Some(s) = self.spans.borrow().get(&k) {
            return s.clone();
        }
        let s = Rc::new(md::parse_inline(text, 0, text.len()));
        let mut m = self.spans.borrow_mut();
        if m.len() > 50_000 {
            m.clear();
        }
        m.insert(k, s.clone());
        s
    }

    /// Visual rows for line `i`. `cursor` is the cursor column when it is on
    /// this line; `active` the cursor position when it is inside this line's
    /// block (a grid table sizes its columns around the cell being edited).
    pub fn rows(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, cursor: Option<usize>, active: Option<Pos>) -> Rc<Vec<VRow>> {
        let block_active = active.is_some();
        let info = &doc.infos[i];
        let text = &lines[i];
        let blk = info.block.map(|b| &doc.blocks[b]);
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        info.hash(&mut h);
        if let Some(b) = blk {
            b.kind.hash(&mut h);
            (i - b.start, b.end - i).hash(&mut h);
            match b.kind {
                BlockKind::Table | BlockKind::Front | BlockKind::Math => lines[b.start..b.end].hash(&mut h),
                BlockKind::Code => {
                    b.lang.hash(&mut h);
                    lines[b.start..b.end].hash(&mut h);
                }
                _ => b.hash(&mut h),
            }
        }
        match blk {
            Some(b) if b.kind == BlockKind::Table => active.map(|p| (p.line - b.start, p.col)).hash(&mut h),
            _ => block_active.hash(&mut h),
        }
        cursor.hash(&mut h);
        if i == 0 {
            ctx.title.hash(&mut h);
        }
        if let Some(t) = &ctx.typo {
            ((t.font_px * 100.0) as u32, (t.line_height * 100.0) as u32, t.cell_w, t.cell_h, t.fg, t.caret, t.selection).hash(&mut h);
            ctx.marks_on(i, text.len()).hash(&mut h);
        }
        (ctx.width, ctx.mode, ctx.heading_mode, ctx.max_image_rows).hash(&mut h);
        if text.contains('!') || text.contains('$') || info.kind == LineKind::Heading || blk.is_some() {
            ctx.epoch.hash(&mut h);
        }
        let key = h.finish();
        if let Some(r) = self.cache.borrow().get(&key) {
            return r.clone();
        }
        let mut rows = self.layout_line(ctx, lines, doc, i, cursor, active);
        if i == 0
            && let Some(t) = ctx.title
            && ctx.mode != Mode::Source
        {
            let mut tr = self.title_rows(ctx, t);
            tr.extend(rows);
            rows = tr;
        }
        if let Some(last) = rows.iter_mut().rev().find(|r| r.cursorable) {
            last.last = true;
        }
        let rows = Rc::new(rows);
        let mut c = self.cache.borrow_mut();
        if c.len() > 20_000 {
            c.clear();
        }
        c.insert(key, rows.clone());
        rows
    }

    /// Atoms of a heading line (markers revealed only on the active line).
    fn heading_atoms(&self, ctx: &Ctx, text: &str, info: &md::LineInfo, active_line: bool, base: Style, reveal: Option<usize>) -> Vec<Atom> {
        let th = ctx.theme;
        let mut atoms = vec![];
        if !info.setext {
            let mstart = crate::text::leading_ws(text);
            if mstart > 0 {
                atoms.push(Atom::hidden(0, mstart));
            }
            if active_line {
                atoms.extend(self.raw_atoms(text, mstart, info.content, th.marker().bold()));
            } else if info.content > mstart {
                atoms.push(Atom::hidden(mstart, info.content));
            }
        }
        let mut end = text.len();
        if !info.setext && !active_line {
            let t = text.trim_end();
            let j = t.trim_end_matches('#').len();
            if j < t.len() && (j == info.content || t[..j].ends_with([' ', '\t'])) {
                end = t[..j].trim_end().len().max(info.content);
            }
        }
        let start = if info.setext { crate::text::leading_ws(text) } else { info.content };
        if info.setext && start > 0 {
            atoms.push(Atom::hidden(0, start));
        }
        atoms.extend(self.inline_atoms(ctx, text, start, end, base, reveal, false).0);
        if end < text.len() {
            atoms.push(Atom::hidden(end, text.len()));
        }
        atoms
    }

    /// Typeset atoms as proportional text in an image placed `x` cells in.
    /// Returns None when the text can't be typeset (e.g. CJK) — the caller
    /// then falls back to terminal cells.
    pub fn typeset_rows(
        &self,
        ctx: &Ctx,
        i: usize,
        text: &str,
        atoms: &[Atom],
        prefix: Vec<Seg>,
        cont_prefix: &[Seg],
        x: usize,
        heading: Option<u8>,
        fill: Option<Style>,
    ) -> Option<Vec<VRow>> {
        use crate::typeset::{Face, Marks, Params, Piece};
        let t = ctx.typo?;
        let imgs = ctx.images?;
        let th = ctx.theme;
        let fonts = crate::typeset::Fonts::get();
        let (scale, hface) = match heading {
            Some(1) => (1.8, Face::Bold),
            Some(2) => (1.55, Face::Bold),
            Some(3) => (1.35, Face::Bold),
            Some(4) => (1.2, Face::SemiBold),
            Some(5) => (1.1, Face::SemiBold),
            Some(_) => (1.0, Face::SemiBold),
            None => (1.0, Face::Regular),
        };
        let rgb = |c: crate::style::Color, d: (u8, u8, u8)| c.rgb().unwrap_or(d);
        let mut pieces: Vec<Piece> = vec![];
        for at in atoms {
            let st = at.style;
            let is_code = st.bg.is_set() && rgb(st.bg, (0, 0, 0)) == t.panel;
            let face = if is_code {
                Face::Mono
            } else {
                match (st.has(crate::style::BOLD) || heading.is_some(), st.has(crate::style::ITALIC)) {
                    (true, true) => Face::BoldItalic,
                    (true, false) => {
                        if heading.is_some() {
                            hface
                        } else {
                            Face::Bold
                        }
                    }
                    (false, true) => Face::Italic,
                    _ => Face::Regular,
                }
            };
            let fg = rgb(st.fg, t.fg);
            let bg = if st.bg.is_set() { st.bg.rgb() } else { None };
            let underline = (st.underline != 0).then(|| rgb(st.ul_color, fg));
            let strike = st.has(crate::style::STRIKE);
            let pscale = if is_code { scale * 0.92 } else { scale };
            let (txt, src) = if at.hidden {
                (String::new(), true)
            } else if let Some(r) = &at.repl {
                (r.clone(), false)
            } else {
                (text[at.a..at.b].to_string(), true)
            };
            if !txt.is_empty() && !fonts.covers(face, &txt) {
                return None;
            }
            // merge plain source runs with identical styling
            if src && !txt.is_empty() {
                if let Some(last) = pieces.last_mut() {
                    if last.src
                        && !last.text.is_empty()
                        && last.b == at.a
                        && last.face == face
                        && last.fg == fg
                        && last.bg == bg
                        && last.underline == underline
                        && last.strike == strike
                    {
                        last.text.push_str(&txt);
                        last.b = at.b;
                        continue;
                    }
                }
            }
            pieces.push(Piece { text: txt, a: at.a, b: at.b, src, face, scale: pscale, fg, bg, underline, strike, tab: at.tab_to });
        }
        let width_cells = ctx.width.saturating_sub(x).max(4);
        let pad_top = if heading.is_some() { (t.font_px * scale * 0.35).round() } else { 0.0 };
        let params = Params {
            width_px: (width_cells as u32 * t.cell_w) as f32 - 2.0,
            font_px: t.font_px,
            line_height: if heading.is_some() { 1.25 } else { t.line_height },
            pad_top,
            cell_w: t.cell_w,
            cell_h: t.cell_h,
        };
        let ts = crate::typeset::layout(pieces, &params);
        let (caret, sel, hits) = ctx.marks_on(i, text.len());
        let marks = Marks {
            caret,
            caret_color: t.caret,
            selection: sel.into_iter().collect(),
            selection_color: t.selection,
            hits: hits.into_iter().map(|(a, b, cur)| (a, b, if cur { t.hit_cur } else { t.hit })).collect(),
        };
        let im = ts.render(&marks);
        let img = imgs.adhoc(&im);
        let map = Rc::new(ts.map);
        let _ = th;
        let mut rows = vec![];
        let anchors: Vec<(usize, usize)> = (0..map.lines.len()).map(|li| (map.anchor_row(li), li)).collect();
        for r in 0..map.rows {
            let anchor = anchors.iter().find(|a| a.0 == r).map(|a| a.1);
            let (a, b) = match anchor {
                Some(li) => (map.lines[li].a, map.lines[li].b),
                None => {
                    let prev = anchors.iter().rev().find(|a| a.0 < r).map(|a| a.1).unwrap_or(0);
                    (map.lines.get(prev).map_or(0, |l| l.b), map.lines.get(prev).map_or(0, |l| l.b))
                }
            };
            let mut row = VRow::new(if r == 0 { 0 } else { a }, b);
            row.kind = RowKind::Image;
            row.image = Some((img.clone(), r as u32, x as u16));
            row.cursorable = anchor.is_some();
            row.segs = if r == 0 { prefix.clone() } else { cont_prefix.to_vec() };
            row.fill = fill;
            row.tmap = Some(map.clone());
            row.trow = r as u16;
            row.end_x = (x + anchor.map_or(0, |li| (map.lines[li].stops.last().map_or(0.0, |s| s.1) / t.cell_w as f32).ceil() as usize)) as u16;
            rows.push(row);
        }
        if let Some(last) = rows.iter_mut().rev().find(|r| r.cursorable) {
            last.b = text.len();
        }
        Some(rows)
    }

    /// The inline title block: note name at H1 size plus a spacer row.
    fn title_rows(&self, ctx: &Ctx, title: &str) -> Vec<VRow> {
        let th = ctx.theme;
        let st = th.heading_style(1);
        let atoms = self.raw_atoms(title, 0, title.len(), st);
        // typeset: line index usize::MAX → no caret/selection marks
        let typeset = if ctx.typo.is_some() { self.typeset_rows(ctx, usize::MAX, title, &atoms, vec![], &[], 0, Some(1), None) } else { None };
        let mut rows = match ctx.heading_mode {
            _ if typeset.is_some() => typeset.unwrap(),
            HeadingMode::Osc66 => multicell_rows(title, &atoms, heading_scale(1), None, ctx.width),
            HeadingMode::Image => {
                let col = st.fg.rgb().unwrap_or(if th.dark { (230, 230, 230) } else { (20, 20, 20) });
                match ctx.images.map(|imgs| imgs.heading(title, 1, ctx.width as u32, col)) {
                    Some(ImgRef::Ready(img)) => (0..img.data.rows)
                        .map(|r| {
                            let mut row = VRow::new(0, 0);
                            row.kind = RowKind::Image;
                            row.image = Some((img.clone(), r, 0));
                            row
                        })
                        .collect(),
                    _ => self.rows_from(ctx, title, &atoms, vec![], &[], 0, title.len(), None, None, 1, None, false, ctx.width),
                }
            }
            HeadingMode::Plain => self.rows_from(ctx, title, &atoms, vec![], &[], 0, title.len(), None, None, 1, None, false, ctx.width),
        };
        for r in rows.iter_mut() {
            if r.kind != RowKind::Image {
                r.kind = RowKind::Deco;
            }
            r.tmap = None;
            r.cursorable = false;
            r.a = 0;
            r.b = 0;
            for s in r.segs.iter_mut() {
                s.src = false;
            }
        }
        rows.push(VRow::deco());
        rows
    }

    // ------------------------------------------------------------------ inline
    pub fn inline_atoms(&self, ctx: &Ctx, text: &str, a: usize, b: usize, base: Style, reveal: Option<usize>, source: bool) -> (Vec<Atom>, Vec<Span>) {
        let th = ctx.theme;
        let reading = ctx.mode == Mode::Read;
        let all = self.spans(text);
        let mut spans: Vec<Span> = all.iter().filter(|s| s.start >= a && s.end <= b).cloned().collect();
        spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        let n = b - a;
        let mut styles = vec![base; n];
        let mut hidden = vec![false; n];
        let mut repl: Vec<(usize, usize, String, Style)> = vec![];
        let marker = th.marker();
        let paint = |styles: &mut Vec<Style>, x0: usize, x1: usize, st: Style| {
            for k in x0.max(a)..x1.min(b) {
                styles[k - a] = styles[k - a].over(st);
            }
        };
        let hide = |hidden: &mut Vec<bool>, x0: usize, x1: usize| {
            for k in x0.max(a)..x1.min(b) {
                hidden[k - a] = true;
            }
        };
        for sp in &spans {
            let rev = source || reveal.is_some_and(|r| sp.start <= r && r <= sp.end);
            let generic = match sp.kind {
                SpanKind::Strong => {
                    paint(&mut styles, sp.open_end, sp.close_start, Style::new().bold());
                    true
                }
                SpanKind::Em => {
                    paint(&mut styles, sp.open_end, sp.close_start, Style::new().italic());
                    true
                }
                SpanKind::Strike => {
                    paint(&mut styles, sp.open_end, sp.close_start, Style::fg(th.muted).strike());
                    true
                }
                SpanKind::Mark => {
                    paint(&mut styles, sp.open_end, sp.close_start, Style::fg(th.mark_fg).with_bg(th.mark_bg));
                    true
                }
                SpanKind::Code => {
                    paint(&mut styles, sp.start, sp.end, Style::fg(th.code_fg).with_bg(th.panel));
                    if rev {
                        paint(&mut styles, sp.start, sp.open_end, Style::fg(th.faint));
                        paint(&mut styles, sp.close_start, sp.end, Style::fg(th.faint));
                    } else {
                        // backticks become the pill's padding: same width, so
                        // revealing them never shifts the line
                        let pill = Style::fg(th.code_fg).with_bg(th.panel);
                        hide(&mut hidden, sp.start, sp.open_end);
                        hide(&mut hidden, sp.close_start, sp.end);
                        repl.push((sp.start, sp.open_end, " ".repeat(sp.open_end - sp.start), pill));
                        repl.push((sp.close_start, sp.end, " ".repeat(sp.end - sp.close_start), pill));
                    }
                    false
                }
                SpanKind::Link => {
                    paint(&mut styles, sp.open_end, sp.close_start, th.link_style());
                    let external = sp.target.contains("://") || sp.target.starts_with("mailto:");
                    if !rev && external && sp.end > sp.close_start {
                        // Obsidian marks external links with a small arrow
                        hide(&mut hidden, sp.start, sp.open_end);
                        hide(&mut hidden, sp.close_start, sp.end);
                        repl.push((sp.close_start, sp.end, "↗".into(), Style::fg(th.link)));
                        false
                    } else {
                        true
                    }
                }
                SpanKind::WikiLink => {
                    let st = if (ctx.link_exists)(&sp.target) { th.link_style() } else { th.unresolved_style() };
                    paint(&mut styles, sp.open_end, sp.close_start, st);
                    true
                }
                SpanKind::Url => {
                    paint(&mut styles, sp.start, sp.end, th.link_style());
                    if sp.open_end > sp.start {
                        if rev {
                            paint(&mut styles, sp.start, sp.open_end, marker);
                            paint(&mut styles, sp.close_start, sp.end, marker);
                        } else {
                            hide(&mut hidden, sp.start, sp.open_end);
                            hide(&mut hidden, sp.close_start, sp.end);
                        }
                    }
                    false
                }
                SpanKind::Tag => {
                    paint(&mut styles, sp.start, sp.end, Style::fg(th.tag_fg).with_bg(th.tag_bg));
                    false
                }
                SpanKind::Escape => {
                    if rev {
                        paint(&mut styles, sp.start, sp.open_end, marker);
                    } else {
                        hide(&mut hidden, sp.start, sp.open_end);
                    }
                    false
                }
                SpanKind::Comment => {
                    if reading {
                        hide(&mut hidden, sp.start, sp.end);
                    } else {
                        paint(&mut styles, sp.start, sp.end, Style::fg(th.faint).italic());
                    }
                    false
                }
                SpanKind::FootRef => {
                    if rev {
                        paint(&mut styles, sp.start, sp.end, Style::fg(th.accent));
                    } else {
                        hide(&mut hidden, sp.start, sp.end);
                        let sup = mathtext::superscript(&sp.target).unwrap_or_else(|| format!("[{}]", sp.target));
                        repl.push((sp.start, sp.end, sup, Style::fg(th.accent)));
                    }
                    false
                }
                SpanKind::Math => {
                    if rev {
                        paint(&mut styles, sp.start, sp.end, Style::fg(th.code_fg));
                        paint(&mut styles, sp.start, sp.open_end, marker);
                        paint(&mut styles, sp.close_start, sp.end, marker);
                    } else {
                        hide(&mut hidden, sp.start, sp.end);
                        repl.push((sp.start, sp.end, mathtext::convert(&sp.target), Style::new().italic()));
                    }
                    false
                }
                SpanKind::Image | SpanKind::Embed => {
                    if source {
                        paint(&mut styles, sp.start, sp.end, Style::fg(th.muted));
                        return_false();
                    } else if !rev && is_image_target(&sp.target) {
                        hide(&mut hidden, sp.start, sp.end);
                    } else if !rev && sp.kind == SpanKind::Embed {
                        hide(&mut hidden, sp.start, sp.end);
                        let st = if (ctx.link_exists)(&sp.target) { th.link_style() } else { th.unresolved_style() };
                        repl.push((
                            sp.start,
                            sp.end,
                            format!("↳ {}", if sp.anchor.is_empty() { sp.target.clone() } else { format!("{} › {}", sp.target, sp.anchor) }),
                            st,
                        ));
                    } else {
                        paint(&mut styles, sp.start, sp.end, Style::fg(th.muted));
                        paint(&mut styles, sp.start, sp.start + if sp.kind == SpanKind::Embed { 3 } else { 2 }, marker);
                    }
                    false
                }
            };
            if generic {
                for (x0, x1) in sp.markers() {
                    if x1 > x0 {
                        if rev {
                            paint(&mut styles, x0, x1, marker);
                        } else {
                            hide(&mut hidden, x0, x1);
                        }
                    }
                }
            }
        }
        let mut atoms = Vec::with_capacity(n);
        let mut k = a;
        let mut ri = 0;
        repl.sort_by_key(|r| r.0);
        while k < b {
            while ri < repl.len() && repl[ri].0 < k {
                ri += 1;
            }
            if ri < repl.len() && repl[ri].0 == k {
                let (s, e, ref t, st) = repl[ri];
                atoms.push(Atom::repl(s, e, t.clone(), styles[k - a].over(st)));
                k = e;
                ri += 1;
                continue;
            }
            if hidden[k - a] {
                let mut e = k + 1;
                while e < b && !text.is_char_boundary(e) {
                    e += 1;
                }
                atoms.push(Atom::hidden(k, e));
                k = e;
                continue;
            }
            let g_end = crate::text::next_boundary(text, k).min(b);
            let g = &text[k..g_end];
            if g == "\t" {
                let mut at = Atom::src(k, g_end, 1, styles[k - a], false);
                at.tab = true;
                atoms.push(at);
            } else {
                atoms.push(Atom::src(k, g_end, grapheme_width(g) as u16, styles[k - a], g == " "));
            }
            k = g_end;
        }
        (atoms, spans)
    }

    fn raw_atoms(&self, text: &str, a: usize, b: usize, st: Style) -> Vec<Atom> {
        let mut out = vec![];
        let mut k = a;
        for g in text[a..b].graphemes(true) {
            let e = k + g.len();
            if g == "\t" {
                let mut at = Atom::src(k, e, 1, st, false);
                at.tab = true;
                out.push(at);
            } else {
                out.push(Atom::src(k, e, grapheme_width(g) as u16, st, g == " "));
            }
            k = e;
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn rows_from(
        &self,
        ctx: &Ctx,
        text: &str,
        atoms: &[Atom],
        prefix: Vec<Seg>,
        cont_prefix: &[Seg],
        a: usize,
        b: usize,
        x0: Option<usize>,
        cont_x: Option<usize>,
        scale: u8,
        fill: Option<Style>,
        char_wrap: bool,
        width: usize,
    ) -> Vec<VRow> {
        let px = x0.unwrap_or_else(|| prefix.iter().map(|s| (s.x + s.w) as usize).max().unwrap_or(0));
        let cx = cont_x.unwrap_or(px);
        let wrapped = wrap_atoms(text, atoms, width, px, cx, scale, char_wrap);
        let _ = ctx;
        let mut rows: Vec<VRow> = vec![];
        let nrows = wrapped.len();
        for (n, segs) in wrapped.into_iter().enumerate() {
            let src: Vec<&Seg> = segs.iter().filter(|s| s.src).collect();
            let ra = if n == 0 { a } else { src.first().map_or(rows.last().map_or(a, |r: &VRow| r.b), |s| s.a) };
            let rb = src.last().map_or(ra, |s| s.b);
            let end_x = segs.last().map_or(if n == 0 { px } else { cx }, |s| (s.x + s.w) as usize);
            let mut row = VRow::new(ra, rb);
            row.segs = if n == 0 { prefix.clone() } else { cont_prefix.to_vec() };
            row.segs.extend(segs);
            row.scale = scale;
            row.fill = fill;
            row.end_x = end_x as u16;
            rows.push(row);
            let _ = nrows;
        }
        if let Some(r) = rows.last_mut() {
            r.b = b;
        }
        rows
    }

    // ------------------------------------------------------------------ dispatcher
    fn layout_line(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, reveal: Option<usize>, active: Option<Pos>) -> Vec<VRow> {
        let block_active = active.is_some();
        let th = ctx.theme;
        let info = &doc.infos[i];
        let text = lines[i].as_str();
        let blk = info.block.map(|b| &doc.blocks[b]);
        let w = ctx.width;
        if ctx.mode == Mode::Source {
            return self.source_line(ctx, lines, doc, i, reveal);
        }
        let active_line = reveal.is_some();
        if info.kind == LineKind::Blank {
            return vec![VRow::new(0, text.len())];
        }
        if let Some(b) = blk {
            match b.kind {
                BlockKind::Code => return self.code_line(ctx, lines, doc, i, b, block_active),
                BlockKind::Math => return self.math_line(ctx, lines, i, b, block_active),
                BlockKind::Front => return self.front_line(ctx, lines, i, b, block_active),
                // the alignment row is edited as source; everything else stays a grid
                BlockKind::Table if i == b.start + 1 && reveal.is_some() => return self.table_raw_line(ctx, lines, doc, i, reveal),
                BlockKind::Table => return self.table_line(ctx, lines, i, b, active),
                _ => {}
            }
        }
        match info.kind {
            LineKind::Comment => {
                if ctx.mode == Mode::Read {
                    return vec![];
                }
                let atoms = self.raw_atoms(text, 0, text.len(), Style::fg(th.faint).italic());
                return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, w);
            }
            LineKind::SetextRule => {
                if block_active {
                    let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
                    return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, w);
                }
                return vec![];
            }
            LineKind::Hr => {
                if active_line {
                    let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
                    return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, w);
                }
                let mut r = VRow::new(0, text.len());
                r.segs.push(Seg {
                    x: 0,
                    w: w as u16,
                    text: "─".repeat(w),
                    style: Style::fg(th.border),
                    a: 0,
                    b: text.len(),
                    src: true,
                    run: false,
                    scale: 1,
                    units: 0,
                    n: 0,
                    d: 0,
                    icon: None,
                });
                r.end_x = 0;
                return vec![r];
            }
            _ => {}
        }
        let mut rows = self.flow_line(ctx, lines, doc, i, reveal, block_active);
        // rounded tinted box around callouts
        if let Some(b) = blk.filter(|b| b.kind == BlockKind::Callout && th.rgb) {
            let tint = th.tint(th.callout_color(&b.callout_kind), 0.10);
            let folded = b.fold == b'-' && !block_active;
            if folded && i != b.start {
                return rows;
            }
            if i == b.start {
                rows.insert(0, edge_row(w, tint, true));
            }
            if i == b.end - 1 || (folded && i == b.start) {
                rows.push(edge_row(w, tint, false));
            }
        }
        rows
    }

    // ------------------------------------------------------------------ flowing text
    fn flow_line(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, reveal: Option<usize>, block_active: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let info = &doc.infos[i];
        let text = lines[i].as_str();
        let blk = info.block.map(|b| &doc.blocks[b]);
        let active_line = reveal.is_some();
        let mut prefix: Vec<Seg> = vec![];
        let mut cont_prefix: Vec<Seg> = vec![];
        let mut x: usize = 0;
        let mut fill = None;
        let mut base = Style::new();
        let mut callout_col = None;
        let src_seg = |x: usize, w: usize, t: &str, st: Style, a: usize, b: usize| Seg {
            x: x as u16,
            w: w as u16,
            text: t.to_string(),
            style: st,
            a,
            b,
            src: true,
            run: false,
            scale: 1,
            units: 0,
            n: 0,
            d: 0,
            icon: None,
        };
        let deco_seg = |x: usize, w: usize, t: &str, st: Style| Seg {
            x: x as u16,
            w: w as u16,
            text: t.to_string(),
            style: st,
            a: 0,
            b: 0,
            src: false,
            run: false,
            scale: 1,
            units: 0,
            n: 0,
            d: 0,
            icon: None,
        };

        // quote / callout gutter
        if info.quote_depth > 0 {
            if let Some(b) = blk.filter(|b| b.kind == BlockKind::Callout) {
                let col = th.callout_color(&b.callout_kind);
                callout_col = Some(col);
                fill = Some(Style::bg(th.tint(col, 0.10)));
                if b.fold == b'-' && !block_active && i != b.start {
                    return vec![];
                }
            }
            // callouts are tinted boxes (no bar) when we know the colours;
            // quotes get Obsidian's accent border
            let boxed = callout_col.is_some() && th.rgb;
            let glyph = if boxed {
                " "
            } else if callout_col.is_some() {
                "▌"
            } else {
                "▎"
            };
            let bar = Style::fg(callout_col.unwrap_or(th.accent));
            let raw = active_line && reveal.is_some_and(|r| r <= info.quote_end);
            for (k, ch) in text[..info.quote_end].char_indices() {
                let e = k + ch.len_utf8();
                let seg = if ch == '>' {
                    if raw { src_seg(x, 1, ">", th.marker(), k, e) } else { src_seg(x, 1, glyph, bar, k, e) }
                } else {
                    src_seg(x, 1, " ", Style::new(), k, e)
                };
                prefix.push(seg);
                x += 1;
            }
            cont_prefix =
                prefix.iter().map(|s| deco_seg(s.x as usize, s.w as usize, if s.text == ">" || s.text == glyph { glyph } else { " " }, bar)).collect();
        }

        // callout header
        if info.kind == LineKind::CalloutHead
            && let Some(co) = &info.callout
        {
            let col = callout_col.unwrap_or(th.accent);
            let title_st = Style::fg(col).bold();
            let revealed = active_line && reveal.is_some_and(|r| r <= co.marker_end);
            let mut atoms: Vec<Atom> = vec![];
            if revealed {
                atoms.extend(self.raw_atoms(text, co.marker_start, co.marker_end, th.marker()));
            } else {
                let kind_title = if co.title.is_empty() {
                    let mut c = co.kind.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                } else {
                    String::new()
                };
                // the Lucide icon Obsidian uses, drawn with kitty graphics
                let icon = match (ctx.images, col.rgb()) {
                    (Some(imgs), Some(rgb)) if th.rgb => imgs.icon(crate::images::callout_svg(&co.kind), rgb, 0.8),
                    _ => None,
                };
                match icon {
                    Some(ic) => {
                        let mut at = Atom::repl(co.marker_start, co.marker_end, "  ".into(), title_st);
                        at.icon = Some(ic);
                        atoms.push(at);
                        atoms.push(Atom::repl(co.marker_end, co.marker_end, format!(" {kind_title}"), title_st));
                    }
                    None => atoms.push(Atom::repl(co.marker_start, co.marker_end, format!("{} {kind_title}", callout_icon(&co.kind)), title_st)),
                }
            }
            let (t, _) = self.inline_atoms(ctx, text, co.marker_end, text.len(), title_st, reveal, false);
            // typeset the title (the icon stays a cell image in the prefix)
            if ctx.typo.is_some() && !revealed {
                let mut pre = prefix.clone();
                let mut px = pre.iter().map(|s| (s.x + s.w) as usize).max().unwrap_or(0);
                let mut title_atoms: Vec<Atom> = vec![];
                for at in atoms.iter() {
                    if at.icon.is_some() {
                        let mut seg = src_seg(px, 3, "   ", title_st, at.a, at.b);
                        seg.icon = at.icon.clone();
                        pre.push(seg);
                        px += 3;
                    } else if at.repl.as_deref().is_some_and(|r| r.trim().is_empty()) {
                        pre.push(src_seg(px, 0, "", title_st, at.a, at.b));
                    } else {
                        let mut a2 = at.clone();
                        if let Some(r) = &a2.repl {
                            a2.repl = Some(r.trim_start().to_string());
                        }
                        title_atoms.push(a2);
                    }
                }
                title_atoms.extend(t.iter().cloned());
                if co.fold != 0 {
                    let open = block_active || co.fold == b'+';
                    title_atoms.push(Atom::repl(text.len(), text.len(), if open { "  ▾".into() } else { "  ▸".into() }, Style::fg(th.muted)));
                }
                if let Some(rows) = self.typeset_rows(ctx, i, text, &title_atoms, pre, &cont_prefix, px, None, fill) {
                    return rows;
                }
            }
            atoms.extend(t);
            if co.fold != 0 && !revealed {
                let open = block_active || co.fold == b'+';
                atoms.push(Atom::repl(text.len(), text.len(), if open { "  ▾".into() } else { "  ▸".into() }, Style::fg(th.muted)));
            }
            return self.rows_from(ctx, text, &atoms, prefix, &cont_prefix, info.quote_end, text.len(), None, None, 1, fill, false, ctx.width);
        }

        // list markers, indentation
        let mut lead_end = info.content;
        if let Some(lm) = &info.lst {
            let depth = lm.depth;
            let ind = crate::text::indent_width(&text[info.quote_end..lm.start]);
            let step = ind.checked_div(depth).map_or(0, |s| s.max(1));
            let guides: Vec<usize> = (0..depth).map(|d| x + step * d).collect();
            let gx0 = x;
            for (k, ch) in text[info.quote_end..lm.start].char_indices() {
                let k = k + info.quote_end;
                let cw = if ch == '\t' { TAB_WIDTH - (x - gx0) % TAB_WIDTH } else { 1 };
                let glyph = if guides.contains(&x) { format!("│{}", " ".repeat(cw - 1)) } else { " ".repeat(cw) };
                prefix.push(src_seg(x, cw, &glyph, Style::fg(th.border), k, k + ch.len_utf8()));
                x += cw;
            }
            let sp_end = if lm.task.is_some() { lm.task_start } else { lm.end };
            let reveal_marker = reveal.is_some_and(|r| lm.start <= r && r < lm.marker_end);
            if lm.ordered || reveal_marker {
                let st = if lm.ordered { Style::fg(th.muted) } else { th.marker() };
                let t = &text[lm.start..sp_end];
                let tw = str_width(&t.replace('\t', " "));
                prefix.push(src_seg(x, tw, &t.replace('\t', " "), st, lm.start, sp_end));
                x += tw;
            } else if lm.task.is_some() {
                // tasks show a checkbox instead of a bullet (Obsidian style)
                prefix.push(src_seg(x, 0, "", Style::new(), lm.start, sp_end));
            } else {
                let glyph = BULLETS[depth % BULLETS.len()];
                let srcw = str_width(&text[lm.start..sp_end].replace('\t', " ")).max(2);
                let label = format!("{glyph}{}", " ".repeat(srcw - 1));
                prefix.push(src_seg(x, srcw, &label, Style::fg(th.muted), lm.start, sp_end));
                x += srcw;
            }
            if let Some(tk) = lm.task {
                let reveal_task = reveal.is_some_and(|r| lm.task_start <= r && r < lm.task_start + 3);
                let t = &text[lm.task_start..lm.task_end];
                let tw = str_width(&t.replace('\t', " "));
                if reveal_task {
                    prefix.push(src_seg(x, tw, &t.replace('\t', " "), th.marker(), lm.task_start, lm.task_end));
                } else {
                    let g = TASK_GLYPHS.iter().find(|(c, _)| *c == tk).map_or("☐", |p| p.1);
                    let st = if tk == ' ' { Style::fg(th.muted) } else { Style::fg(th.accent).bold() };
                    // Obsidian's rounded checkbox, as a crisp inline image
                    let icon = match (ctx.images, th.muted.rgb(), th.accent.rgb()) {
                        (Some(imgs), Some(m), Some(a)) if th.rgb => {
                            crate::images::checkbox_svg(tk, m, a, th.canvas.rgb().unwrap_or((255, 255, 255))).and_then(|svg| imgs.icon(&svg, m, 0.82))
                        }
                        _ => None,
                    };
                    match icon {
                        Some(ic) => {
                            let mut seg = src_seg(x, 3, "   ", st, lm.task_start, lm.task_end);
                            seg.icon = Some(ic);
                            prefix.push(seg);
                        }
                        None => prefix.push(src_seg(x, 2, &format!("{g} "), st, lm.task_start, lm.task_end)),
                    }
                }
                let shown = prefix.last().map_or(2, |s| s.w as usize);
                x += if reveal_task { tw } else { shown };
                if matches!(tk, 'x' | 'X' | '-') {
                    base = base.over(Style::fg(th.muted).strike());
                }
            }
            // hanging indent: keep guides, blank out the marker
            for s in &prefix[cont_prefix.len()..] {
                let t = if s.text.starts_with('│') { s.text.clone() } else { " ".repeat(s.w as usize) };
                cont_prefix.push(deco_seg(s.x as usize, s.w as usize, &t, s.style));
            }
            lead_end = lm.end;
        } else if (info.kind == LineKind::Text || info.kind == LineKind::FootDef) && info.content > info.quote_end {
            if info.kind == LineKind::FootDef && !reveal.is_some_and(|r| r < info.content) {
                let id = text[2..info.content].trim_end().trim_end_matches(':').trim_end_matches(']');
                let label = mathtext::superscript(id).unwrap_or_else(|| id.to_string());
                let label = format!("{label} ");
                let lw = str_width(&label);
                prefix.push(src_seg(x, lw, &label, Style::fg(th.accent), info.quote_end, info.content));
                x += lw;
                base = base.over(Style::fg(th.muted));
            } else {
                let x0 = x;
                for (k, ch) in text[info.quote_end..info.content].char_indices() {
                    let k = k + info.quote_end;
                    let cw = if ch == '\t' { TAB_WIDTH - (x - x0) % TAB_WIDTH } else { 1 };
                    prefix.push(src_seg(x, cw, &" ".repeat(cw), Style::new(), k, k + ch.len_utf8()));
                    x += cw;
                }
                for s in &prefix[cont_prefix.len()..] {
                    cont_prefix.push(deco_seg(s.x as usize, s.w as usize, &" ".repeat(s.w as usize), Style::new()));
                }
            }
        }

        // headings
        let scale = 1u8;
        let is_heading = info.kind == LineKind::Heading;
        let big = is_heading && info.level <= 3 && info.quote_depth == 0;
        if is_heading {
            let hst = th.heading_style(info.level);
            base = base.over(hst);
            if ctx.typo.is_some() && info.quote_depth == 0 {
                let atoms = self.heading_atoms(ctx, text, info, active_line, base, reveal);
                if let Some(rows) = self.typeset_rows(ctx, i, text, &atoms, vec![], &[], 0, Some(info.level), fill) {
                    return rows;
                }
            }
            if big
                && ctx.heading_mode == HeadingMode::Image
                && !active_line
                && let (Some(imgs), Some((r, g, b))) = (ctx.images, hst.fg.rgb().or(th.canvas.rgb().map(|_| (0, 0, 0))).or(Some((200, 200, 200))))
            {
                let plain = if info.setext { md::inline_text(text) } else { md::inline_text(text[info.content..].trim_end().trim_end_matches('#').trim_end()) };
                let col = if hst.fg.rgb().is_some() {
                    (r, g, b)
                } else if th.dark {
                    (230, 230, 230)
                } else {
                    (20, 20, 20)
                };
                if let ImgRef::Ready(img) = imgs.heading(&plain, info.level, (ctx.width.saturating_sub(x)) as u32, col) {
                    let mut out = vec![];
                    for r in 0..img.data.rows {
                        let mut row = VRow::new(0, text.len());
                        row.kind = RowKind::Image;
                        row.cursorable = r == 0;
                        row.image = Some((img.clone(), r, x as u16));
                        row.end_x = x as u16;
                        out.push(row);
                    }
                    return out;
                }
            }
            if big && ctx.heading_mode == HeadingMode::Osc66 {
                // multicell heading: H1 2×, H2 1.6×, H3 1.33× (kitty text sizing)
                let mut atoms = vec![];
                if !info.setext {
                    let mstart = crate::text::leading_ws(text);
                    if mstart > 0 {
                        atoms.push(Atom::hidden(0, mstart));
                    }
                    if active_line {
                        atoms.extend(self.raw_atoms(text, mstart, info.content, th.marker().bold()));
                    } else if info.content > mstart {
                        atoms.push(Atom::hidden(mstart, info.content));
                    }
                }
                let mut end = text.len();
                if !info.setext && !active_line {
                    let t = text.trim_end();
                    let j = t.trim_end_matches('#').len();
                    if j < t.len() && (j == info.content || t[..j].ends_with([' ', '\t'])) {
                        end = t[..j].trim_end().len().max(info.content);
                    }
                }
                let start = if info.setext { crate::text::leading_ws(text) } else { info.content };
                if info.setext && start > 0 {
                    atoms.push(Atom::hidden(0, start));
                }
                atoms.extend(self.inline_atoms(ctx, text, start, end, base, reveal, false).0);
                if end < text.len() {
                    atoms.push(Atom::hidden(end, text.len()));
                }
                return multicell_rows(text, &atoms, heading_scale(info.level), reveal, ctx.width);
            }
            if !info.setext {
                let mstart = info.quote_end + crate::text::leading_ws(&text[info.quote_end..]);
                if mstart > info.quote_end {
                    prefix.push(src_seg(x, 0, "", Style::new(), info.quote_end, mstart));
                }
                if active_line {
                    let t = &text[mstart..info.content];
                    let tw = str_width(&t.replace('\t', " "));
                    let mut seg = src_seg(x, tw * scale as usize, &t.replace('\t', " "), th.marker().bold(), mstart, info.content);
                    seg.scale = scale;
                    prefix.push(seg);
                } else {
                    prefix.push(src_seg(x, 0, "", Style::new(), mstart, info.content));
                }
            }
        }
        let mut end = text.len();
        if is_heading && !info.setext && !active_line {
            let t = text.trim_end();
            let j = t.trim_end_matches('#').len();
            if j < t.len() && (j == info.content || t[..j].ends_with([' ', '\t'])) {
                end = t[..j].trim_end().len().max(info.content);
            }
        }
        let (mut atoms, spans) = self.inline_atoms(ctx, text, lead_end, end, base, reveal, false);
        if end < text.len() {
            atoms.push(Atom::hidden(end, text.len()));
        }
        let cont_x = prefix.iter().map(|s| (s.x + s.w) as usize).max().unwrap_or(0);
        if ctx.typo.is_some() && !is_heading && !atoms.is_empty() {
            let x = if is_heading { 0 } else { cont_x };
            if let Some(rows) = self.typeset_rows(ctx, i, text, &atoms, prefix.clone(), &cont_prefix, x, None, fill) {
                let img_x = if info.lst.is_some() || info.quote_depth > 0 { cont_x } else { 0 };
                let rows = self.attach_images(ctx, text, &spans, rows, reveal, img_x);
                return self.attach_embeds(ctx, text, &spans, rows, reveal, img_x);
            }
        }
        let mut rows = self.rows_from(
            ctx,
            text,
            &atoms,
            prefix,
            &cont_prefix,
            0,
            text.len(),
            None,
            Some(if is_heading { 0 } else { cont_x }),
            scale,
            fill,
            false,
            ctx.width,
        );
        if big && ctx.heading_mode == HeadingMode::Image {
            rows.push(VRow::deco());
        }
        if is_heading && info.level == 1 && ctx.heading_mode == HeadingMode::Plain && info.quote_depth == 0 {
            let mut r = VRow::deco();
            r.segs.push(Seg {
                x: 0,
                w: ctx.width as u16,
                text: "━".repeat(ctx.width),
                style: Style::fg(th.border),
                a: 0,
                b: 0,
                src: false,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            rows.push(r);
        }
        let img_x = if info.lst.is_some() || info.quote_depth > 0 { cont_x } else { 0 };
        let rows = self.attach_images(ctx, text, &spans, rows, reveal, img_x);
        self.attach_embeds(ctx, text, &spans, rows, reveal, img_x)
    }

    /// `![[note]]` / `![[note#section]]`: show the embedded note in a panel.
    fn attach_embeds(&self, ctx: &Ctx, text: &str, spans: &[Span], mut rows: Vec<VRow>, reveal: Option<usize>, px: usize) -> Vec<VRow> {
        let th = ctx.theme;
        let embeds: Vec<&Span> = spans.iter().filter(|s| s.kind == SpanKind::Embed && !is_image_target(&s.target)).collect();
        if embeds.is_empty() || ctx.mode == Mode::Source {
            return rows;
        }
        let width = ctx.width.saturating_sub(px + 2).max(4);
        // an embed alone on its line replaces the line (like images)
        let mut rest = text.to_string();
        for s in &embeds {
            rest.replace_range(s.start..s.end, &" ".repeat(s.end - s.start));
        }
        let revealed = embeds.iter().any(|s| reveal.is_some_and(|r| s.start <= r && r <= s.end));
        let mut first_cursorable = false;
        if rest.trim().is_empty() && !revealed {
            rows.retain(|r| r.kind == RowKind::Image);
            first_cursorable = rows.is_empty();
        }
        let fill = Some(Style::bg(th.sidebar));
        let bar = Style::fg(th.accent);
        let deco = |x: usize, t: &str, st: Style| Seg {
            x: x as u16,
            w: str_width(t) as u16,
            text: t.to_string(),
            style: st,
            a: 0,
            b: 0,
            src: false,
            run: false,
            scale: 1,
            units: 0,
            n: 0,
            d: 0,
            icon: None,
        };
        for s in embeds {
            let _ = reveal;
            let Some(lines) = (ctx.embed)(&s.target, &s.anchor) else { continue };
            let sub_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            let doc = md::parse_document(&sub_lines);
            let mut body: Vec<(String, Style)> = vec![];
            for (i, l) in sub_lines.iter().enumerate() {
                let info = &doc.infos[i];
                let (t, st) = match info.kind {
                    LineKind::Front | LineKind::FrontFence => continue,
                    LineKind::Heading => (md::inline_text(l[info.content..].trim_end_matches('#')), th.heading_style(info.level)),
                    LineKind::Fence => continue,
                    LineKind::Code => (l.to_string(), Style::fg(th.code_fg)),
                    LineKind::Blank => (String::new(), Style::new()),
                    _ => match &info.lst {
                        Some(lm) => {
                            let mark = match lm.task {
                                Some(' ') => "☐ ",
                                Some(_) => "☑ ",
                                None if lm.ordered => "",
                                None => "• ",
                            };
                            let pre = if lm.ordered { &l[lm.start..lm.end] } else { "" };
                            (format!("{}{mark}{pre}{}", "  ".repeat(lm.depth), md::inline_text(&l[lm.end..])), Style::new())
                        }
                        None => (md::inline_text(&l[info.content.min(l.len())..]), Style::new()),
                    },
                };
                body.push((t, st));
            }
            while body.last().is_some_and(|b| b.0.is_empty()) {
                body.pop();
            }
            while body.first().is_some_and(|b| b.0.is_empty()) {
                body.remove(0);
            }
            let title = if s.anchor.is_empty() { s.target.clone() } else { format!("{} › {}", s.target, s.anchor) };
            // the embedded note's own title heading would just repeat the header
            let last_part = title.rsplit(" › ").next().unwrap_or(&title).to_string();
            if body.first().is_some_and(|b| b.0.eq_ignore_ascii_case(&last_part) || b.0.eq_ignore_ascii_case(&s.target)) {
                body.remove(0);
                while body.first().is_some_and(|b| b.0.is_empty()) {
                    body.remove(0);
                }
            }
            let mut head = VRow::new(0, text.len());
            head.kind = if first_cursorable { RowKind::Text } else { RowKind::Deco };
            head.cursorable = first_cursorable;
            first_cursorable = false;
            head.fill = fill;
            head.segs.push(deco(px, "▍", bar));
            head.segs.push(deco(px + 2, &format!("↳ {title}"), Style::fg(th.muted).bold()));
            rows.push(head);
            let max_rows = 14usize;
            let mut emitted = 0;
            'outer: for (t, st) in &body {
                // simple word wrap
                let mut cur = String::new();
                let mut chunks = vec![];
                for word in t.split(' ') {
                    if !cur.is_empty() && str_width(&cur) + 1 + str_width(word) > width {
                        chunks.push(std::mem::take(&mut cur));
                    }
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(word);
                }
                chunks.push(cur);
                for c in chunks {
                    if emitted >= max_rows {
                        let mut r = VRow::new(text.len(), text.len());
                        r.kind = RowKind::Deco;
                        r.cursorable = false;
                        r.fill = fill;
                        r.segs.push(deco(px, "▍", bar));
                        r.segs.push(deco(px + 2, "…", Style::fg(th.faint)));
                        rows.push(r);
                        break 'outer;
                    }
                    let mut r = VRow::new(text.len(), text.len());
                    r.kind = RowKind::Deco;
                    r.cursorable = false;
                    r.fill = fill;
                    r.segs.push(deco(px, "▍", bar));
                    let c = crate::text::truncate(&c, width);
                    r.segs.push(deco(px + 2, &c, *st));
                    rows.push(r);
                    emitted += 1;
                }
            }
        }
        rows
    }

    fn attach_images(&self, ctx: &Ctx, text: &str, spans: &[Span], rows: Vec<VRow>, reveal: Option<usize>, px: usize) -> Vec<VRow> {
        let th = ctx.theme;
        let imgs: Vec<&Span> = spans.iter().filter(|s| matches!(s.kind, SpanKind::Image | SpanKind::Embed) && is_image_target(&s.target)).collect();
        let Some(store) = ctx.images else { return rows };
        if imgs.is_empty() {
            return rows;
        }
        let mut rest = text.to_string();
        for s in &imgs {
            rest.replace_range(s.start..s.end, &" ".repeat(s.end - s.start));
        }
        let revealed = imgs.iter().any(|s| reveal.is_some_and(|r| s.start <= r && r <= s.end));
        let image_only = rest.trim().is_empty() || rest.trim_start().starts_with(['-', '*', '>']) && rest.trim().len() <= 2;
        let mut out: Vec<VRow> = if image_only && !revealed {
            rows.iter().filter(|r| r.kind != RowKind::Text || r.segs.iter().any(|s| !s.src)).take(0).cloned().collect()
        } else {
            rows
        };
        let maxc = ctx.width.saturating_sub(px).max(1) as u32;
        for s in imgs {
            let r = store.get(&s.target, maxc, ctx.max_image_rows, s.width, s.height);
            match r {
                ImgRef::Ready(img) => {
                    for k in 0..img.data.rows {
                        let mut row = VRow::new(if out.is_empty() { 0 } else { text.len() }, text.len());
                        row.kind = RowKind::Image;
                        row.cursorable = out.is_empty();
                        row.image = Some((img.clone(), k, px as u16));
                        row.end_x = px as u16;
                        out.push(row);
                    }
                }
                other => {
                    let (icon, msg) = match other {
                        ImgRef::Pending => ("⧗", "loading…".to_string()),
                        ImgRef::Error(e) => ("⚠", e),
                        _ => unreachable!(),
                    };
                    let name = if s.label.is_empty() { s.target.clone() } else { s.label.clone() };
                    let label = crate::text::truncate(&format!("{icon} {name} — {msg}"), maxc as usize);
                    let mut row = VRow::new(if out.is_empty() { 0 } else { text.len() }, text.len());
                    row.cursorable = out.is_empty();
                    if !row.cursorable {
                        row.kind = RowKind::Deco;
                    }
                    row.segs.push(Seg {
                        x: px as u16,
                        w: str_width(&label) as u16,
                        text: label,
                        style: Style::fg(th.faint).italic(),
                        a: 0,
                        b: 0,
                        src: false,
                        run: false,
                        scale: 1,
                        units: 0,
                        n: 0,
                        d: 0,
                        icon: None,
                    });
                    row.end_x = px as u16;
                    out.push(row);
                }
            }
        }
        out
    }

    // ------------------------------------------------------------------ code
    fn highlight_block(&self, lines: &[String], b: &md::Block) -> Rc<Vec<Vec<(usize, usize, Tok)>>> {
        let body_end = if b.closed { b.end - 1 } else { b.end };
        let body = &lines[(b.start + 1).min(body_end)..body_end];
        let key = h64(&(&b.lang, body));
        if let Some(h) = self.hl.borrow().get(&key) {
            return h.clone();
        }
        let code = body.join("\n");
        let h = Rc::new(highlight::highlight(&b.lang, &code));
        let mut m = self.hl.borrow_mut();
        if m.len() > 512 {
            m.clear();
        }
        m.insert(key, h.clone());
        h
    }

    fn code_line(&self, ctx: &Ctx, lines: &[String], _doc: &Document, i: usize, b: &md::Block, active: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let text = lines[i].as_str();
        let fill = Some(Style::bg(th.panel));
        let w = ctx.width;
        let is_open = i == b.start;
        let is_close = b.closed && i == b.end - 1;
        let pad = Seg { x: 0, w: 1, text: " ".into(), style: Style::new(), a: 0, b: 0, src: false, run: false, scale: 1, units: 0, n: 0, d: 0, icon: None };
        if is_open || is_close {
            if active || ctx.mode == Mode::Source {
                let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
                return self.rows_from(ctx, text, &atoms, vec![pad], &[], 0, text.len(), Some(1), Some(1), 1, fill, true, w);
            }
            // fence rows become the panel's rounded half-height edges
            let mut r = if th.rgb { edge_row(w, th.panel, is_open) } else { VRow::new(0, text.len()) };
            r.kind = RowKind::Text;
            r.cursorable = true;
            r.a = 0;
            r.b = text.len();
            r.segs.push(Seg {
                x: 1,
                w: 0,
                text: String::new(),
                style: Style::new(),
                a: 0,
                b: text.len(),
                src: true,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            r.end_x = 1;
            return vec![r];
        }
        let hl = self.highlight_block(lines, b);
        let toks = hl.get(i - b.start - 1).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut atoms = self.raw_atoms(text, 0, text.len(), Style::new());
        if !toks.is_empty() {
            let mut ti = 0;
            for at in &mut atoms {
                while ti < toks.len() && toks[ti].1 <= at.a {
                    ti += 1;
                }
                if ti < toks.len() && toks[ti].0 <= at.a {
                    let col = match toks[ti].2 {
                        Tok::Keyword => th.syn_keyword,
                        Tok::String => th.syn_string,
                        Tok::Comment => th.syn_comment,
                        Tok::Number => th.syn_number,
                        Tok::Function => th.syn_function,
                        Tok::Type => th.syn_type,
                        Tok::Constant => th.syn_constant,
                        Tok::Punct => th.syn_punct,
                        Tok::Plain => th.fg,
                    };
                    at.style = Style::fg(col);
                    if toks[ti].2 == Tok::Comment {
                        at.style = at.style.italic();
                    }
                }
            }
        }
        let mut rows = self.rows_from(ctx, text, &atoms, vec![pad.clone()], &[pad], 0, text.len(), Some(1), Some(1), 1, fill, true, w.saturating_sub(1));
        if i == b.start + 1 && !active && !b.lang.is_empty() && ctx.mode != Mode::Source {
            let lw = str_width(&b.lang);
            if let Some(r0) = rows.first_mut() {
                if (r0.end_x as usize) + lw + 3 < w {
                    r0.segs.push(Seg {
                        x: (w - lw - 1) as u16,
                        w: lw as u16,
                        text: b.lang.clone(),
                        style: Style::fg(th.faint),
                        a: 0,
                        b: 0,
                        src: false,
                        run: false,
                        scale: 1,
                        units: 0,
                        n: 0,
                        d: 0,
                        icon: None,
                    });
                }
            }
        }
        rows
    }

    // ------------------------------------------------------------------ math
    fn math_line(&self, ctx: &Ctx, lines: &[String], i: usize, b: &md::Block, active: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let text = lines[i].as_str();
        if active {
            let st = if text.trim() == "$$" { th.marker() } else { Style::fg(th.code_fg) };
            let atoms = self.raw_atoms(text, 0, text.len(), st);
            let mut rows = self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, ctx.width);
            if i == b.end - 1 {
                rows.extend(self.math_render(ctx, lines, b, i, true));
            }
            return rows;
        }
        if i != b.start {
            return vec![];
        }
        self.math_render(ctx, lines, b, i, false)
    }

    fn math_render(&self, ctx: &Ctx, lines: &[String], b: &md::Block, i: usize, preview: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let src = lines[b.start..b.end].join("\n");
        let mut tex = src.trim();
        tex = tex.strip_prefix("$$").unwrap_or(tex);
        tex = tex.strip_suffix("$$").unwrap_or(tex);
        let tex = tex.trim();
        let text = &lines[i];
        let mut rows = vec![];
        let parts: Vec<&str> = if tex.is_empty() { vec![""] } else { tex.split("\\\\").collect() };
        let fill = if preview { None } else { Some(Style::bg(th.tint(th.accent, 0.05))) };
        for (n, ln) in parts.iter().enumerate() {
            let u = mathtext::convert(ln.trim().trim_start_matches("\\begin{aligned}").trim_end_matches("\\end{aligned}"));
            let u = u.trim().to_string();
            let uw = str_width(&u);
            let x = ctx.width.saturating_sub(uw) / 2;
            let mut r = VRow::new(0, text.len());
            r.cursorable = n == 0 && !preview;
            if !r.cursorable {
                r.kind = RowKind::Deco;
            }
            r.segs.push(Seg {
                x: x as u16,
                w: uw as u16,
                text: u,
                style: Style::new().italic(),
                a: 0,
                b: 0,
                src: false,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            r.fill = fill;
            r.end_x = x as u16;
            rows.push(r);
        }
        rows
    }

    // ------------------------------------------------------------------ frontmatter
    fn front_line(&self, ctx: &Ctx, lines: &[String], i: usize, b: &md::Block, active: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let text = lines[i].as_str();
        let w = ctx.width;
        // Obsidian's Properties: a plain list under a muted heading (no box)
        let fill: Option<Style> = None;
        let is_key = |l: &str| l.contains(':') && !l.starts_with([' ', '\t', '-']);
        if active {
            if i == b.start || i == b.end - 1 {
                let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
                return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, w);
            }
            let mut atoms = self.raw_atoms(text, 0, text.len(), Style::new());
            if is_key(text) {
                let colon = text.find(':').unwrap();
                for a in atoms.iter_mut() {
                    a.style = if a.a <= colon { Style::fg(th.accent) } else { Style::new() };
                }
            } else {
                for a in atoms.iter_mut() {
                    a.style = Style::fg(th.muted);
                }
            }
            return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, w);
        }
        let mut r = VRow::new(0, text.len());
        r.fill = fill;
        if i == b.start
            && ctx.typo.is_some()
            && let Some(rows) =
                self.typeset_rows(ctx, i, text, &[Atom::repl(0, text.len(), "Properties".into(), Style::fg(th.muted).bold())], vec![], &[], 0, None, fill)
        {
            return rows;
        }
        if i == b.start {
            let lab = "Properties";
            r.segs.push(Seg {
                x: 0,
                w: str_width(lab) as u16,
                text: lab.into(),
                style: Style::fg(th.muted).bold(),
                a: 0,
                b: text.len(),
                src: true,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            return vec![r];
        }
        if i == b.end - 1 {
            r.segs.push(Seg {
                x: 0,
                w: 0,
                text: String::new(),
                style: Style::new(),
                a: 0,
                b: text.len(),
                src: true,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            return vec![r];
        }
        let kw =
            lines[b.start + 1..b.end - 1].iter().filter(|l| is_key(l)).map(|l| str_width(l.split(':').next().unwrap().trim())).max().unwrap_or(4).min(18) + 3;
        if is_key(text) {
            let colon = text.find(':').unwrap();
            let key = text[..colon].trim();
            let mut va = colon + 1;
            while va < text.len() && text.as_bytes()[va] == b' ' {
                va += 1;
            }
            let lab = crate::text::pad_to(key, kw);
            let prefix = vec![Seg {
                x: 0,
                w: kw as u16,
                text: lab,
                style: Style::fg(th.muted),
                a: 0,
                b: va,
                src: true,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            }];
            let val = &text[va..];
            let atoms = if val.starts_with('[') && val.ends_with(']') {
                let mut v = self.raw_atoms(text, va, text.len(), Style::fg(th.tag_fg));
                for a in v.iter_mut() {
                    let c = &text[a.a..a.b];
                    if c == "[" || c == "]" {
                        *a = Atom::hidden(a.a, a.b);
                    } else if c == "," {
                        a.style = Style::fg(th.faint);
                    }
                }
                v
            } else {
                self.inline_atoms(ctx, text, va, text.len(), Style::new(), None, false).0
            };
            if let Some(t) = ctx.typo {
                // proportional: key, then the value at a tab stop (aligned column)
                let mut ta = vec![Atom::repl(0, va, key.to_string(), Style::fg(th.muted))];
                let mut first = true;
                for mut at in atoms.iter().cloned() {
                    if first && !at.hidden {
                        at.tab_to = Some((kw as f32 * t.cell_w as f32 * 0.85).round());
                        first = false;
                    }
                    ta.push(at);
                }
                if let Some(rows) = self.typeset_rows(ctx, i, text, &ta, vec![], &[], 0, None, fill) {
                    return rows;
                }
            }
            return self.rows_from(ctx, text, &atoms, prefix, &[], 0, text.len(), Some(kw), Some(kw), 1, fill, false, w);
        }
        let atoms = self.raw_atoms(text, 0, text.len(), Style::fg(th.muted));
        self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), Some(kw), Some(kw), 1, fill, false, w)
    }

    // ------------------------------------------------------------------ tables
    fn table_raw_line(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, reveal: Option<usize>) -> Vec<VRow> {
        let th = ctx.theme;
        let text = lines[i].as_str();
        let info = &doc.infos[i];
        if info.kind == LineKind::TableDelim {
            let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
            return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, ctx.width);
        }
        let base = if info.kind == LineKind::TableHead { Style::new().bold() } else { Style::new() };
        let mut atoms = vec![];
        let mut k = 0;
        for (a, b) in md::split_table_row(text) {
            atoms.extend(self.raw_atoms(text, k, a, th.marker()));
            atoms.extend(self.inline_atoms(ctx, text, a, b, base, reveal, false).0);
            k = b;
        }
        atoms.extend(self.raw_atoms(text, k, text.len(), th.marker()));
        self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, ctx.width)
    }

    /// Cells and column widths. `edit` is the caret (row offset, col) when it
    /// is in this table: that row reveals markup around it, and the widths
    /// grow to fit, as Obsidian's table editor shows the cell being edited.
    fn table_model(&self, ctx: &Ctx, lines: &[String], b: &md::Block, edit: Option<(usize, usize)>) -> Rc<TableModel> {
        let key = h64(&(&lines[b.start..b.end], ctx.width, ctx.mode, ctx.theme.name.as_str(), ctx.epoch, edit));
        if let Some(m) = self.tables.borrow().get(&key) {
            return m.clone();
        }
        let ncols = b.ncols.max(1);
        let mut cells = HashMap::new();
        for ln in b.start..b.end {
            if ln == b.start + 1 {
                continue;
            }
            let text = &lines[ln];
            let base = if ln == b.start { Style::new().bold() } else { Style::new() };
            let reveal = edit.filter(|e| e.0 == ln - b.start).map(|e| e.1);
            let mut row: Vec<Vec<Atom>> =
                md::split_table_row(text).into_iter().take(ncols).map(|(a, e)| self.inline_atoms(ctx, text, a, e, base, reveal, false).0).collect();
            row.resize(ncols, vec![]);
            // keyed by row offset: the model is shared by identical tables anywhere
            cells.insert(ln - b.start, row);
        }
        let mut natural = vec![1usize; ncols];
        for row in cells.values() {
            for (c, at) in row.iter().enumerate() {
                natural[c] = natural[c].max(at.iter().map(|x| x.w as usize).sum());
            }
        }
        let avail = ctx.width.saturating_sub(3 * ncols + 1).max(ncols * 3);
        let mut widths = natural;
        while widths.iter().sum::<usize>() > avail {
            let m = *widths.iter().max().unwrap();
            if m <= 3 {
                break;
            }
            let over = widths.iter().sum::<usize>() - avail;
            let nmax = widths.iter().filter(|&&w| w == m).count();
            let second = widths.iter().copied().filter(|&w| w < m).max().unwrap_or(3).max(3);
            let cut = (over.div_ceil(nmax)).min(m - second).max(1);
            for w in widths.iter_mut() {
                if *w == m {
                    *w -= cut;
                }
            }
        }
        let mut aligns = b.aligns.clone();
        aligns.resize(ncols, b'l');
        let spans = (b.start..b.end).filter(|&ln| ln != b.start + 1).map(|ln| (ln - b.start, md::split_table_row(&lines[ln]))).collect();
        let m = Rc::new(TableModel { widths, aligns, cells, spans });
        let mut t = self.tables.borrow_mut();
        // keyed by caret position while editing, so bound it
        if t.len() > 256 {
            t.clear();
        }
        t.insert(key, m.clone());
        m
    }

    fn table_line(&self, ctx: &Ctx, lines: &[String], i: usize, b: &md::Block, active: Option<Pos>) -> Vec<VRow> {
        let th = ctx.theme;
        let edit = active.filter(|p| (b.start..b.end).contains(&p.line)).map(|p| (p.line - b.start, p.col));
        let m = self.table_model(ctx, lines, b, edit);
        let text = lines[i].as_str();
        let bst = Style::fg(th.border);
        let border = |l: &str, mid: &str, r: &str| -> Seg {
            let s = format!("{l}{}{r}", m.widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>().join(mid));
            Seg { x: 0, w: str_width(&s) as u16, text: s, style: bst, a: 0, b: 0, src: false, run: false, scale: 1, units: 0, n: 0, d: 0, icon: None }
        };
        if i == b.start + 1 {
            // not a caret stop: Up/Down step over it (with the caret on it,
            // the line is shown as source instead)
            let mut r = VRow { cursorable: false, kind: RowKind::Deco, ..VRow::new(0, text.len()) };
            r.segs.push(border("├", "┼", "┤"));
            r.segs.push(Seg {
                x: 0,
                w: 0,
                text: String::new(),
                style: Style::new(),
                a: 0,
                b: text.len(),
                src: true,
                run: false,
                scale: 1,
                units: 0,
                n: 0,
                d: 0,
                icon: None,
            });
            return vec![r];
        }
        let mut out = vec![];
        if i == b.start {
            let mut r = VRow::deco();
            r.segs.push(border("┌", "┬", "┐"));
            out.push(r);
        }
        let empty = vec![];
        let row_cells = m.cells.get(&(i - b.start)).unwrap_or(&empty);
        let no_spans = vec![];
        let spans = m.spans.get(&(i - b.start)).unwrap_or(&no_spans);
        let wrapped: Vec<Vec<Vec<Seg>>> = row_cells.iter().enumerate().map(|(c, at)| wrap_atoms(text, at, m.widths[c], 0, 0, 1, false)).collect();
        let h = wrapped.iter().map(|w| w.len()).max().unwrap_or(1).max(1);
        for r in 0..h {
            let mut row = VRow::new(0, text.len());
            let mut x = 0usize;
            row.segs.push(Seg { x: 0, w: 1, text: "│".into(), style: bst, a: 0, b: 0, src: false, run: false, scale: 1, units: 0, n: 0, d: 0, icon: None });
            x += 1;
            for c in 0..m.widths.len() {
                let parts: &[Seg] = wrapped.get(c).and_then(|w| w.get(r)).map(|v| v.as_slice()).unwrap_or(&[]);
                let used: usize = parts.iter().map(|s| s.w as usize).sum();
                let pad = match m.aligns[c] {
                    b'r' => m.widths[c].saturating_sub(used),
                    b'c' => m.widths[c].saturating_sub(used) / 2,
                    _ => 0,
                };
                let mut cs = CellStops { x0: (x + 1) as u16, x1: (x + 1 + m.widths[c]) as u16, stops: vec![] };
                for s in parts {
                    let mut s = s.clone();
                    s.x += (x + 1 + pad) as u16;
                    seg_stops(&s, text, &mut cs);
                    row.segs.push(s);
                }
                if let Some(&(a, e)) = spans.get(c) {
                    // the cell's ends are always reachable, even when empty
                    if r == 0 && !cs.stops.iter().any(|s| s.0 == a) {
                        cs.stops.insert(0, (a, (x + 1 + pad) as u16));
                    }
                    if r + 1 == wrapped.get(c).map_or(1, |w| w.len()) {
                        cs.push(e, (x + 1 + pad + used) as u16);
                    }
                }
                if !cs.stops.is_empty() {
                    row.cells.push(cs);
                }
                x += m.widths[c] + 2;
                row.segs.push(Seg {
                    x: x as u16,
                    w: 1,
                    text: "│".into(),
                    style: bst,
                    a: 0,
                    b: 0,
                    src: false,
                    run: false,
                    scale: 1,
                    units: 0,
                    n: 0,
                    d: 0,
                    icon: None,
                });
                x += 1;
            }
            if i == b.start {
                row.fill = Some(Style::bg(th.sidebar));
                row.fill_x0 = 1;
                row.fill_x1 = Some(x.saturating_sub(1) as u16);
            }
            row.end_x = x.saturating_sub(1) as u16;
            out.push(row);
        }
        if i == b.end - 1 {
            let mut r = VRow::deco();
            r.segs.push(border("└", "┴", "┘"));
            out.push(r);
        }
        out
    }

    // ------------------------------------------------------------------ source mode
    fn source_line(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, reveal: Option<usize>) -> Vec<VRow> {
        let th = ctx.theme;
        let info = &doc.infos[i];
        let text = lines[i].as_str();
        if let Some(b) = info.block.map(|b| &doc.blocks[b])
            && b.kind == BlockKind::Code
        {
            return self.code_line(ctx, lines, doc, i, b, true);
        }
        let st = match info.kind {
            LineKind::Front | LineKind::FrontFence => Some(Style::fg(th.muted)),
            LineKind::Math | LineKind::MathFence => Some(Style::fg(th.code_fg)),
            LineKind::Comment => Some(Style::fg(th.faint).italic()),
            LineKind::TableDelim | LineKind::Hr | LineKind::SetextRule => Some(th.marker()),
            _ => None,
        };
        if let Some(st) = st {
            let atoms = self.raw_atoms(text, 0, text.len(), st);
            return self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, None, 1, None, false, ctx.width);
        }
        let base = if info.kind == LineKind::Heading { th.heading_style(info.level) } else { Style::new() };
        let pre_st = if info.lst.is_some() {
            Style::fg(th.accent)
        } else {
            th.marker().over(if info.kind == LineKind::Heading { Style::new().bold() } else { Style::new() })
        };
        let mut atoms = self.raw_atoms(text, 0, info.content, pre_st);
        let cont: usize = if info.lst.is_some() { atoms.iter().map(|a| a.w as usize).sum() } else { 0 };
        atoms.extend(self.inline_atoms(ctx, text, info.content, text.len(), base, reveal, true).0);
        self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, Some(cont), 1, None, false, ctx.width)
    }
}

fn return_false() {}

/// Caret stops of a laid-out segment (cell-relative x already applied).
fn seg_stops(s: &Seg, line: &str, cs: &mut CellStops) {
    if !s.src {
        return;
    }
    if !s.run {
        cs.push(s.a, s.x);
        cs.push(s.b, s.x + s.w);
        return;
    }
    let (mut k, mut nat) = (s.a, 0usize);
    cs.push(k, s.x);
    for g in line[s.a..s.b].graphemes(true) {
        nat += if g == "\t" { 1 } else { grapheme_width(g) };
        k += g.len();
        cs.push(k, s.x + seg_adv(s, nat).floor() as u16);
    }
}

// ---------------------------------------------------------------------- mapping

/// Grid-table row: (x, distance) of the caret stop nearest to `col`.
fn nearest_stop(row: &VRow, col: usize) -> Option<(usize, usize)> {
    row.cells.iter().flat_map(|c| &c.stops).map(|&(c, x)| (x as usize, c.abs_diff(col))).min_by_key(|&(_, d)| d)
}

/// Whether vertical motion at goal `x` can enter this row. A grid-table row
/// whose cell under `x` has no text on it (a neighbour wrapped further) is
/// skipped, so Up/Down stays in its column.
pub fn row_reaches(row: &VRow, x: usize) -> bool {
    row.cells.is_empty() || row.cells.iter().any(|c| c.x0 as usize <= x && x <= c.x1 as usize)
}

/// Grid-table row: x spans covering source bytes `a..b`, one per cell.
pub fn cell_spans(row: &VRow, a: usize, b: usize) -> Vec<(usize, usize)> {
    row.cells
        .iter()
        .filter_map(|c| {
            let xs = c.stops.iter().filter(|s| a <= s.0 && s.0 <= b).map(|s| s.1 as usize);
            let (lo, hi) = xs.fold((usize::MAX, 0), |(lo, hi), x| (lo.min(x), hi.max(x)));
            (lo < hi).then_some((lo, hi))
        })
        .collect()
}

/// Relative x of a cursor before source byte `col` in this row.
pub fn x_of_col(row: &VRow, line: &str, col: usize) -> usize {
    if !row.cells.is_empty() {
        return nearest_stop(row, col).map_or(row.end_x as usize, |s| s.0);
    }
    if let Some(m) = &row.tmap {
        let li = (0..m.lines.len()).find(|&li| m.anchor_row(li) == row.trow as usize).unwrap_or_else(|| m.line_of(col, line.len()));
        let ix = row.image.as_ref().map_or(0, |i| i.2 as usize);
        return ix + (m.x_of(li, col) / m.cell_w as f32).round() as usize;
    }
    for s in &row.segs {
        if !s.src {
            continue;
        }
        if s.a <= col && col < s.b {
            if s.w == 0 {
                continue;
            }
            if s.run {
                let mut nat = 0usize;
                let mut k = s.a;
                for g in line[s.a..s.b].graphemes(true) {
                    if k >= col {
                        break;
                    }
                    nat += if g == "\t" { 1 } else { grapheme_width(g) };
                    k += g.len();
                }
                return s.x as usize + seg_adv(s, nat).floor() as usize;
            }
            return s.x as usize;
        }
        if s.a >= col && s.w > 0 {
            return s.x as usize;
        }
    }
    row.end_x as usize
}

/// Source byte for a click at relative x.
pub fn col_of_x(row: &VRow, line: &str, x: usize) -> usize {
    if !row.cells.is_empty() {
        let nearest = row.cells.iter().flat_map(|c| &c.stops).min_by_key(|s| (s.1 as usize).abs_diff(x));
        return nearest.map_or(row.a, |s| s.0);
    }
    if let Some(m) = &row.tmap {
        let _ = line;
        let li = (0..m.lines.len()).find(|&li| m.anchor_row(li) == row.trow as usize).unwrap_or(0);
        let ix = row.image.as_ref().map_or(0, |i| i.2 as usize);
        return m.col_at(li, x.saturating_sub(ix) as f32 * m.cell_w as f32);
    }
    let mut best = row.a;
    for s in &row.segs {
        if !s.src || s.w == 0 {
            continue;
        }
        let (sx, sw) = (s.x as usize, s.w as usize);
        if sx <= x && x < sx + sw {
            if s.run {
                let mut nat = 0usize;
                let mut k = s.a;
                for g in line[s.a..s.b].graphemes(true) {
                    let gw = grapheme_width(g).max(1);
                    let gx = sx as f32 + seg_adv(s, nat);
                    let gx2 = sx as f32 + seg_adv(s, nat + gw);
                    if (x as f32) < gx2 - 0.01 {
                        let mid = (gx + gx2) / 2.0;
                        return if x as f32 + 0.5 > mid && gx2 - gx > 1.0 { k + g.len() } else { k };
                    }
                    nat += gw;
                    k += g.len();
                }
                return s.b;
            }
            return if x - sx >= sw.div_ceil(2) && sw > 1 && s.b - s.a <= 1 { s.b } else { s.a.max(if x - sx > sw / 2 { s.b } else { s.a }) };
        }
        if sx + sw <= x {
            best = s.b;
        }
    }
    if !row.last && best >= row.b && row.b > row.a {
        return crate::text::prev_boundary(line, row.b).max(row.a);
    }
    best.min(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn setup(src: &str) -> (Vec<String>, Document) {
        let lines: Vec<String> = src.split('\n').map(String::from).collect();
        let d = md::parse_document(&lines);
        (lines, d)
    }

    fn render(src: &str, width: usize, cursor: Option<(usize, usize)>) -> Vec<String> {
        let (lines, d) = setup(src);
        let th = Theme::ansi();
        let le = |_: &str| true;
        let ctx = Ctx {
            theme: &th,
            width,
            mode: Mode::Live,
            heading_mode: HeadingMode::Plain,
            images: None,
            link_exists: &le,
            embed: &|_: &str, _: &str| None,
            max_image_rows: 10,
            epoch: 0,
            title: None,
            typo: None,
            caret: None,
            sel: None,
            hits: &[],
            cur_hit: None,
        };
        let l = Layout::new();
        let active_block = cursor.and_then(|(ln, _)| d.infos[ln].block);
        let mut out = vec![];
        for i in 0..lines.len() {
            let c = cursor.filter(|c| c.0 == i).map(|c| c.1);
            let ba = d.infos[i].block.is_some() && d.infos[i].block == active_block;
            for r in l
                .rows(
                    &ctx,
                    &lines,
                    &d,
                    i,
                    c,
                    ba.then(|| {
                        let (ln, col) = cursor.unwrap();
                        Pos::new(ln, col)
                    }),
                )
                .iter()
            {
                let mut s = String::new();
                let mut x = 0;
                for seg in &r.segs {
                    while x < seg.x as usize {
                        s.push(' ');
                        x += 1;
                    }
                    s.push_str(&seg.text);
                    x += seg.w as usize;
                }
                out.push(s.trim_end().to_string());
            }
        }
        out
    }

    #[test]
    fn concealment_and_reveal() {
        assert_eq!(render("some **bold** and [link](http://x)", 80, None), ["some bold and link↗"]);
        assert_eq!(render("some **bold** and [link](http://x)", 80, Some((0, 7))), ["some **bold** and link↗"]);
        assert_eq!(render("a [note](other.md) b", 80, None), ["a note b"], "internal links get no arrow");
        assert_eq!(render("# Title", 80, None)[0], "Title");
        assert_eq!(render("# Title", 80, Some((0, 3)))[0], "# Title");
        assert_eq!(render("- item\n  - sub\n- [ ] todo\n- [x] done", 80, None), ["• item", "│ • sub", "☐ todo", "☑ done"]);
    }

    #[test]
    fn wrapping_hanging_indent() {
        let r = render("- alpha beta gamma delta", 14, None);
        assert_eq!(r, ["• alpha beta", "  gamma delta"]);
        let r = render("> quoted text that wraps", 12, None);
        assert_eq!(r, ["▎ quoted", "▎ text that", "▎ wraps"]);
    }

    #[test]
    fn table_render() {
        let r = render("| a | bb |\n|---|:-:|\n| 1 | 2 |", 40, None);
        assert_eq!(r, ["┌───┬────┐", "│ a │ bb │", "├───┼────┤", "│ 1 │ 2  │", "└───┴────┘"]);
        // editing keeps the grid
        let r2 = render("| a | bb |\n|---|:-:|\n| 1 | 2 |", 40, Some((2, 2)));
        assert_eq!(r2, r);
        // only the alignment row turns to source when the caret is on it
        let r = render("| a | bb |\n|---|:-:|\n| 1 | 2 |", 40, Some((1, 0)));
        assert_eq!(r, ["┌───┬────┐", "│ a │ bb │", "|---|:-:|", "│ 1 │ 2  │", "└───┴────┘"]);
    }

    #[test]
    fn table_cell_being_edited_reveals_markup_and_widens() {
        let src = "| **b** | c |\n|---|---|\n| 1 | 2 |";
        assert_eq!(render(src, 40, None)[1], "│ b │ c │");
        let r = render(src, 40, Some((0, 4)));
        assert_eq!(r, ["┌───────┬───┐", "│ **b** │ c │", "├───────┼───┤", "│ 1     │ 2 │", "└───────┴───┘"]);
    }

    #[test]
    fn code_and_callout() {
        let r = render("```rust\nfn x() {}\n```", 30, None);
        assert_eq!(r[0].trim(), "");
        assert!(r[1].starts_with(" fn x() {}") && r[1].ends_with("rust"), "{:?}", r[1]);
        let r = render("> [!warning] Careful\n> body", 40, None);
        assert_eq!(r, ["▌ ⚠ Careful", "▌ body"]);
        let r = render("> [!note]-\n> hidden", 40, None);
        assert_eq!(r, ["▌ ✎ Note  ▸"]);
    }

    #[test]
    fn mapping_roundtrip() {
        let (lines, d) = setup("hello **wörld** 日本 end");
        let th = Theme::ansi();
        let le = |_: &str| true;
        let ctx = Ctx {
            theme: &th,
            width: 80,
            mode: Mode::Live,
            heading_mode: HeadingMode::Plain,
            images: None,
            link_exists: &le,
            embed: &|_: &str, _: &str| None,
            max_image_rows: 10,
            epoch: 0,
            title: None,
            typo: None,
            caret: None,
            sel: None,
            hits: &[],
            cur_hit: None,
        };
        let l = Layout::new();
        let rows = l.rows(&ctx, &lines, &d, 0, None, None);
        let row = &rows[0];
        let line = &lines[0];
        // "hello wörld 日本 end" — col of 'w' (byte 8) at x=6
        assert_eq!(x_of_col(row, line, 8), 6);
        assert_eq!(col_of_x(row, line, 6), 8);
        // 日 at byte 17, x = 12; 本 at 20, x = 14
        assert_eq!(x_of_col(row, line, 17), 12);
        assert_eq!(x_of_col(row, line, 20), 14);
        assert_eq!(col_of_x(row, line, 15), 23); // right half of a wide char
        assert_eq!(col_of_x(row, line, 14), 20);
        assert_eq!(x_of_col(row, line, line.len()), str_width("hello wörld 日本 end"));
    }

    #[test]
    fn heading_osc66_scale() {
        let (lines, d) = setup("# Big");
        let th = Theme::ansi();
        let le = |_: &str| true;
        let ctx = Ctx {
            theme: &th,
            width: 40,
            mode: Mode::Live,
            heading_mode: HeadingMode::Osc66,
            images: None,
            link_exists: &le,
            embed: &|_: &str, _: &str| None,
            max_image_rows: 10,
            epoch: 0,
            title: None,
            typo: None,
            caret: None,
            sel: None,
            hits: &[],
            cur_hit: None,
        };
        let l = Layout::new();
        let rows = l.rows(&ctx, &lines, &d, 0, Some(5), None);
        assert_eq!(rows[0].scale, 2);
        // "# " revealed at scale 2 → 'B' starts at x=4
        assert_eq!(x_of_col(&rows[0], &lines[0], 2), 4);
        assert_eq!(x_of_col(&rows[0], &lines[0], 5), 10);
    }
}
