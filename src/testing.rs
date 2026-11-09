//! Headless driver: runs the real `App` against the virtual terminal. Used by
//! the integration tests and by `examples/snap.rs` to produce screenshots.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::app::{App, Config};
use crate::input::{ALT, CTRL, Event, Key, Mouse, MouseKind, SHIFT, SUPER};
use crate::term::Caps;
use crate::theme::Palette;
use crate::vt::Vt;

pub struct Harness {
    pub app: App,
    pub vt: Vt,
    pub dir: PathBuf,
    pub out_bytes: usize,
}

pub fn kitty_caps() -> Caps {
    let mut p = Palette { fg: Some((218, 218, 218)), bg: Some((30, 30, 30)), ..Default::default() };
    let ansi = [(30, 30, 30), (233, 82, 92), (80, 200, 120), (229, 192, 80), (90, 150, 250), (150, 110, 245), (60, 190, 200), (218, 218, 218)];
    for (i, c) in ansi.iter().enumerate() {
        p.ansi[i] = Some(*c);
    }
    Caps { graphics: true, placeholders: true, text_sizing: true, kitty_keyboard: true, cell_w: 10, cell_h: 20, name: "kitty(0.43.1)".into(), palette: p }
}

/// Parse "ctrl+shift+left" style names into a Key.
pub fn key(name: &str) -> Key {
    let parts: Vec<&str> = if name == "+" { vec!["+"] } else { name.split('+').collect() };
    let (mods_s, k) = parts.split_at(parts.len() - 1);
    let mut mods = 0;
    for m in mods_s {
        mods |= match *m {
            "ctrl" => CTRL,
            "alt" => ALT,
            "shift" => SHIFT,
            "super" => SUPER,
            _ => 0,
        };
    }
    let k = k[0];
    if k.chars().count() == 1 && mods & (CTRL | ALT | SUPER) == 0 {
        return Key::text(k);
    }
    if k == "space" && mods == 0 {
        return Key { key: "space".into(), mods: 0, text: " ".into() };
    }
    Key::new(k, mods)
}

impl Harness {
    pub fn with_caps(w: usize, h: usize, files: &[(&str, &str)], caps: Caps, config: Config) -> Harness {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("glyph-h-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
        for (name, content) in files {
            let p = dir.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        Harness::open_dir(w, h, &dir, files.first().map(|f| f.0), caps, config)
    }

    pub fn open_dir(w: usize, h: usize, dir: &Path, first: Option<&str>, caps: Caps, config: Config) -> Harness {
        let mut app = App::new(caps, w, h, dir, config, Arc::new(|| {}));
        app.images.synchronous = true;
        app.open_url = Box::new(|_| {});
        app.system_paste = Box::new(|| None);
        match first {
            Some(f) => app.open(&dir.join(f), false),
            None => app.open_text(""),
        }
        let mut h = Harness { app, vt: Vt::new(w, h), dir: dir.to_path_buf(), out_bytes: 0 };
        h.frame();
        h
    }

    pub fn new(w: usize, h: usize, files: &[(&str, &str)]) -> Harness {
        let config = Config { autosave: false, ..Config::default() };
        Harness::with_caps(w, h, files, kitty_caps(), config)
    }

    /// Render a frame into the virtual terminal; returns the screen text.
    pub fn frame(&mut self) -> String {
        let out = self.app.render();
        self.out_bytes = out.len();
        self.vt.feed(&out);
        self.vt.text()
    }

    /// Space-separated key names, e.g. "ctrl+s down down enter".
    pub fn keys(&mut self, spec: &str) -> String {
        for k in spec.split_whitespace() {
            self.app.handle(Event::Key(key(k)));
            self.frame();
        }
        self.vt.text()
    }

    pub fn type_text(&mut self, s: &str) -> String {
        for c in s.chars() {
            let k = match c {
                '\n' => Key::new("enter", 0),
                '\t' => Key::new("tab", 0),
                ' ' => Key { key: "space".into(), mods: 0, text: " ".into() },
                c => Key::text(&c.to_string()),
            };
            self.app.handle(Event::Key(k));
        }
        self.frame()
    }

    pub fn paste(&mut self, s: &str) -> String {
        self.app.handle(Event::Paste(s.into()));
        self.frame()
    }

    pub fn click(&mut self, x: usize, y: usize, mods: u8) -> String {
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Press, button: 0, x: x as u16, y: y as u16, mods }));
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Release, button: 0, x: x as u16, y: y as u16, mods }));
        self.frame()
    }

    pub fn drag(&mut self, from: (usize, usize), to: (usize, usize)) -> String {
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Press, button: 0, x: from.0 as u16, y: from.1 as u16, mods: 0 }));
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Drag, button: 0, x: to.0 as u16, y: to.1 as u16, mods: 0 }));
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Release, button: 0, x: to.0 as u16, y: to.1 as u16, mods: 0 }));
        self.frame()
    }

    pub fn wheel(&mut self, x: usize, y: usize, down: bool) -> String {
        self.app.handle(Event::Mouse(Mouse { kind: MouseKind::Wheel, button: if down { 1 } else { 0 }, x: x as u16, y: y as u16, mods: 0 }));
        self.frame()
    }

    pub fn text(&self) -> String {
        self.app.tabs[self.app.cur].ed.buf.text()
    }

    pub fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    pub fn screenshot(&self, path: &Path) -> std::io::Result<()> {
        let font = load_mono(false).ok_or_else(|| std::io::Error::other("no monospace font"))?;
        let bold = load_mono(true);
        let img = self.vt.screenshot(&font, bold.as_ref(), self.app.images.cell_w, self.app.images.cell_h, (218, 218, 218), (30, 30, 30));
        img.save(path).map_err(std::io::Error::other)
    }
}

pub fn load_mono(bold: bool) -> Option<ab_glyph::FontVec> {
    let cands: &[&str] = if bold {
        &["/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", "/usr/share/fonts/TTF/DejaVuSansMono-Bold.ttf"]
    } else {
        &["/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", "/usr/share/fonts/TTF/DejaVuSansMono.ttf"]
    };
    for c in cands {
        if let Ok(b) = std::fs::read(c)
            && let Ok(f) = ab_glyph::FontVec::try_from_vec(b)
        {
            return Some(f);
        }
    }
    None
}
