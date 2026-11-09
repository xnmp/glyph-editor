//! Image loading/scaling/caching. Decoding and resizing happen on worker
//! threads; the UI asks for an image every frame and gets Pending until ready.
//! Images are padded to an exact cell grid so placeholder and placement modes
//! render identically and crop cleanly when partially scrolled off-screen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, Rgba, RgbaImage};

use crate::screen::ImageData;

pub struct Img {
    pub data: Arc<ImageData>,
    /// heading images: x (px) of each char boundary, for click mapping
    pub char_xs: Vec<f32>,
}

/// An image used inline as an icon; compared by image id.
#[derive(Clone)]
pub struct IconRef(pub Arc<Img>);

impl PartialEq for IconRef {
    fn eq(&self, o: &Self) -> bool {
        self.0.data.id == o.0.data.id
    }
}

impl std::fmt::Debug for IconRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Icon#{}", self.0.data.id)
    }
}

/// Lucide icons (the set Obsidian uses), keyed by callout type.
pub fn callout_svg(kind: &str) -> &'static str {
    match kind {
        "abstract" | "summary" | "tldr" => include_str!("icons/clipboard-list.svg"),
        "info" => include_str!("icons/info.svg"),
        "todo" => include_str!("icons/circle-check.svg"),
        "tip" | "hint" | "important" => include_str!("icons/flame.svg"),
        "success" | "check" | "done" => include_str!("icons/check.svg"),
        "question" | "help" | "faq" => include_str!("icons/circle-question-mark.svg"),
        "warning" | "caution" | "attention" => include_str!("icons/triangle-alert.svg"),
        "failure" | "fail" | "missing" => include_str!("icons/x.svg"),
        "danger" | "error" => include_str!("icons/zap.svg"),
        "bug" => include_str!("icons/bug.svg"),
        "example" => include_str!("icons/list.svg"),
        "quote" | "cite" => include_str!("icons/quote.svg"),
        _ => include_str!("icons/pencil.svg"),
    }
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Obsidian-style rounded checkbox for a task state.
pub fn checkbox_svg(state: char, stroke: (u8, u8, u8), accent: (u8, u8, u8), check: (u8, u8, u8)) -> Option<String> {
    let rect = |fill: &str, s: &str| format!(r#"<rect x="3" y="3" width="18" height="18" rx="4.5" fill="{fill}" stroke="{s}" stroke-width="1.8"/>"#);
    let body = match state {
        ' ' => rect("none", &hex(stroke)),
        'x' | 'X' => format!(
            r#"{}<path d="M7.5 12.4l3.1 3.1 6-6.6" fill="none" stroke="{}" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>"#,
            rect(&hex(accent), &hex(accent)),
            hex(check)
        ),
        '/' => format!(r#"{}<path d="M4 20 L20 4 L20 20 Z" fill="{}" opacity="0.9"/>"#, rect("none", &hex(accent)), hex(accent)),
        '-' => format!(r#"{}<path d="M7.5 12h9" stroke="{}" stroke-width="2.4" stroke-linecap="round"/>"#, rect("none", &hex(stroke)), hex(stroke)),
        _ => return None,
    };
    Some(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">{body}</svg>"#))
}

/// Rasterize an SVG centred in a `w`×`h` canvas, the icon box being `box_px` square.
pub fn render_svg(svg: &str, w: u32, h: u32, box_px: f32) -> Option<RgbaImage> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pm = resvg::tiny_skia::Pixmap::new(w, h)?;
    let size = tree.size();
    let s = box_px / size.width().max(size.height());
    let tx = (w as f32 - size.width() * s) / 2.0;
    let ty = (h as f32 - size.height() * s) / 2.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_row(s, 0.0, 0.0, s, tx, ty), &mut pm.as_mut());
    let mut out = RgbaImage::new(w, h);
    for (i, p) in pm.pixels().iter().enumerate() {
        let c = p.demultiply();
        out.put_pixel(i as u32 % w, i as u32 / w, Rgba([c.red(), c.green(), c.blue(), c.alpha()]));
    }
    Some(out)
}

#[derive(Clone)]
pub enum ImgRef {
    Pending,
    Ready(Arc<Img>),
    Error(String),
}

impl ImgRef {
    pub fn ready(&self) -> Option<&Arc<Img>> {
        match self {
            ImgRef::Ready(i) => Some(i),
            _ => None,
        }
    }
}

type Job = Box<dyn FnOnce() + Send>;
pub type Waker = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    slots: HashMap<String, ImgRef>,
    next_id: u32,
}

pub struct ImageStore {
    pub cell_w: u32,
    pub cell_h: u32,
    shared: Arc<Mutex<Shared>>,
    tx: Option<Sender<Job>>,
    waker: Waker,
    pub base_dir: PathBuf,
    pub vault_root: PathBuf,
    /// lowercase basename → path (Obsidian resolves embeds by name)
    pub by_name: HashMap<String, PathBuf>,
    pub allow_remote: bool,
    font: Option<Arc<ab_glyph::FontVec>>,
    pub synchronous: bool,
    pub epoch: u64,
    /// bumped by workers whenever an image finishes (part of the layout cache key)
    pub ready: Arc<AtomicU64>,
}

fn new_id(sh: &Arc<Mutex<Shared>>) -> u32 {
    let mut g = sh.lock().unwrap();
    g.next_id += 1;
    if g.next_id >= 0xFF_FFFF {
        g.next_id = 0x01_0001;
    }
    g.next_id
}

impl ImageStore {
    pub fn new(cell_w: u32, cell_h: u32, waker: Waker) -> ImageStore {
        let (tx, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..2 {
            let rx = rx.clone();
            std::thread::spawn(move || {
                loop {
                    let job = { rx.lock().unwrap().recv() };
                    match job {
                        Ok(j) => j(),
                        Err(_) => break,
                    }
                }
            });
        }
        ImageStore {
            cell_w: cell_w.max(1),
            cell_h: cell_h.max(1),
            shared: Arc::new(Mutex::new(Shared { slots: HashMap::new(), next_id: 0x01_0001 })),
            tx: Some(tx),
            waker,
            base_dir: PathBuf::from("."),
            vault_root: PathBuf::from("."),
            by_name: HashMap::new(),
            allow_remote: true,
            font: load_font().map(Arc::new),
            synchronous: false,
            epoch: 0,
            ready: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_cell_size(&mut self, w: u32, h: u32) {
        if (w, h) != (self.cell_w, self.cell_h) && w > 0 && h > 0 {
            self.cell_w = w;
            self.cell_h = h;
            self.shared.lock().unwrap().slots.clear();
            self.epoch += 1;
        }
    }

    pub fn generation(&self) -> u64 {
        self.epoch * 1_000_003 + self.ready.load(Ordering::SeqCst)
    }

    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    fn submit(&self, key: String, job: impl FnOnce() -> Result<(RgbaImage, Vec<f32>), String> + Send + 'static) -> ImgRef {
        {
            let mut g = self.shared.lock().unwrap();
            if let Some(r) = g.slots.get(&key) {
                return r.clone();
            }
            g.slots.insert(key.clone(), ImgRef::Pending);
        }
        let shared = self.shared.clone();
        let waker = self.waker.clone();
        let ready = self.ready.clone();
        let (cw, ch) = (self.cell_w, self.cell_h);
        let run = move || {
            let r = match job() {
                Ok((im, xs)) => {
                    let id = new_id(&shared);
                    ImgRef::Ready(Arc::new(Img { data: Arc::new(encode_grid(&im, cw, ch, id)), char_xs: xs }))
                }
                Err(e) => ImgRef::Error(e),
            };
            shared.lock().unwrap().slots.insert(key, r);
            ready.fetch_add(1, Ordering::SeqCst);
            waker();
        };
        if self.synchronous {
            run();
        } else if let Some(tx) = &self.tx {
            let _ = tx.send(Box::new(run));
        }
        ImgRef::Pending
    }

    fn lookup(&self, key: &str) -> Option<ImgRef> {
        self.shared.lock().unwrap().slots.get(key).cloned()
    }

    pub fn resolve(&self, src: &str) -> Option<PathBuf> {
        if src.contains("://") {
            return Some(PathBuf::from(src));
        }
        let s = percent_decode(src.split(['#', '?']).next().unwrap_or(src));
        let s = if let Some(rest) = s.strip_prefix("~/") {
            std::env::var_os("HOME").map(|h| Path::new(&h).join(rest).to_string_lossy().to_string()).unwrap_or(s.clone())
        } else {
            s
        };
        let p = Path::new(&s);
        let cands: Vec<PathBuf> = if p.is_absolute() { vec![p.to_path_buf()] } else { vec![self.base_dir.join(p), self.vault_root.join(p)] };
        for c in cands {
            if c.is_file() {
                return Some(c);
            }
        }
        let name = p.file_name()?.to_string_lossy().to_lowercase();
        self.by_name.get(&name).cloned()
    }

    /// An image file or URL fitted into max_cols × max_rows cells.
    pub fn get(&self, src: &str, max_cols: u32, max_rows: u32, req_w: Option<u32>, req_h: Option<u32>) -> ImgRef {
        let Some(path) = self.resolve(src) else {
            return ImgRef::Error("not found".into());
        };
        let remote = path.to_string_lossy().contains("://");
        if remote && !self.allow_remote {
            return ImgRef::Error("remote images disabled".into());
        }
        let mtime = if remote {
            0
        } else {
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))
                .unwrap_or(0)
        };
        let key = format!("img|{}|{mtime}|{max_cols}|{max_rows}|{req_w:?}|{req_h:?}", path.display());
        if let Some(r) = self.lookup(&key) {
            return r;
        }
        let (cw, ch) = (self.cell_w, self.cell_h);
        self.submit(key.clone(), move || {
            let im = if remote {
                fetch_remote(&path.to_string_lossy())?
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
                let svg = std::fs::read_to_string(&path).map_err(|e| short(e.to_string()))?;
                let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).map_err(|e| short(e.to_string()))?;
                let sz = tree.size();
                let k = (2.0f32).min(4096.0 / sz.width().max(sz.height()));
                let (w, h) = ((sz.width() * k).ceil() as u32, (sz.height() * k).ceil() as u32);
                DynamicImage::ImageRgba8(render_svg(&svg, w.max(1), h.max(1), (w.max(h)) as f32).ok_or("svg render failed")?)
            } else {
                image::open(&path).map_err(|e| short(e.to_string()))?
            };
            Ok((fit(im, max_cols * cw, max_rows * ch, req_w, req_h), vec![]))
        });
        self.lookup(&key).unwrap_or(ImgRef::Pending)
    }

    /// A small inline icon (2 cells × 1 row) from SVG markup, tinted `color`.
    pub fn icon(&self, svg: &str, color: (u8, u8, u8), scale: f32) -> Option<IconRef> {
        let svg = svg.replace("currentColor", &hex(color));
        let key = format!("icon|{svg}|{scale}|{}x{}", self.cell_w, self.cell_h);
        if let Some(ImgRef::Ready(i)) = self.lookup(&key) {
            return Some(IconRef(i));
        }
        let (w, h) = (self.cell_w * 2, self.cell_h);
        let im = render_svg(&svg, w, h, (w.min(h) as f32) * scale)?;
        let id = new_id(&self.shared);
        let img = Arc::new(Img { data: Arc::new(encode_grid(&im, self.cell_w, self.cell_h, id)), char_xs: vec![] });
        self.shared.lock().unwrap().slots.insert(key, ImgRef::Ready(img.clone()));
        Some(IconRef(img))
    }

    /// Encode a freshly drawn image (typeset text) under a new id.
    pub fn adhoc(&self, im: &RgbaImage) -> Arc<Img> {
        let id = new_id(&self.shared);
        Arc::new(Img { data: Arc::new(encode_grid(im, self.cell_w, self.cell_h, id)), char_xs: vec![] })
    }

    /// Heading text drawn with a real font, `rows` cells tall.
    pub fn heading(&self, text: &str, level: u8, max_cols: u32, color: (u8, u8, u8)) -> ImgRef {
        let Some(font) = self.font.clone() else { return ImgRef::Error("no font".into()) };
        let key = format!("h|{text}|{level}|{max_cols}|{color:?}|{}x{}", self.cell_w, self.cell_h);
        if let Some(r) = self.lookup(&key) {
            return r;
        }
        let (cw, ch) = (self.cell_w, self.cell_h);
        let text = text.to_string();
        // headings render synchronously: they are small and must not flicker
        let r = render_text(
            &font,
            &text,
            match level {
                1 => 1.55,
                2 => 1.3,
                _ => 1.1,
            } * ch as f32,
            max_cols * cw,
            2 * ch,
            color,
        );
        let out = match r {
            Some((im, xs)) => {
                let id = new_id(&self.shared);
                ImgRef::Ready(Arc::new(Img { data: Arc::new(encode_grid(&im, cw, ch, id)), char_xs: xs }))
            }
            None => ImgRef::Error("render failed".into()),
        };
        self.shared.lock().unwrap().slots.insert(key, out.clone());
        out
    }
}

impl Drop for ImageStore {
    fn drop(&mut self) {
        self.tx.take();
    }
}

fn short(s: String) -> String {
    s.chars().take(80).collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && s.is_char_boundary(i + 3)
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn fetch_remote(url: &str) -> Result<DynamicImage, String> {
    let out = std::process::Command::new("curl")
        .args(["-sSLf", "--max-time", "15", "--max-filesize", "33554432", "-A", "glyph-markdown-editor", url])
        .output()
        .map_err(|_| "curl not available".to_string())?;
    if !out.status.success() {
        return Err(short(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    image::load_from_memory(&out.stdout).map_err(|e| short(e.to_string()))
}

/// Scale to fit max px box (never upscale unless an explicit width is given).
pub fn fit(im: DynamicImage, max_w: u32, max_h: u32, req_w: Option<u32>, req_h: Option<u32>) -> RgbaImage {
    let (w0, h0) = (im.width().max(1), im.height().max(1));
    let (mut tw, mut th) = match (req_w, req_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, (h0 as f64 * w as f64 / w0 as f64).round() as u32),
        _ => (w0, h0),
    };
    let s = (max_w as f64 / tw.max(1) as f64).min(max_h as f64 / th.max(1) as f64).min(1.0);
    tw = ((tw as f64 * s).round() as u32).max(1);
    th = ((th as f64 * s).round() as u32).max(1);
    if (tw, th) == (w0, h0) { im.to_rgba8() } else { im.resize_exact(tw, th, image::imageops::FilterType::Lanczos3).to_rgba8() }
}

/// Pad to whole cells and PNG-encode.
pub fn encode_grid(im: &RgbaImage, cw: u32, ch: u32, id: u32) -> ImageData {
    let cols = im.width().div_ceil(cw).max(1);
    let rows = im.height().div_ceil(ch).max(1);
    let mut canvas = RgbaImage::from_pixel(cols * cw, rows * ch, Rgba([0, 0, 0, 0]));
    image::imageops::overlay(&mut canvas, im, 0, 0);
    let mut png = Vec::new();
    let enc = image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive);
    let _ = DynamicImage::ImageRgba8(canvas).write_with_encoder(enc);
    ImageData { id, png, cols, rows }
}

const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/inter/Inter-Bold.otf",
    "/usr/share/fonts/truetype/inter/Inter-Bold.ttf",
    "/usr/share/fonts/inter/Inter-Bold.otf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/Library/Fonts/Arial Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    "/usr/share/fonts/noto/NotoSans-Bold.ttf",
];

fn load_font() -> Option<ab_glyph::FontVec> {
    let mut cands: Vec<PathBuf> = vec![];
    if let Ok(p) = std::env::var("GLYPH_HEADING_FONT") {
        cands.push(p.into());
    }
    cands.extend(FONT_CANDIDATES.iter().map(PathBuf::from));
    for c in &cands {
        if let Ok(bytes) = std::fs::read(c)
            && let Ok(f) = ab_glyph::FontVec::try_from_vec(bytes)
        {
            return Some(f);
        }
    }
    if let Ok(out) = std::process::Command::new("fc-match").args(["-f", "%{file}", "sans:bold"]).output() {
        let p = String::from_utf8_lossy(&out.stdout).to_string();
        if let Ok(bytes) = std::fs::read(&p) {
            return ab_glyph::FontVec::try_from_vec(bytes).ok();
        }
    }
    None
}

/// Render one line of text; shrinks the size to fit `max_w`, then ellipsizes.
pub fn render_text(font: &ab_glyph::FontVec, text: &str, px: f32, max_w: u32, h: u32, color: (u8, u8, u8)) -> Option<(RgbaImage, Vec<f32>)> {
    use ab_glyph::{Font, ScaleFont};
    let measure = |t: &str, px: f32| -> f32 {
        let sf = font.as_scaled(px);
        let mut w = 0.0;
        let mut prev = None;
        for c in t.chars() {
            let g = sf.glyph_id(c);
            if let Some(p) = prev {
                w += sf.kern(p, g);
            }
            w += sf.h_advance(g);
            prev = Some(g);
        }
        w
    };
    let mut px = px;
    let min_px = h as f32 * 0.4;
    while px > min_px && measure(text, px) > max_w as f32 {
        px -= 1.0;
    }
    let mut t: String = text.to_string();
    while !t.is_empty() && measure(&t, px) > max_w as f32 {
        t.pop();
        t.pop();
        t.push('…');
    }
    let sf = font.as_scaled(px);
    let w = (measure(&t, px).ceil() as u32 + 2).clamp(1, max_w.max(1));
    let mut im = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
    let asc = sf.ascent();
    let desc = sf.descent();
    let baseline = ((h as f32 - (asc - desc)) / 2.0 + asc).round();
    let mut x = 0.0f32;
    let mut prev = None;
    let mut xs = vec![0.0];
    for c in t.chars() {
        let gid = sf.glyph_id(c);
        if let Some(p) = prev {
            x += sf.kern(p, gid);
        }
        let g = gid.with_scale_and_position(px, ab_glyph::point(x, baseline));
        if let Some(og) = font.outline_glyph(g) {
            let bb = og.px_bounds();
            og.draw(|gx, gy, cov| {
                let px_ = gx as i32 + bb.min.x as i32;
                let py = gy as i32 + bb.min.y as i32;
                if px_ >= 0 && py >= 0 && (px_ as u32) < w && (py as u32) < h {
                    let p = im.get_pixel_mut(px_ as u32, py as u32);
                    let a = (cov.clamp(0.0, 1.0) * 255.0) as u8;
                    if a > p.0[3] {
                        *p = Rgba([color.0, color.1, color.2, a]);
                    }
                }
            });
        }
        x += sf.h_advance(gid);
        xs.push(x);
        prev = Some(gid);
    }
    Some((im, xs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_padding_and_fit() {
        let im = DynamicImage::ImageRgba8(RgbaImage::from_pixel(250, 100, Rgba([255, 0, 0, 255])));
        let f = fit(im, 100, 400, None, None);
        assert_eq!((f.width(), f.height()), (100, 40));
        let d = encode_grid(&f, 10, 20, 7);
        assert_eq!((d.cols, d.rows), (10, 2));
        let back = image::load_from_memory(&d.png).unwrap();
        assert_eq!((back.width(), back.height()), (100, 40));
    }

    #[test]
    fn async_load() {
        let dir = std::env::temp_dir().join(format!("glyph-img-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        RgbaImage::from_pixel(30, 30, Rgba([0, 255, 0, 255])).save(&p).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let mut st = ImageStore::new(
            10,
            20,
            Arc::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        st.base_dir = dir.clone();
        // the worker may already be done by the time get() returns
        if matches!(st.get("a.png", 80, 20, None, None), ImgRef::Pending) {
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        }
        let r = st.get("a.png", 80, 20, None, None);
        let img = r.ready().expect("ready");
        assert_eq!((img.data.cols, img.data.rows), (3, 2));
        assert!(matches!(st.get("missing.png", 80, 20, None, None), ImgRef::Error(_)));
        st.by_name.insert("a.png".into(), p.clone());
        st.base_dir = PathBuf::from("/nonexistent");
        st.vault_root = PathBuf::from("/nonexistent");
        assert!(st.resolve("sub/a.png").is_some());
        assert_eq!(percent_decode("my%20pic.png"), "my pic.png");
    }

    #[test]
    fn heading_render() {
        let st = ImageStore::new(10, 20, Arc::new(|| {}));
        if !st.has_font() {
            return;
        }
        let r = st.heading("Hello", 1, 80, (255, 255, 255));
        let img = r.ready().unwrap();
        assert_eq!(img.data.rows, 2);
        assert_eq!(img.char_xs.len(), 6);
    }
}
