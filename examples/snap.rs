//! Render screenshots of glyph through the virtual terminal:
//!   cargo run --example snap -- <out-dir> [vault-dir] [file]
use glyph::app::Config;
use glyph::testing::{Harness, kitty_caps};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--gen-image") {
        gen_image(&args[1]);
        return;
    }
    let out = PathBuf::from(args.first().cloned().unwrap_or("snapshots/out".into()));
    std::fs::create_dir_all(&out).unwrap();
    let vault = PathBuf::from(args.get(1).cloned().unwrap_or("examples/demo".into()));
    let file = args.get(2).cloned().unwrap_or("Welcome.md".into());
    let (w, h) = (110, 46);
    let script: Vec<(&str, &str)> = vec![
        ("01-live", ""),
        ("02-cursor-in-heading", "down"),
        ("03-sidebars", "ctrl+\\ ctrl+shift+\\"),
        ("04-palette", "ctrl+p"),
        ("05-switcher", "escape ctrl+o"),
        ("06-scrolled", "escape pagedown pagedown"),
        ("07-reading", "ctrl+e"),
        ("08-source", "ctrl+e alt+e"),
    ];
    let mut config = Config { autosave: false, ..Config::default() };
    if let Ok(t) = std::env::var("SNAP_THEME") {
        config.theme = t;
    }
    let mut caps = kitty_caps();
    if std::env::var_os("SNAP_PLAIN").is_some() {
        caps.text_sizing = false;
    }
    let mut hs = Harness::open_dir(w, h, &vault, Some(&file), caps, config);
    for (name, keys) in script {
        hs.keys(keys);
        let p = out.join(format!("{name}.png"));
        hs.screenshot(&p).unwrap();
        println!("{} ({} bytes of output)", p.display(), hs.out_bytes);
    }
}

#[allow(dead_code)]
pub fn gen_image(path: &str) {
    let (w, h) = (840u32, 300u32);
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let t = x as f32 / w as f32;
        let s = y as f32 / h as f32;
        let wave = ((x as f32 / 60.0).sin() * 0.5 + 0.5) * (1.0 - s);
        image::Rgba([(40.0 + 120.0 * t + 60.0 * wave) as u8, (30.0 + 180.0 * wave) as u8, (120.0 + 100.0 * (1.0 - t)) as u8, 255])
    });
    img.save(path).unwrap();
}
