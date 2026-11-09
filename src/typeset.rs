//! Proportional typesetting: render prose the way Obsidian does (Inter,
//! 1.5 line height, real heading sizes) into images shown with the kitty
//! graphics protocol. Everything that is drawn — text, inline-code pills,
//! highlights, links, selection, search hits and the caret — goes into the
//! image, and a [`TypeMap`] maps source columns ↔ pixels so the cursor,
//! mouse and vertical motion keep working exactly as with cell text.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Regular,
    SemiBold,
    Bold,
    Italic,
    BoldItalic,
    Mono,
}

pub struct Fonts {
    regular: FontRef<'static>,
    semibold: FontRef<'static>,
    bold: FontRef<'static>,
    italic: FontRef<'static>,
    bold_italic: FontRef<'static>,
    mono: FontRef<'static>,
    fallback: FontRef<'static>,
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

impl Fonts {
    pub fn get() -> &'static Fonts {
        FONTS.get_or_init(|| {
            let f = |b: &'static [u8]| FontRef::try_from_slice(b).expect("embedded font");
            Fonts {
                regular: f(include_bytes!("../assets/fonts/Inter-Regular.otf")),
                semibold: f(include_bytes!("../assets/fonts/Inter-SemiBold.otf")),
                bold: f(include_bytes!("../assets/fonts/Inter-Bold.otf")),
                italic: f(include_bytes!("../assets/fonts/Inter-Italic.otf")),
                bold_italic: f(include_bytes!("../assets/fonts/Inter-BoldItalic.otf")),
                mono: f(include_bytes!("../assets/fonts/DejaVuSansMono.ttf")),
                fallback: f(include_bytes!("../assets/fonts/DejaVuSans.ttf")),
            }
        })
    }

    fn face(&self, f: Face) -> &FontRef<'static> {
        match f {
            Face::Regular => &self.regular,
            Face::SemiBold => &self.semibold,
            Face::Bold => &self.bold,
            Face::Italic => &self.italic,
            Face::BoldItalic => &self.bold_italic,
            Face::Mono => &self.mono,
        }
    }

    /// The font that has `c` (the face, else the fallback), with its index.
    fn pick(&self, f: Face, c: char) -> Option<(u8, &FontRef<'static>)> {
        let fc = self.face(f);
        if fc.glyph_id(c).0 != 0 {
            return Some((0, fc));
        }
        if self.fallback.glyph_id(c).0 != 0 {
            return Some((1, &self.fallback));
        }
        None
    }

    /// Can every character of `s` be typeset in face `f`?
    pub fn covers(&self, f: Face, s: &str) -> bool {
        s.chars().all(|c| c.is_whitespace() || c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{fe0f}') || self.pick(f, c).is_some())
    }
}

/// A styled stretch of text with its source columns.
#[derive(Clone, Debug)]
pub struct Piece {
    pub text: String,
    pub a: usize,
    pub b: usize,
    /// text is exactly line[a..b] (one source grapheme per glyph)
    pub src: bool,
    pub face: Face,
    /// relative to the base font size
    pub scale: f32,
    pub fg: (u8, u8, u8),
    pub bg: Option<(u8, u8, u8)>,
    pub underline: Option<(u8, u8, u8)>,
    pub strike: bool,
    /// advance to this absolute x (px) before the text: a tab stop
    pub tab: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub width_px: f32,
    pub font_px: f32,
    /// line height as a multiple of the (scaled) font size
    pub line_height: f32,
    /// extra space above the first line, px (heading margins)
    pub pad_top: f32,
    pub cell_w: u32,
    pub cell_h: u32,
}

#[derive(Clone, Debug)]
struct G {
    x: f32,
    w: f32,
    tab: Option<f32>,
    text: String,
    a: usize,
    b: usize,
    piece: usize,
    space: bool,
}

#[derive(Clone, Debug)]
struct Line {
    top: f32,
    height: f32,
    baseline: f32,
    glyphs: Vec<G>,
}

/// Column ↔ pixel map of a typeset source line.
#[derive(Clone, Debug, Default)]
pub struct TypeMap {
    pub lines: Vec<MapLine>,
    pub cell_w: u32,
    pub cell_h: u32,
    pub rows: usize,
}

#[derive(Clone, Debug, Default)]
pub struct MapLine {
    pub top: f32,
    pub height: f32,
    pub a: usize,
    pub b: usize,
    /// (source column, x px) at every caret stop, in order
    pub stops: Vec<(usize, f32)>,
}

impl TypeMap {
    /// Index of the visual line holding a caret at `col`.
    pub fn line_of(&self, col: usize, at_end: usize) -> usize {
        for (k, l) in self.lines.iter().enumerate() {
            if col < l.b || (col == l.b && (k + 1 == self.lines.len() || col == at_end)) {
                return k;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    pub fn x_of(&self, li: usize, col: usize) -> f32 {
        let l = &self.lines[li];
        let mut x = l.stops.first().map_or(0.0, |s| s.1);
        for &(c, sx) in &l.stops {
            if c > col {
                break;
            }
            x = sx;
        }
        x
    }

    /// Nearest caret stop to x on visual line `li`.
    pub fn col_at(&self, li: usize, x: f32) -> usize {
        let l = &self.lines[li];
        let mut best = (f32::MAX, l.a);
        for &(c, sx) in &l.stops {
            let d = (sx - x).abs();
            if d < best.0 {
                best = (d, c);
            }
        }
        best.1
    }

    /// Visual line at pixel y (nearest).
    pub fn line_at_y(&self, y: f32) -> usize {
        for (k, l) in self.lines.iter().enumerate() {
            if y < l.top + l.height {
                return k;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    /// The cell row a visual line "lives on" (where up/down lands).
    pub fn anchor_row(&self, li: usize) -> usize {
        let l = &self.lines[li];
        (((l.top + l.height * 0.55) / self.cell_h as f32) as usize).min(self.rows.saturating_sub(1))
    }
}

pub struct Typeset {
    lines: Vec<Line>,
    pieces: Vec<Piece>,
    pub map: TypeMap,
    pub height: f32,
    width: f32,
    font_px: f32,
}

fn advance(fonts: &Fonts, face: Face, px: f32, g: &str) -> f32 {
    let mut w = 0.0;
    for c in g.chars() {
        if let Some((_, f)) = fonts.pick(face, c) {
            w += f.as_scaled(PxScale::from(px)).h_advance(f.glyph_id(c));
        }
    }
    w
}

/// Break pieces into lines of at most `p.width_px`.
pub fn layout(pieces: Vec<Piece>, p: &Params) -> Typeset {
    let fonts = Fonts::get();
    // flatten into glyph clusters
    let mut gs: Vec<G> = vec![];
    for (pi, pc) in pieces.iter().enumerate() {
        let px = p.font_px * pc.scale;
        if pc.tab.is_some() {
            gs.push(G { x: 0.0, w: 0.0, tab: pc.tab, text: String::new(), a: pc.a, b: pc.a, piece: pi, space: false });
        }
        if pc.text.is_empty() {
            gs.push(G { x: 0.0, w: 0.0, tab: pc.tab, text: String::new(), a: pc.a, b: pc.b, piece: pi, space: false });
            continue;
        }
        if pc.src {
            let mut k = pc.a;
            for g in pc.text.graphemes(true) {
                let t = if g == "\t" { "    " } else { g };
                gs.push(G {
                    x: 0.0,
                    w: advance(fonts, pc.face, px, t),
                    tab: None,
                    text: t.to_string(),
                    a: k,
                    b: k + g.len(),
                    piece: pi,
                    space: g == " " || g == "\t",
                });
                k += g.len();
            }
        } else {
            // a replacement (bullet, math, arrow…) is one unit for the caret
            gs.push(G { x: 0.0, w: advance(fonts, pc.face, px, &pc.text), tab: None, text: pc.text.clone(), a: pc.a, b: pc.b, piece: pi, space: false });
        }
    }
    // greedy line breaking at spaces
    let mut lines_g: Vec<Vec<G>> = vec![vec![]];
    let mut x = 0.0f32;
    let mut last_space: Option<usize> = None;
    for mut g in gs {
        if let Some(t) = g.tab {
            g.w = (t - x).max(0.0);
        }
        let cur = lines_g.last_mut().unwrap();
        if x + g.w > p.width_px && !cur.is_empty() && g.w > 0.0 {
            if g.space {
                // trailing space hangs at the end of the line
                cur.push(G { w: 0.0, ..g });
                lines_g.push(vec![]);
                x = 0.0;
                last_space = None;
                continue;
            }
            match last_space {
                Some(ls) if ls < cur.len() && ls > 0 => {
                    let carry = cur.split_off(ls);
                    x = carry.iter().map(|g| g.w).sum();
                    lines_g.push(carry);
                }
                _ => {
                    lines_g.push(vec![]);
                    x = 0.0;
                }
            }
            last_space = None;
        }
        let cur = lines_g.last_mut().unwrap();
        x += g.w;
        let sp = g.space;
        cur.push(g);
        if sp {
            last_space = Some(cur.len());
        }
    }
    // position lines
    let mut lines = vec![];
    let mut y = p.pad_top;
    for lg in lines_g {
        let scale = lg.iter().map(|g| pieces[g.piece].scale).fold(0.0f32, f32::max);
        let scale = if scale == 0.0 { pieces.iter().map(|p| p.scale).fold(1.0f32, f32::max) } else { scale };
        let size = p.font_px * scale;
        let height = (size * p.line_height).round();
        let f = &fonts.regular.as_scaled(PxScale::from(size));
        let (asc, desc) = (f.ascent(), f.descent());
        let baseline = (y + (height - (asc - desc)) / 2.0 + asc).round();
        let mut gx = 0.0;
        let mut glyphs = lg;
        for g in glyphs.iter_mut() {
            g.x = gx;
            gx += g.w;
        }
        lines.push(Line { top: y, height, baseline, glyphs });
        y += height;
    }
    // The terminal grid quantizes height to whole rows. Rounding every block
    // up would leave a one-line paragraph with half a row of dead space, so
    // round to the nearest row count and spread the line pitch to fit
    // (≈1.5 inside paragraphs, tighter for single lines), never cramping
    // the text below 1.15× its size.
    let natural = y;
    let ch = p.cell_h as f32;
    let mut rows = ((natural / ch).round() as usize).max(1);
    let min_h: f32 = p.pad_top * 0.5 + lines.iter().map(|l: &Line| l.height / p.line_height * 1.15).sum::<f32>();
    if (rows as f32) * ch < min_h {
        rows = (min_h / ch).ceil() as usize;
    }
    let k = rows as f32 * ch / natural.max(1.0);
    if (k - 1.0).abs() > 0.001 {
        let mut ty = p.pad_top * k;
        for l in lines.iter_mut() {
            let h = l.height * k;
            let shift = ty - l.top + (h - l.height) / 2.0;
            l.baseline = (l.baseline + shift).round();
            l.top = ty;
            l.height = h;
            ty += h;
        }
    }
    let height = rows as f32 * ch;
    // caret map
    let mut map = TypeMap { lines: vec![], cell_w: p.cell_w, cell_h: p.cell_h, rows };
    for (k, l) in lines.iter().enumerate() {
        let mut stops = vec![];
        for g in &l.glyphs {
            stops.push((g.a, g.x));
        }
        let end_x = l.glyphs.last().map_or(0.0, |g| g.x + g.w);
        let a = l.glyphs.first().map_or(0, |g| g.a);
        let mut b = l.glyphs.last().map_or(a, |g| g.b);
        if k + 1 < lines.len() {
            // the next line starts where this one ends
            b = lines[k + 1].glyphs.first().map_or(b, |g| g.a);
        }
        stops.push((l.glyphs.last().map_or(a, |g| g.b), end_x));
        map.lines.push(MapLine { top: l.top, height: l.height, a, b, stops });
    }
    Typeset { lines, pieces, map, height, width: p.width_px, font_px: p.font_px }
}

/// What to paint into the image besides the text.
#[derive(Default, Clone)]
pub struct Marks {
    pub caret: Option<usize>,
    pub caret_color: (u8, u8, u8),
    /// source column ranges
    pub selection: Vec<(usize, usize)>,
    pub selection_color: (u8, u8, u8),
    pub hits: Vec<(usize, usize, (u8, u8, u8))>,
}

type GlyphKey = (u8, u8, u16, u32);

struct RasterGlyph {
    left: i32,
    top: i32,
    w: u32,
    h: u32,
    cov: Vec<u8>,
}

static GLYPHS: OnceLock<Mutex<HashMap<GlyphKey, std::sync::Arc<RasterGlyph>>>> = OnceLock::new();

fn raster(font_idx: u8, face: Face, font: &FontRef<'static>, id: GlyphId, px: f32) -> std::sync::Arc<RasterGlyph> {
    let key = (font_idx, face as u8, id.0, (px * 4.0) as u32);
    let cache = GLYPHS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(g) = cache.lock().unwrap().get(&key) {
        return g.clone();
    }
    let g = id.with_scale_and_position(PxScale::from(px), ab_glyph::point(0.0, 0.0));
    let rg = match font.outline_glyph(g) {
        Some(og) => {
            let bb = og.px_bounds();
            let (w, h) = (bb.width().ceil() as u32 + 1, bb.height().ceil() as u32 + 1);
            let mut cov = vec![0u8; (w * h) as usize];
            og.draw(|x, y, c| {
                if x < w && y < h {
                    cov[(y * w + x) as usize] = (c.clamp(0.0, 1.0) * 255.0) as u8;
                }
            });
            RasterGlyph { left: bb.min.x.floor() as i32, top: bb.min.y.floor() as i32, w, h, cov }
        }
        None => RasterGlyph { left: 0, top: 0, w: 0, h: 0, cov: vec![] },
    };
    let rg = std::sync::Arc::new(rg);
    let mut c = cache.lock().unwrap();
    if c.len() > 20_000 {
        c.clear();
    }
    c.insert(key, rg.clone());
    rg
}

#[inline]
fn blend(img: &mut RgbaImage, x: i32, y: i32, col: (u8, u8, u8), a: u8) {
    if x < 0 || y < 0 || x as u32 >= img.width() || y as u32 >= img.height() || a == 0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let sa = a as f32 / 255.0;
    let da = p[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return;
    }
    let mix = |s: u8, d: u8| ((s as f32 * sa + d as f32 * da * (1.0 - sa)) / oa).round() as u8;
    *p = Rgba([mix(col.0, p[0]), mix(col.1, p[1]), mix(col.2, p[2]), (oa * 255.0).round() as u8]);
}

fn rect(img: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, col: (u8, u8, u8), radius: f32) {
    let (xa, ya, xb, yb) = (x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32);
    for y in ya..yb {
        for x in xa..xb {
            // rounded corners: skip pixels outside the corner circles
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = if fx < x0 + radius {
                x0 + radius
            } else if fx > x1 - radius {
                x1 - radius
            } else {
                fx
            };
            let cy = if fy < y0 + radius {
                y0 + radius
            } else if fy > y1 - radius {
                y1 - radius
            } else {
                fy
            };
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = (radius + 0.5 - d).clamp(0.0, 1.0);
            blend(img, x, y, col, (a * 255.0) as u8);
        }
    }
}

impl Typeset {
    /// x ranges on each visual line covered by source columns [a, b).
    fn spans(&self, a: usize, b: usize) -> Vec<(usize, f32, f32)> {
        let mut out = vec![];
        for (k, l) in self.lines.iter().enumerate() {
            let mut x0 = None;
            let mut x1 = 0.0;
            for g in &l.glyphs {
                if g.a >= a && g.b <= b && g.b > g.a {
                    if x0.is_none() {
                        x0 = Some(g.x);
                    }
                    x1 = g.x + g.w.max(1.0);
                }
            }
            if let Some(x0) = x0 {
                out.push((k, x0, x1));
            }
        }
        out
    }

    pub fn render(&self, marks: &Marks) -> RgbaImage {
        let fonts = Fonts::get();
        let rows = self.map.rows as u32;
        let (cw, ch) = (self.map.cell_w, self.map.cell_h);
        // only as wide as the text (plus room for a caret at the end)
        let used = self.lines.iter().map(|l| l.glyphs.last().map_or(0.0, |g| g.x + g.w)).fold(0.0f32, f32::max) + 4.0;
        let w = ((used.min(self.width.max(1.0)) / cw as f32).ceil() as u32).max(1) * cw;
        let mut img = RgbaImage::from_pixel(w, rows * ch, Rgba([0, 0, 0, 0]));
        // selection and search hits behind the text
        for &(a, b) in &marks.selection {
            for (k, x0, x1) in self.spans(a, b) {
                let l = &self.lines[k];
                rect(&mut img, x0, l.top + 1.0, x1, l.top + l.height - 1.0, marks.selection_color, 0.0);
            }
        }
        for &(a, b, col) in &marks.hits {
            for (k, x0, x1) in self.spans(a, b) {
                let l = &self.lines[k];
                rect(&mut img, x0 - 1.0, l.top + 2.0, x1 + 1.0, l.top + l.height - 2.0, col, 3.0);
            }
        }
        for l in &self.lines {
            // backgrounds: pills for code / tags / highlights (contiguous per piece)
            let mut k = 0;
            while k < l.glyphs.len() {
                let pi = l.glyphs[k].piece;
                let mut e = k;
                while e + 1 < l.glyphs.len() && l.glyphs[e + 1].piece == pi {
                    e += 1;
                }
                if let Some(bg) = self.pieces[pi].bg {
                    let size = self.font_px * self.pieces[pi].scale;
                    let x0 = l.glyphs[k].x;
                    let x1 = l.glyphs[e].x + l.glyphs[e].w;
                    let mid = l.baseline - size * 0.32;
                    rect(&mut img, x0, mid - size * 0.68, x1, mid + size * 0.62, bg, 4.0);
                }
                k = e + 1;
            }
            // glyphs
            for g in &l.glyphs {
                let pc = &self.pieces[g.piece];
                let px = self.font_px * pc.scale;
                let mut x = g.x;
                for c in g.text.chars() {
                    let Some((fi, font)) = fonts.pick(pc.face, c) else { continue };
                    let id = font.glyph_id(c);
                    let rg = raster(fi, pc.face, font, id, px);
                    let (ox, oy) = (x.round() as i32 + rg.left, l.baseline as i32 + rg.top);
                    for yy in 0..rg.h {
                        for xx in 0..rg.w {
                            let a = rg.cov[(yy * rg.w + xx) as usize];
                            if a > 0 {
                                blend(&mut img, ox + xx as i32, oy + yy as i32, pc.fg, a);
                            }
                        }
                    }
                    x += font.as_scaled(PxScale::from(px)).h_advance(id);
                }
                if let Some(u) = pc.underline {
                    let y = l.baseline + (px * 0.12).max(2.0);
                    rect(&mut img, g.x, y, g.x + g.w, y + 1.0, u, 0.0);
                }
                if pc.strike {
                    let y = l.baseline - px * 0.3;
                    rect(&mut img, g.x, y, g.x + g.w, y + 1.0, pc.fg, 0.0);
                }
            }
        }
        // caret: a 2px bar spanning the text height of its line
        if let Some(col) = marks.caret {
            let end = self.map.lines.last().map_or(0, |l| l.b);
            let li = self.map.line_of(col, end);
            if let Some(l) = self.lines.get(li) {
                let x = self.map.x_of(li, col).round().max(0.0);
                let size = self.font_px * l.glyphs.iter().map(|g| self.pieces[g.piece].scale).fold(1.0f32, f32::max);
                let top = (l.baseline - size * 0.95).max(l.top);
                rect(&mut img, x, top, x + 2.0, (l.baseline + size * 0.28).min(l.top + l.height), marks.caret_color, 0.0);
            }
        }
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(text: &str, a: usize) -> Piece {
        Piece {
            text: text.into(),
            a,
            b: a + text.len(),
            src: true,
            face: Face::Regular,
            scale: 1.0,
            fg: (220, 220, 220),
            bg: None,
            underline: None,
            strike: false,
            tab: None,
        }
    }

    #[test]
    fn tab_stops_align_columns() {
        let p = Params { width_px: 400.0, font_px: 16.0, line_height: 1.5, pad_top: 0.0, cell_w: 10, cell_h: 20 };
        for key in ["a", "longer key"] {
            let mut v = piece("value", key.len());
            v.tab = Some(120.0);
            let t = layout(vec![piece(key, 0), v], &p);
            let x = t.map.x_of(0, key.len());
            assert!((x - 120.0).abs() < 0.5, "value starts at the tab stop for key {key:?}: {x}");
        }
    }

    #[test]
    fn wraps_and_maps() {
        let p = Params { width_px: 200.0, font_px: 16.0, line_height: 1.5, pad_top: 0.0, cell_w: 10, cell_h: 20 };
        let t = layout(vec![piece("The quick brown fox jumps over the lazy dog again and again", 0)], &p);
        assert!(t.map.lines.len() >= 2, "wrapped");
        // every line fits
        for l in &t.lines {
            let w: f32 = l.glyphs.iter().map(|g| g.w).sum();
            assert!(w <= 200.0 + 0.01);
        }
        // proportional: "i" is narrower than "m"
        let ti = layout(vec![piece("iiii", 0)], &p);
        let tm = layout(vec![piece("mmmm", 0)], &p);
        assert!(ti.map.lines[0].stops.last().unwrap().1 < tm.map.lines[0].stops.last().unwrap().1);
        // columns round-trip through pixels
        let l0 = &t.map.lines[0];
        for &(c, x) in &l0.stops {
            assert_eq!(t.map.col_at(0, x), c);
        }
        // 24px lines in 20px cells → rows = round(n*24/20), lines spread to fill
        assert_eq!(t.map.rows, ((t.map.lines.len() as f32 * 24.0) / 20.0).round() as usize);
        let last = t.map.lines.last().unwrap();
        assert!((last.top + last.height - t.map.rows as f32 * 20.0).abs() < 0.5);
        // a single line fits one row
        let one = layout(vec![piece("short", 0)], &p);
        assert_eq!(one.map.rows, 1);
        let img = t.render(&Marks { caret: Some(4), caret_color: (255, 255, 255), ..Default::default() });
        assert_eq!(img.height(), t.map.rows as u32 * 20);
        assert!(img.pixels().any(|p| p[3] > 200), "text drawn");
    }

    #[test]
    fn coverage() {
        let f = Fonts::get();
        assert!(f.covers(Face::Regular, "Hello, wörld — “quotes” α→β ∑"));
        assert!(!f.covers(Face::Regular, "日本語"));
    }
}
