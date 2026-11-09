//! Generates the README screenshots through the virtual terminal.
//!   cargo run --release --example gallery -- <out-dir>
use glyph::app::Config;
use glyph::input::{Event, Mouse, MouseKind};
use glyph::term::Caps;
use glyph::testing::{Harness, kitty_caps};
use std::path::{Path, PathBuf};

fn shot(name: &str, out: &Path, caps: Caps, config: Config, setup: impl FnOnce(&mut Harness)) {
    let vault = PathBuf::from("examples/demo");
    let tmp = std::env::temp_dir().join(format!("glyph-gallery-{name}"));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(&vault, &tmp);
    let mut h = Harness::open_dir(120, 44, &tmp, Some("Welcome.md"), caps, config);
    setup(&mut h);
    h.frame();
    let p = out.join(format!("{name}.png"));
    h.screenshot(&p).unwrap();
    println!("{}", p.display());
}

fn copy_dir(a: &Path, b: &Path) {
    std::fs::create_dir_all(b).unwrap();
    for e in std::fs::read_dir(a).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &b.join(e.file_name()));
        } else {
            std::fs::copy(&p, b.join(e.file_name())).unwrap();
        }
    }
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or("docs/screenshots".into()));
    std::fs::create_dir_all(&out).unwrap();
    let cfg = || Config { autosave: false, ..Config::default() };
    shot("live-preview", &out, kitty_caps(), cfg(), |h| {
        h.keys("ctrl+\\ ctrl+shift+\\");
        h.keys("down down");
    });
    shot("editing", &out, kitty_caps(), cfg(), |h| {
        h.keys("down down down down down down down down down down down down end");
        h.type_text(" See [[Ro");
    });
    shot("tables-code-images", &out, kitty_caps(), cfg(), |h| {
        for _ in 0..3 {
            h.app.handle(Event::Mouse(Mouse { kind: MouseKind::Wheel, button: 1, x: 50, y: 20, mods: 0 }));
        }
        h.keys("ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down ctrl+down");
    });
    shot("palette", &out, kitty_caps(), cfg(), |h| {
        h.keys("ctrl+p");
        h.type_text("toggle");
    });
    shot("switcher", &out, kitty_caps(), cfg(), |h| {
        h.keys("ctrl+o");
        h.type_text("road");
    });
    shot("find-replace", &out, kitty_caps(), cfg(), |h| {
        h.keys("ctrl+h");
        h.type_text("glyph");
        h.keys("tab");
        h.type_text("Glyph");
    });
    shot("reading", &out, kitty_caps(), cfg(), |h| {
        h.keys("ctrl+e");
    });
    let mut img_caps = kitty_caps();
    img_caps.text_sizing = false;
    shot("image-headings", &out, img_caps, cfg(), |h| {
        h.keys("ctrl+down ctrl+down ctrl+down ctrl+down");
    });
    shot("light-theme", &out, kitty_caps(), Config { theme: "light".into(), autosave: false, ..Config::default() }, |h| {
        h.keys("ctrl+shift+\\");
    });
    let mut plain = kitty_caps();
    plain.graphics = false;
    plain.placeholders = false;
    plain.text_sizing = false;
    plain.palette = Default::default();
    shot("plain-terminal", &out, plain, cfg(), |_| {});
    shot("help", &out, kitty_caps(), cfg(), |h| {
        h.keys("f1");
    });
}
