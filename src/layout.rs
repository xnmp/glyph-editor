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
    pub scale: u8,
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
    pub fill_x1: Option<u16>,
    pub image: Option<(Arc<Img>, u32, u16)>,
    pub last: bool,
    pub end_x: u16,
}

impl VRow {
    fn new(a: usize, b: usize) -> VRow {
        VRow { a, b, segs: vec![], scale: 1, kind: RowKind::Text, cursorable: true, fill: None, fill_x1: None, image: None, last: false, end_x: 0 }
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
    pub max_image_rows: u32,
    pub epoch: u64,
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
}

impl Atom {
    fn src(a: usize, b: usize, w: u16, style: Style, space: bool) -> Atom {
        Atom { a, b, w, style, repl: None, hidden: false, space, tab: false }
    }
    fn repl(a: usize, b: usize, text: String, style: Style) -> Atom {
        let w = str_width(&text) as u16;
        Atom { a, b, w, style, repl: Some(text), hidden: false, space: false, tab: false }
    }
    fn hidden(a: usize, b: usize) -> Atom {
        Atom { a, b, w: 0, style: Style::new(), repl: None, hidden: true, space: false, tab: false }
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
                    segs.push(Seg { x: x as u16, w: 0, text: String::new(), style: at.style, a: at.a, b: at.b, src: true, run: false, scale });
                    continue;
                }
                if let Some(r) = at.repl {
                    segs.push(Seg { x: x as u16, w, text: r, style: at.style, a: at.a, b: at.b, src: true, run: false, scale });
                    continue;
                }
                if at.tab {
                    segs.push(Seg { x: x as u16, w, text: " ".repeat(at.w as usize), style: at.style, a: at.a, b: at.b, src: true, run: false, scale });
                    continue;
                }
                if let Some(last) = segs.last_mut() {
                    if last.run && last.style == at.style && last.b == at.a && (last.x + last.w) as i64 == x {
                        last.b = at.b;
                        last.w += w;
                        last.text.push_str(&line[at.a..at.b]);
                        continue;
                    }
                }
                segs.push(Seg { x: x as u16, w, text: line[at.a..at.b].to_string(), style: at.style, a: at.a, b: at.b, src: true, run: true, scale });
            }
            segs
        })
        .collect()
}

const TASK_GLYPHS: &[(char, &str)] = &[(' ', "☐"), ('x', "☑"), ('X', "☑"), ('/', "◐"), ('-', "⊘"), ('!', "!"), ('?', "?"), ('>', "➜"), ('<', "⇠"), ('*', "★")];
const BULLETS: &[&str] = &["•", "◦", "▪", "▫"];

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
    /// this line; `block_active` when the cursor is inside this line's block.
    pub fn rows(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, cursor: Option<usize>, block_active: bool) -> Rc<Vec<VRow>> {
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
        block_active.hash(&mut h);
        cursor.hash(&mut h);
        (ctx.width, ctx.mode, ctx.heading_mode, ctx.max_image_rows).hash(&mut h);
        if text.contains('!') || text.contains('$') || info.kind == LineKind::Heading || blk.is_some() {
            ctx.epoch.hash(&mut h);
        }
        let key = h.finish();
        if let Some(r) = self.cache.borrow().get(&key) {
            return r.clone();
        }
        let mut rows = self.layout_line(ctx, lines, doc, i, cursor, block_active);
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
                        hide(&mut hidden, sp.start, sp.open_end);
                        hide(&mut hidden, sp.close_start, sp.end);
                    }
                    false
                }
                SpanKind::Link => {
                    paint(&mut styles, sp.open_end, sp.close_start, th.link_style());
                    true
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
                        repl.push((sp.start, sp.end, format!("⧉ {}", if sp.anchor.is_empty() { sp.target.clone() } else { format!("{} › {}", sp.target, sp.anchor) }), st));
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
    fn rows_from(&self, ctx: &Ctx, text: &str, atoms: &[Atom], prefix: Vec<Seg>, cont_prefix: &[Seg], a: usize, b: usize, x0: Option<usize>, cont_x: Option<usize>, scale: u8, fill: Option<Style>, char_wrap: bool, width: usize) -> Vec<VRow> {
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
    fn layout_line(&self, ctx: &Ctx, lines: &[String], doc: &Document, i: usize, reveal: Option<usize>, block_active: bool) -> Vec<VRow> {
        let th = ctx.theme;
        let info = &doc.infos[i];
        let text = lines[i].as_str();
        let blk = info.block.map(|b| &doc.blocks[b]);
        let w = ctx.width;
        if ctx.mode == Mode::Source {
            return self.source_line(ctx, lines, doc, i, reveal);
        }
        let active_line = reveal.is_some();
        match info.kind {
            LineKind::Blank => return vec![VRow::new(0, text.len())],
            _ => {}
        }
        if let Some(b) = blk {
            match b.kind {
                BlockKind::Code => return self.code_line(ctx, lines, doc, i, b, block_active),
                BlockKind::Math => return self.math_line(ctx, lines, i, b, block_active),
                BlockKind::Front => return self.front_line(ctx, lines, i, b, block_active),
                BlockKind::Table if !block_active => return self.table_line(ctx, lines, i, b),
                BlockKind::Table => return self.table_raw_line(ctx, lines, doc, i, reveal),
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
                r.segs.push(Seg { x: 0, w: w as u16, text: "─".repeat(w), style: Style::fg(th.border), a: 0, b: text.len(), src: true, run: false, scale: 1 });
                r.end_x = 0;
                return vec![r];
            }
            _ => {}
        }
        self.flow_line(ctx, lines, doc, i, reveal, block_active)
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
        let src_seg = |x: usize, w: usize, t: &str, st: Style, a: usize, b: usize| Seg { x: x as u16, w: w as u16, text: t.to_string(), style: st, a, b, src: true, run: false, scale: 1 };
        let deco_seg = |x: usize, w: usize, t: &str, st: Style| Seg { x: x as u16, w: w as u16, text: t.to_string(), style: st, a: 0, b: 0, src: false, run: false, scale: 1 };

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
            let bar = Style::fg(callout_col.unwrap_or(th.faint));
            let raw = active_line && reveal.is_some_and(|r| r <= info.quote_end);
            for (k, ch) in text[..info.quote_end].char_indices() {
                let e = k + ch.len_utf8();
                let seg = if ch == '>' {
                    if raw { src_seg(x, 1, ">", th.marker(), k, e) } else { src_seg(x, 1, if callout_col.is_some() { "▌" } else { "▍" }, bar, k, e) }
                } else {
                    src_seg(x, 1, " ", Style::new(), k, e)
                };
                prefix.push(seg);
                x += 1;
            }
            cont_prefix = prefix
                .iter()
                .map(|s| deco_seg(s.x as usize, s.w as usize, if s.text == ">" || s.text == "▍" || s.text == "▌" { if callout_col.is_some() { "▌" } else { "▍" } } else { " " }, bar))
                .collect();
        }

        // callout header
        if info.kind == LineKind::CalloutHead {
            if let Some(co) = &info.callout {
                let col = callout_col.unwrap_or(th.accent);
                let title_st = Style::fg(col).bold();
                let revealed = active_line && reveal.is_some_and(|r| r <= co.marker_end);
                let mut atoms: Vec<Atom> = vec![];
                if revealed {
                    atoms.extend(self.raw_atoms(text, co.marker_start, co.marker_end, th.marker()));
                } else {
                    let mut label = format!("{} ", callout_icon(&co.kind));
                    if co.title.is_empty() {
                        let mut c = co.kind.chars();
                        label.push_str(&c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default());
                    }
                    atoms.push(Atom::repl(co.marker_start, co.marker_end, label, title_st));
                }
                let (t, _) = self.inline_atoms(ctx, text, co.marker_end, text.len(), title_st, reveal, false);
                atoms.extend(t);
                if co.fold != 0 && !revealed {
                    let open = block_active || co.fold == b'+';
                    atoms.push(Atom::repl(text.len(), text.len(), if open { "  ▾".into() } else { "  ▸".into() }, Style::fg(th.muted)));
                }
                return self.rows_from(ctx, text, &atoms, prefix, &cont_prefix, info.quote_end, text.len(), None, None, 1, fill, false, ctx.width);
            }
        }

        // list markers, indentation
        let mut lead_end = info.content;
        if let Some(lm) = &info.lst {
            let depth = lm.depth;
            let ind = crate::text::indent_width(&text[info.quote_end..lm.start]);
            let step = if depth > 0 { (ind / depth).max(1) } else { 0 };
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
                    prefix.push(src_seg(x, 2, &format!("{g} "), st, lm.task_start, lm.task_end));
                }
                x += if reveal_task { tw } else { 2 };
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
        let mut scale = 1u8;
        let is_heading = info.kind == LineKind::Heading;
        let big = is_heading && info.level <= 2 && info.quote_depth == 0;
        if is_heading {
            let hst = th.heading_style(info.level);
            base = base.over(hst);
            if big && ctx.heading_mode == HeadingMode::Image && !active_line {
                if let (Some(imgs), Some((r, g, b))) = (ctx.images, hst.fg.rgb().or(th.canvas.rgb().map(|_| (0, 0, 0))).or(Some((200, 200, 200)))) {
                    let plain = if info.setext { md::inline_text(text) } else { md::inline_text(text[info.content..].trim_end().trim_end_matches('#').trim_end()) };
                    let col = if hst.fg.rgb().is_some() { (r, g, b) } else if th.dark { (230, 230, 230) } else { (20, 20, 20) };
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
            }
            if big && ctx.heading_mode == HeadingMode::Osc66 {
                scale = 2;
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
        let mut rows = self.rows_from(ctx, text, &atoms, prefix, &cont_prefix, 0, text.len(), None, Some(if is_heading { 0 } else { cont_x }), scale, fill, false, ctx.width);
        if big && ctx.heading_mode == HeadingMode::Image {
            rows.push(VRow::deco());
        }
        if is_heading && info.level == 1 && ctx.heading_mode == HeadingMode::Plain && info.quote_depth == 0 {
            let mut r = VRow::deco();
            r.segs.push(Seg { x: 0, w: ctx.width as u16, text: "━".repeat(ctx.width), style: Style::fg(th.border), a: 0, b: 0, src: false, run: false, scale: 1 });
            rows.push(r);
        }
        let img_x = if info.lst.is_some() || info.quote_depth > 0 { cont_x } else { 0 };
        self.attach_images(ctx, text, &spans, rows, reveal, img_x)
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
        let mut out: Vec<VRow> = if image_only && !revealed { rows.iter().filter(|r| r.kind != RowKind::Text || r.segs.iter().any(|s| !s.src)).take(0).cloned().collect() } else { rows };
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
                    row.segs.push(Seg { x: px as u16, w: str_width(&label) as u16, text: label, style: Style::fg(th.faint).italic(), a: 0, b: 0, src: false, run: false, scale: 1 });
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
        let pad = Seg { x: 0, w: 1, text: " ".into(), style: Style::new(), a: 0, b: 0, src: false, run: false, scale: 1 };
        if is_open || is_close {
            if active || ctx.mode == Mode::Source {
                let atoms = self.raw_atoms(text, 0, text.len(), th.marker());
                return self.rows_from(ctx, text, &atoms, vec![pad], &[], 0, text.len(), Some(1), Some(1), 1, fill, true, w);
            }
            let mut r = VRow::new(0, text.len());
            r.segs.push(Seg { x: 1, w: 0, text: String::new(), style: Style::new(), a: 0, b: text.len(), src: true, run: false, scale: 1 });
            if is_open && !b.lang.is_empty() {
                let lw = str_width(&b.lang);
                r.segs.push(Seg { x: w.saturating_sub(lw + 1) as u16, w: lw as u16, text: b.lang.clone(), style: Style::fg(th.faint), a: 0, b: 0, src: false, run: false, scale: 1 });
            }
            r.fill = fill;
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
        self.rows_from(ctx, text, &atoms, vec![pad.clone()], &[pad], 0, text.len(), Some(1), Some(1), 1, fill, true, w.saturating_sub(1))
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
            r.segs.push(Seg { x: x as u16, w: uw as u16, text: u, style: Style::new().italic(), a: 0, b: 0, src: false, run: false, scale: 1 });
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
        let fill = Some(Style::bg(th.sidebar));
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
        if i == b.start {
            let lab = " ⚙ Properties";
            r.segs.push(Seg { x: 0, w: str_width(lab) as u16, text: lab.into(), style: Style::fg(th.muted).bold(), a: 0, b: text.len(), src: true, run: false, scale: 1 });
            return vec![r];
        }
        if i == b.end - 1 {
            r.segs.push(Seg { x: 0, w: 0, text: String::new(), style: Style::new(), a: 0, b: text.len(), src: true, run: false, scale: 1 });
            return vec![r];
        }
        let kw = lines[b.start + 1..b.end - 1].iter().filter(|l| is_key(l)).map(|l| str_width(l.split(':').next().unwrap().trim())).max().unwrap_or(4).min(18) + 3;
        if is_key(text) {
            let colon = text.find(':').unwrap();
            let key = text[..colon].trim();
            let mut va = colon + 1;
            while va < text.len() && text.as_bytes()[va] == b' ' {
                va += 1;
            }
            let lab = crate::text::pad_to(&format!(" {key}"), kw);
            let prefix = vec![Seg { x: 0, w: kw as u16, text: lab, style: Style::fg(th.muted), a: 0, b: va, src: true, run: false, scale: 1 }];
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

    fn table_model(&self, ctx: &Ctx, lines: &[String], b: &md::Block) -> Rc<TableModel> {
        let key = h64(&(&lines[b.start..b.end], ctx.width, ctx.mode, ctx.theme.name.as_str(), ctx.epoch));
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
            let mut row: Vec<Vec<Atom>> = md::split_table_row(text).into_iter().take(ncols).map(|(a, e)| self.inline_atoms(ctx, text, a, e, base, None, false).0).collect();
            row.resize(ncols, vec![]);
            cells.insert(ln, row);
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
        let m = Rc::new(TableModel { widths, aligns, cells });
        self.tables.borrow_mut().insert(key, m.clone());
        m
    }

    fn table_line(&self, ctx: &Ctx, lines: &[String], i: usize, b: &md::Block) -> Vec<VRow> {
        let th = ctx.theme;
        let m = self.table_model(ctx, lines, b);
        let text = lines[i].as_str();
        let bst = Style::fg(th.border);
        let border = |l: &str, mid: &str, r: &str| -> Seg {
            let s = format!("{l}{}{r}", m.widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>().join(mid));
            Seg { x: 0, w: str_width(&s) as u16, text: s, style: bst, a: 0, b: 0, src: false, run: false, scale: 1 }
        };
        if i == b.start + 1 {
            let mut r = VRow::new(0, text.len());
            r.segs.push(border("├", "┼", "┤"));
            r.segs.push(Seg { x: 0, w: 0, text: String::new(), style: Style::new(), a: 0, b: text.len(), src: true, run: false, scale: 1 });
            return vec![r];
        }
        let mut out = vec![];
        if i == b.start {
            let mut r = VRow::deco();
            r.segs.push(border("┌", "┬", "┐"));
            out.push(r);
        }
        let empty = vec![];
        let row_cells = m.cells.get(&i).unwrap_or(&empty);
        let wrapped: Vec<Vec<Vec<Seg>>> = row_cells.iter().enumerate().map(|(c, at)| wrap_atoms(text, at, m.widths[c], 0, 0, 1, false)).collect();
        let h = wrapped.iter().map(|w| w.len()).max().unwrap_or(1).max(1);
        for r in 0..h {
            let mut row = VRow::new(0, text.len());
            let mut x = 0usize;
            row.segs.push(Seg { x: 0, w: 1, text: "│".into(), style: bst, a: 0, b: 0, src: false, run: false, scale: 1 });
            x += 1;
            for c in 0..m.widths.len() {
                let parts: &[Seg] = wrapped.get(c).and_then(|w| w.get(r)).map(|v| v.as_slice()).unwrap_or(&[]);
                let used: usize = parts.iter().map(|s| s.w as usize).sum();
                let pad = match m.aligns[c] {
                    b'r' => m.widths[c].saturating_sub(used),
                    b'c' => m.widths[c].saturating_sub(used) / 2,
                    _ => 0,
                };
                for s in parts {
                    let mut s = s.clone();
                    s.x += (x + 1 + pad) as u16;
                    row.segs.push(s);
                }
                x += m.widths[c] + 2;
                row.segs.push(Seg { x: x as u16, w: 1, text: "│".into(), style: bst, a: 0, b: 0, src: false, run: false, scale: 1 });
                x += 1;
            }
            row.cursorable = r == 0;
            if !row.cursorable {
                row.kind = RowKind::Deco;
            }
            if i == b.start {
                row.fill = Some(Style::bg(th.sidebar));
                row.fill_x1 = Some(x as u16);
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
        if let Some(b) = info.block.map(|b| &doc.blocks[b]) {
            if b.kind == BlockKind::Code {
                return self.code_line(ctx, lines, doc, i, b, true);
            }
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
        let pre_st = if info.lst.is_some() { Style::fg(th.accent) } else { th.marker().over(if info.kind == LineKind::Heading { Style::new().bold() } else { Style::new() }) };
        let mut atoms = self.raw_atoms(text, 0, info.content, pre_st);
        let cont: usize = if info.lst.is_some() { atoms.iter().map(|a| a.w as usize).sum() } else { 0 };
        atoms.extend(self.inline_atoms(ctx, text, info.content, text.len(), base, reveal, true).0);
        self.rows_from(ctx, text, &atoms, vec![], &[], 0, text.len(), None, Some(cont), 1, None, false, ctx.width)
    }
}

fn return_false() {}

// ---------------------------------------------------------------------- mapping

/// Relative x of a cursor before source byte `col` in this row.
pub fn x_of_col(row: &VRow, line: &str, col: usize) -> usize {
    for s in &row.segs {
        if !s.src {
            continue;
        }
        if s.a <= col && col < s.b {
            if s.w == 0 {
                continue;
            }
            if s.run {
                let mut x = s.x as usize;
                let mut k = s.a;
                for g in line[s.a..s.b].graphemes(true) {
                    if k >= col {
                        break;
                    }
                    x += if g == "\t" { 1 } else { grapheme_width(g) } * s.scale as usize;
                    k += g.len();
                }
                return x;
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
    let mut best = row.a;
    for s in &row.segs {
        if !s.src || s.w == 0 {
            continue;
        }
        let (sx, sw) = (s.x as usize, s.w as usize);
        if sx <= x && x < sx + sw {
            if s.run {
                let mut gx = sx;
                let mut k = s.a;
                for g in line[s.a..s.b].graphemes(true) {
                    let gw = grapheme_width(g).max(1) * s.scale as usize;
                    if x < gx + gw {
                        return if x - gx >= gw.div_ceil(2) && gw > 1 { k + g.len() } else { k };
                    }
                    gx += gw;
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
        let ctx = Ctx { theme: &th, width, mode: Mode::Live, heading_mode: HeadingMode::Plain, images: None, link_exists: &le, max_image_rows: 10, epoch: 0 };
        let l = Layout::new();
        let active_block = cursor.and_then(|(ln, _)| d.infos[ln].block);
        let mut out = vec![];
        for i in 0..lines.len() {
            let c = cursor.filter(|c| c.0 == i).map(|c| c.1);
            let ba = d.infos[i].block.is_some() && d.infos[i].block == active_block;
            for r in l.rows(&ctx, &lines, &d, i, c, ba).iter() {
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
        assert_eq!(render("some **bold** and [link](http://x)", 80, None), ["some bold and link"]);
        assert_eq!(render("some **bold** and [link](http://x)", 80, Some((0, 7))), ["some **bold** and link"]);
        assert_eq!(render("# Title", 80, None)[0], "Title");
        assert_eq!(render("# Title", 80, Some((0, 3)))[0], "# Title");
        assert_eq!(render("- item\n  - sub\n- [ ] todo\n- [x] done", 80, None), ["• item", "│ ◦ sub", "☐ todo", "☑ done"]);
    }

    #[test]
    fn wrapping_hanging_indent() {
        let r = render("- alpha beta gamma delta", 14, None);
        assert_eq!(r, ["• alpha beta", "  gamma delta"]);
        let r = render("> quoted text that wraps", 12, None);
        assert_eq!(r, ["▍ quoted", "▍ text that", "▍ wraps"]);
    }

    #[test]
    fn table_render() {
        let r = render("| a | bb |\n|---|:-:|\n| 1 | 2 |", 40, None);
        assert_eq!(r, ["┌───┬────┐", "│ a │ bb │", "├───┼────┤", "│ 1 │ 2  │", "└───┴────┘"]);
        // active table shows source
        let r = render("| a | bb |\n|---|:-:|\n| 1 | 2 |", 40, Some((2, 0)));
        assert_eq!(r[0], "| a | bb |");
    }

    #[test]
    fn code_and_callout() {
        let r = render("```rust\nfn x() {}\n```", 30, None);
        assert_eq!(r[0].trim(), "rust");
        assert_eq!(r[1], " fn x() {}");
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
        let ctx = Ctx { theme: &th, width: 80, mode: Mode::Live, heading_mode: HeadingMode::Plain, images: None, link_exists: &le, max_image_rows: 10, epoch: 0 };
        let l = Layout::new();
        let rows = l.rows(&ctx, &lines, &d, 0, None, false);
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
        let ctx = Ctx { theme: &th, width: 40, mode: Mode::Live, heading_mode: HeadingMode::Osc66, images: None, link_exists: &le, max_image_rows: 10, epoch: 0 };
        let l = Layout::new();
        let rows = l.rows(&ctx, &lines, &d, 0, Some(5), false);
        assert_eq!(rows[0].scale, 2);
        // "# " revealed at scale 2 → 'B' starts at x=4
        assert_eq!(x_of_col(&rows[0], &lines[0], 2), 4);
        assert_eq!(x_of_col(&rows[0], &lines[0], 5), 10);
    }
}
