//! A small virtual terminal used by the test-suite and the snapshot tool. It
//! interprets the subset of escape sequences glyph emits — CSI/SGR, OSC 8/66,
//! kitty graphics APC (incl. chunking, Unicode placeholders and placements) —
//! and can render the screen to a PNG for visual review.

use std::collections::HashMap;

use base64::Engine;
use image::{Rgba, RgbaImage};
use unicode_segmentation::UnicodeSegmentation;

use crate::graphics::{PLACEHOLDER, diacritic_index};
use crate::style::{BOLD, Color, DIM, ITALIC, REVERSE, STRIKE, Style};
use crate::text::grapheme_width;

#[derive(Clone, Debug, PartialEq)]
pub struct VCell {
    pub text: String,
    pub style: Style,
    /// 0 = normal, 1 = wide tail, 2 = covered by multicell
    pub flag: u8,
    pub scale: u8,
    pub link: Option<String>,
}

impl VCell {
    fn blank() -> VCell {
        VCell { text: " ".into(), style: Style::new(), flag: 0, scale: 1, link: None }
    }
}

#[derive(Clone, Debug)]
pub struct VImage {
    pub png: Vec<u8>,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Clone, Debug)]
pub struct VPlacement {
    pub id: u32,
    pub pid: u32,
    pub x: usize,
    pub y: usize,
    pub src: (u32, u32, u32, u32),
}

pub struct Vt {
    pub w: usize,
    pub h: usize,
    pub cells: Vec<VCell>,
    pub cx: usize,
    pub cy: usize,
    pub cursor_visible: bool,
    pub style: Style,
    pub link: Option<String>,
    pub images: HashMap<u32, VImage>,
    pub placements: Vec<VPlacement>,
    pub title: String,
    pub clipboard: Option<String>,
    pending: Vec<u8>,
    chunk: Option<(String, Vec<u8>)>,
}

impl Vt {
    pub fn new(w: usize, h: usize) -> Vt {
        Vt {
            w,
            h,
            cells: vec![VCell::blank(); w * h],
            cx: 0,
            cy: 0,
            cursor_visible: true,
            style: Style::new(),
            link: None,
            images: HashMap::new(),
            placements: vec![],
            title: String::new(),
            clipboard: None,
            pending: vec![],
            chunk: None,
        }
    }

    pub fn cell(&self, x: usize, y: usize) -> &VCell {
        &self.cells[y * self.w + x]
    }

    /// Visible text of row y (covered and wide-tail cells skipped).
    pub fn row(&self, y: usize) -> String {
        let mut s = String::new();
        for x in 0..self.w {
            let c = self.cell(x, y);
            if c.flag == 0 {
                if c.text.starts_with(PLACEHOLDER) {
                    s.push('▒');
                } else {
                    s.push_str(&c.text);
                }
            }
        }
        s.trim_end().to_string()
    }

    pub fn text(&self) -> String {
        (0..self.h).map(|y| self.row(y)).collect::<Vec<_>>().join("\n")
    }

    /// Find the first (x, y) where `needle` appears in a row's text.
    pub fn find(&self, needle: &str) -> Option<(usize, usize)> {
        for y in 0..self.h {
            // build the row text with a byte → column map (covered cells skipped)
            let mut r = String::new();
            let mut cols = vec![];
            for x in 0..self.w {
                let c = self.cell(x, y);
                if c.flag == 0 {
                    let t = if c.text.starts_with(PLACEHOLDER) { "▒" } else { c.text.as_str() };
                    for _ in 0..t.len() {
                        cols.push(x);
                    }
                    r.push_str(t);
                }
            }
            if let Some(i) = r.find(needle) {
                return Some((cols[i], y));
            }
        }
        None
    }

    fn erase_multicell_at(&mut self, x: usize, y: usize) {
        // writing into a multicell character erases the whole character
        let c = &self.cells[y * self.w + x];
        if c.flag == 2 || c.scale > 1 {
            // find owner: scan up/left within 2 cells (scale ≤ 2 in practice)
            for oy in (y.saturating_sub(3)..=y).rev() {
                for ox in (x.saturating_sub(7)..=x).rev() {
                    let o = &self.cells[oy * self.w + ox];
                    if o.scale > 1 && o.flag == 0 {
                        let s = o.scale as usize;
                        let cw = grapheme_width(&o.text).max(1) * s;
                        if ox + cw > x && oy + s > y {
                            for dy in 0..s {
                                for dx in 0..cw {
                                    if oy + dy < self.h && ox + dx < self.w {
                                        self.cells[(oy + dy) * self.w + ox + dx] = VCell::blank();
                                    }
                                }
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    fn put_grapheme(&mut self, g: &str) {
        let w = if g.starts_with(PLACEHOLDER) { 1 } else { grapheme_width(g) };
        if w == 0 {
            if self.cx > 0 {
                let i = self.cy * self.w + self.cx - 1;
                self.cells[i].text.push_str(g);
            }
            return;
        }
        if self.cx + w > self.w || self.cy >= self.h {
            return; // autowrap disabled
        }
        self.erase_multicell_at(self.cx, self.cy);
        let i = self.cy * self.w + self.cx;
        self.cells[i] = VCell { text: g.to_string(), style: self.style, flag: 0, scale: 1, link: self.link.clone() };
        if w == 2 {
            self.cells[i + 1] = VCell { text: String::new(), style: self.style, flag: 1, scale: 1, link: None };
        }
        self.cx += w;
    }

    fn put_scaled(&mut self, text: &str, s: usize) {
        for g in text.graphemes(true) {
            let cw = grapheme_width(g).max(1) * s;
            if self.cx + cw > self.w || self.cy + s > self.h {
                return;
            }
            for dy in 0..s {
                for dx in 0..cw {
                    self.erase_multicell_at(self.cx + dx, self.cy + dy);
                }
            }
            for dy in 0..s {
                for dx in 0..cw {
                    self.cells[(self.cy + dy) * self.w + self.cx + dx] = VCell { text: String::new(), style: self.style, flag: 2, scale: 1, link: None };
                }
            }
            self.cells[self.cy * self.w + self.cx] = VCell { text: g.to_string(), style: self.style, flag: 0, scale: s as u8, link: self.link.clone() };
            self.cx += cw;
        }
    }

    pub fn feed(&mut self, data: &str) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data.as_bytes());
        let s = String::from_utf8_lossy(&buf).to_string();
        let b = s.as_bytes();
        let n = b.len();
        let mut i = 0;
        let mut text_start = None::<usize>;
        let flush_text = |vt: &mut Vt, a: usize, e: usize| {
            for g in s[a..e].graphemes(true) {
                vt.put_grapheme(g);
            }
        };
        while i < n {
            if b[i] == 0x1b {
                if let Some(a) = text_start.take() {
                    flush_text(self, a, i);
                }
                if i + 1 >= n {
                    break;
                }
                match b[i + 1] {
                    b'[' => {
                        let mut k = i + 2;
                        while k < n && (0x30..=0x3f).contains(&b[k]) {
                            k += 1;
                        }
                        while k < n && (0x20..=0x2f).contains(&b[k]) {
                            k += 1;
                        }
                        if k >= n {
                            break;
                        }
                        self.csi(&s[i + 2..k], b[k]);
                        i = k + 1;
                    }
                    b']' | b'_' | b'P' => {
                        let kind = b[i + 1];
                        let mut k = i + 2;
                        let mut end = None;
                        while k < n {
                            if b[k] == 0x07 {
                                end = Some((k, 1));
                                break;
                            }
                            if b[k] == 0x1b && k + 1 < n && b[k + 1] == b'\\' {
                                end = Some((k, 2));
                                break;
                            }
                            k += 1;
                        }
                        let Some((e, l)) = end else { break };
                        let body = s[i + 2..e].to_string();
                        i = e + l;
                        match kind {
                            b']' => self.osc(&body),
                            b'_' => self.apc(&body),
                            _ => {}
                        }
                    }
                    _ => i += 2,
                }
                continue;
            }
            if b[i] == b'\r' {
                if let Some(a) = text_start.take() {
                    flush_text(self, a, i);
                }
                self.cx = 0;
                i += 1;
                continue;
            }
            if b[i] < 0x20 {
                if let Some(a) = text_start.take() {
                    flush_text(self, a, i);
                }
                i += 1;
                continue;
            }
            if text_start.is_none() {
                text_start = Some(i);
            }
            i += 1;
        }
        if let Some(a) = text_start.take() {
            flush_text(self, a, i.min(n));
        }
        if i < n {
            self.pending = b[i..].to_vec();
        }
    }

    fn csi(&mut self, p: &str, f: u8) {
        let nums: Vec<u32> = p.trim_start_matches(['?', '>', '<', '=']).split(';').map(|x| x.split(':').next().unwrap_or("").parse().unwrap_or(0)).collect();
        let n0 = nums.first().copied().unwrap_or(0);
        match f {
            b'H' | b'f' => {
                self.cy = (n0.max(1) as usize - 1).min(self.h - 1);
                self.cx = (nums.get(1).copied().unwrap_or(1).max(1) as usize - 1).min(self.w - 1);
            }
            b'K' => {
                let (a, e) = match n0 {
                    1 => (0, self.cx + 1),
                    2 => (0, self.w),
                    _ => (self.cx, self.w),
                };
                for x in a..e {
                    self.erase_multicell_at(x, self.cy);
                    self.cells[self.cy * self.w + x] = VCell { style: Style { bg: self.style.bg, ..Style::new() }, ..VCell::blank() };
                }
            }
            b'J' => {
                if n0 == 2 || n0 == 3 {
                    self.cells = vec![VCell::blank(); self.w * self.h];
                }
            }
            b'm' => self.sgr(p),
            b'h' | b'l' if p.starts_with('?') && p == "?25" => {
                self.cursor_visible = f == b'h';
            }
            _ => {}
        }
    }

    fn sgr(&mut self, p: &str) {
        let parts: Vec<&str> = p.split(';').collect();
        let mut i = 0;
        let mut st = self.style;
        while i < parts.len() {
            let part = parts[i];
            let sub: Vec<&str> = part.split(':').collect();
            let n: u32 = sub[0].parse().unwrap_or(0);
            match n {
                0 => {
                    let link = st.link;
                    st = Style::new();
                    st.link = link;
                }
                1 => st.attrs |= BOLD,
                2 => st.attrs |= DIM,
                3 => st.attrs |= ITALIC,
                4 => st.underline = sub.get(1).and_then(|x| x.parse().ok()).unwrap_or(1),
                7 => st.attrs |= REVERSE,
                9 => st.attrs |= STRIKE,
                22 => st.attrs &= !(BOLD | DIM),
                23 => st.attrs &= !ITALIC,
                24 => st.underline = 0,
                27 => st.attrs &= !REVERSE,
                29 => st.attrs &= !STRIKE,
                30..=37 => st.fg = Color::Idx((n - 30) as u8),
                90..=97 => st.fg = Color::Idx((n - 90 + 8) as u8),
                40..=47 => st.bg = Color::Idx((n - 40) as u8),
                100..=107 => st.bg = Color::Idx((n - 100 + 8) as u8),
                39 => st.fg = Color::Default,
                49 => st.bg = Color::Default,
                38 | 48 | 58 => {
                    let (c, used) = if sub.len() > 1 {
                        // colon form 58:2::r:g:b
                        let v: Vec<u8> = sub.iter().skip(2).filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect();
                        (if v.len() >= 3 { Color::Rgb(v[0], v[1], v[2]) } else { Color::Default }, 1)
                    } else if parts.get(i + 1) == Some(&"2") {
                        let g = |k: usize| parts.get(i + k).and_then(|x| x.parse::<u8>().ok()).unwrap_or(0);
                        (Color::Rgb(g(2), g(3), g(4)), 5)
                    } else if parts.get(i + 1) == Some(&"5") {
                        (Color::Idx(parts.get(i + 2).and_then(|x| x.parse().ok()).unwrap_or(0)), 3)
                    } else {
                        (Color::Default, 1)
                    };
                    match n {
                        38 => st.fg = c,
                        48 => st.bg = c,
                        _ => st.ul_color = c,
                    }
                    i += used;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        self.style = st;
    }

    fn osc(&mut self, body: &str) {
        if let Some(rest) = body.strip_prefix("8;") {
            let url = rest.split_once(';').map(|x| x.1).unwrap_or("");
            self.link = if url.is_empty() { None } else { Some(url.to_string()) };
        } else if let Some(rest) = body.strip_prefix("66;") {
            let (meta, text) = rest.split_once(';').unwrap_or(("", rest));
            let s = meta.split(':').find_map(|kv| kv.strip_prefix("s=")).and_then(|v| v.parse().ok()).unwrap_or(1);
            self.put_scaled(text, s);
        } else if let Some(t) = body.strip_prefix("2;") {
            self.title = t.to_string();
        } else if let Some(rest) = body.strip_prefix("52;")
            && let Some((_, b)) = rest.split_once(';')
        {
            self.clipboard = base64::engine::general_purpose::STANDARD.decode(b).ok().map(|v| String::from_utf8_lossy(&v).to_string());
        }
    }

    fn apc(&mut self, body: &str) {
        let Some(body) = body.strip_prefix('G') else { return };
        let (ctrl, payload) = body.split_once(';').unwrap_or((body, ""));
        let kv: HashMap<&str, &str> = ctrl.split(',').filter_map(|p| p.split_once('=')).collect();
        let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let more = num("m") == 1;
        // continuation chunk
        if let Some((c0, mut data)) = self.chunk.take() {
            data.extend_from_slice(payload.as_bytes());
            if more {
                self.chunk = Some((c0, data));
                return;
            }
            let full = format!("{c0};{}", String::from_utf8_lossy(&data));
            self.chunk = None;
            return self.apc(&format!("G{}", full.replacen("m=1", "m=0", 1)));
        }
        if more {
            self.chunk = Some((ctrl.to_string(), payload.as_bytes().to_vec()));
            return;
        }
        match kv.get("a").copied().unwrap_or("t") {
            "T" | "t" => {
                let png = base64::engine::general_purpose::STANDARD.decode(payload).unwrap_or_default();
                let id = num("i");
                self.images.insert(id, VImage { png, cols: num("c"), rows: num("r") });
                if kv.get("a") == Some(&"T") && kv.get("U") != Some(&"1") {
                    self.placements.push(VPlacement { id, pid: num("p"), x: self.cx, y: self.cy, src: (0, 0, 0, 0) });
                }
            }
            "p" => {
                let id = num("i");
                let pid = num("p");
                self.placements.retain(|p| !(p.id == id && p.pid == pid));
                self.placements.push(VPlacement { id, pid, x: self.cx, y: self.cy, src: (num("x"), num("y"), num("w"), num("h")) });
            }
            "d" => match kv.get("d").copied().unwrap_or("a") {
                "i" | "I" => {
                    let id = num("i");
                    let pid = num("p");
                    self.placements.retain(|p| !(p.id == id && (pid == 0 || p.pid == pid)));
                    if kv.get("d") == Some(&"I") {
                        self.images.remove(&id);
                    }
                }
                _ => {
                    self.placements.clear();
                    if kv.get("d") == Some(&"A") {
                        self.images.clear();
                    }
                }
            },
            _ => {}
        }
    }

    /// Images visible through Unicode placeholders: (id, image row, image col, x, y).
    pub fn placeholder_cells(&self) -> Vec<(u32, usize, usize, usize, usize)> {
        let mut out = vec![];
        for y in 0..self.h {
            for x in 0..self.w {
                let c = self.cell(x, y);
                if !c.text.starts_with(PLACEHOLDER) {
                    continue;
                }
                let id = match c.style.fg {
                    Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
                    _ => 0,
                };
                let mut ch = c.text.chars().skip(1);
                let row = ch.next().and_then(diacritic_index).unwrap_or(0);
                let col = ch.next().and_then(diacritic_index).unwrap_or(0);
                out.push((id, row, col, x, y));
            }
        }
        out
    }

    /// Render the screen to an image (cell size cw×ch).
    pub fn screenshot(
        &self,
        font: &ab_glyph::FontVec,
        bold: Option<&ab_glyph::FontVec>,
        cw: u32,
        ch: u32,
        default_fg: (u8, u8, u8),
        default_bg: (u8, u8, u8),
    ) -> RgbaImage {
        use ab_glyph::{Font, ScaleFont};
        let mut img = RgbaImage::from_pixel(self.w as u32 * cw, self.h as u32 * ch, Rgba([default_bg.0, default_bg.1, default_bg.2, 255]));
        let rgb = |c: Color, def: (u8, u8, u8)| -> (u8, u8, u8) {
            match c {
                Color::Rgb(r, g, b) => (r, g, b),
                Color::Idx(i) => {
                    const P: [(u8, u8, u8); 16] = [
                        (0, 0, 0),
                        (205, 49, 49),
                        (13, 188, 121),
                        (229, 229, 16),
                        (36, 114, 200),
                        (188, 63, 188),
                        (17, 168, 205),
                        (229, 229, 229),
                        (102, 102, 102),
                        (241, 76, 76),
                        (35, 209, 139),
                        (245, 245, 67),
                        (59, 142, 234),
                        (214, 112, 214),
                        (41, 184, 219),
                        (255, 255, 255),
                    ];
                    P[(i as usize) % 16]
                }
                Color::Default => def,
            }
        };
        let decoded: HashMap<u32, RgbaImage> =
            self.images.iter().filter_map(|(id, im)| image::load_from_memory(&im.png).ok().map(|d| (*id, d.to_rgba8()))).collect();
        // backgrounds
        for y in 0..self.h {
            for x in 0..self.w {
                let c = self.cell(x, y);
                let (mut fg, mut bg) = (rgb(c.style.fg, default_fg), rgb(c.style.bg, default_bg));
                if c.style.has(REVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let _ = fg;
                if c.flag == 2 {
                    continue;
                }
                let s = c.scale.max(1) as u32;
                let wcells = if c.flag == 0 && s > 1 { grapheme_width(&c.text).max(1) as u32 * s } else { 1 };
                for py in 0..ch * s {
                    for px in 0..cw * wcells {
                        let (ix, iy) = (x as u32 * cw + px, y as u32 * ch + py);
                        if ix < img.width() && iy < img.height() {
                            img.put_pixel(ix, iy, Rgba([bg.0, bg.1, bg.2, 255]));
                        }
                    }
                }
            }
        }
        let blend = |img: &mut RgbaImage, x: u32, y: u32, col: (u8, u8, u8), a: f32| {
            if x < img.width() && y < img.height() {
                let p = img.get_pixel_mut(x, y);
                let m = |b: u8, f: u8| (b as f32 + (f as f32 - b as f32) * a).round() as u8;
                *p = Rgba([m(p[0], col.0), m(p[1], col.1), m(p[2], col.2), 255]);
            }
        };
        // glyphs
        for y in 0..self.h {
            for x in 0..self.w {
                let c = self.cell(x, y);
                if c.flag != 0 || c.text.trim().is_empty() || c.text.starts_with(PLACEHOLDER) {
                    continue;
                }
                let (mut fg, mut bg) = (rgb(c.style.fg, default_fg), rgb(c.style.bg, default_bg));
                if c.style.has(REVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if c.style.has(DIM) {
                    fg = ((fg.0 as u16 + bg.0 as u16) as u8 / 2, (fg.1 as u16 + bg.1 as u16) as u8 / 2, (fg.2 as u16 + bg.2 as u16) as u8 / 2);
                }
                let s = c.scale.max(1) as f32;
                let f = if c.style.has(BOLD) { bold.unwrap_or(font) } else { font };
                let px = ch as f32 * 0.82 * s;
                let sf = f.as_scaled(px);
                let ox = x as f32 * cw as f32;
                let base = y as f32 * ch as f32 + (ch as f32 * s - (sf.ascent() - sf.descent())) / 2.0 + sf.ascent();
                let ch0 = c.text.chars().next().unwrap();
                let gid = f.glyph_id(ch0);
                let italic = if c.style.has(ITALIC) { 0.18 } else { 0.0 };
                let g = gid.with_scale_and_position(px, ab_glyph::point(ox, base));
                if let Some(og) = f.outline_glyph(g) {
                    let bb = og.px_bounds();
                    og.draw(|gx, gy, cov| {
                        let yy = bb.min.y as i32 + gy as i32;
                        let skew = ((base - yy as f32) * italic) as i32;
                        let xx = bb.min.x as i32 + gx as i32 + skew;
                        if xx >= 0 && yy >= 0 {
                            blend(&mut img, xx as u32, yy as u32, fg, cov.clamp(0.0, 1.0));
                        }
                    });
                }
                let wcells = grapheme_width(&c.text).max(1) as u32 * s as u32;
                if c.style.underline != 0 {
                    let uc = rgb(if c.style.ul_color.is_set() { c.style.ul_color } else { c.style.fg }, default_fg);
                    let uy = (y as u32 + s as u32) * ch - 3;
                    for px in 0..cw * wcells {
                        if c.style.underline == 4 && px % 3 == 2 {
                            continue;
                        }
                        blend(&mut img, x as u32 * cw + px, uy, uc, 0.9);
                    }
                }
                if c.style.has(STRIKE) {
                    let sy = y as u32 * ch + ch / 2 + 1;
                    for px in 0..cw * wcells {
                        blend(&mut img, x as u32 * cw + px, sy, fg, 0.9);
                    }
                }
            }
        }
        // placeholder images
        for (id, row, col, x, y) in self.placeholder_cells() {
            if let Some(src) = decoded.get(&id) {
                for py in 0..ch {
                    for px in 0..cw {
                        let (sx, sy) = (col as u32 * cw + px, row as u32 * ch + py);
                        if sx < src.width() && sy < src.height() {
                            let p = src.get_pixel(sx, sy);
                            blend(&mut img, x as u32 * cw + px, y as u32 * ch + py, (p[0], p[1], p[2]), p[3] as f32 / 255.0);
                        }
                    }
                }
            }
        }
        // classic placements
        for pl in &self.placements {
            if let Some(src) = decoded.get(&pl.id) {
                let (sx0, sy0, sw, sh) = if pl.src.2 > 0 { pl.src } else { (0, 0, src.width(), src.height()) };
                for py in 0..sh {
                    for px in 0..sw {
                        let (sx, sy) = (sx0 + px, sy0 + py);
                        if sx < src.width() && sy < src.height() {
                            let p = src.get_pixel(sx, sy);
                            blend(&mut img, pl.x as u32 * cw + px, pl.y as u32 * ch + py, (p[0], p[1], p[2]), p[3] as f32 / 255.0);
                        }
                    }
                }
            }
        }
        // cursor
        if self.cursor_visible && self.cx < self.w && self.cy < self.h {
            for py in 2..ch - 2 {
                blend(&mut img, self.cx as u32 * cw, self.cy as u32 * ch + py, default_fg, 1.0);
                blend(&mut img, self.cx as u32 * cw + 1, self.cy as u32 * ch + py, default_fg, 1.0);
            }
        }
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_parse() {
        let mut v = Vt::new(20, 4);
        v.feed("\x1b[2;3H\x1b[0;1;38;2;255;0;0mhi\x1b[0m 日本");
        assert_eq!(v.row(1), "  hi 日本");
        assert!(v.cell(2, 1).style.has(BOLD));
        assert_eq!(v.cell(2, 1).style.fg, Color::Rgb(255, 0, 0));
        v.feed("\x1b[1;1H\x1b]66;s=2;Big\x07");
        assert_eq!(v.cell(0, 0).scale, 2);
        assert_eq!(v.cell(1, 1).flag, 2);
        assert_eq!(v.row(0), "Big");
        // overwriting part of a multicell char erases that whole char only
        v.feed("\x1b[2;2Hx");
        assert_eq!(v.cell(0, 0).scale, 1);
        assert_eq!(v.row(0), "  ig");
        assert_eq!(v.row(1), " x本");
    }
}
