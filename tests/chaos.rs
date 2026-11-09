//! Randomised key/mouse/resize sequences must never panic or corrupt state.

use glyph::app::Config;
use glyph::input::{Event, Mouse, MouseKind};
use glyph::testing::{Harness, key, kitty_caps};

const DOC: &str = "---\ntitle: x\n---\n# Head\n\nSome **bold** and *it* [[Other]] $x^2$ #tag\n- [ ] task\n    - nested 日本語\n1. one\n\n> [!tip] T\n> body\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn x() {}\n```\n\n$$\ny\n$$\n![img](missing.png)\nend";

const KEYS: &[&str] = &[
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "pageup",
    "pagedown",
    "ctrl+home",
    "ctrl+end",
    "shift+left",
    "shift+right",
    "shift+up",
    "shift+down",
    "ctrl+left",
    "ctrl+right",
    "enter",
    "shift+enter",
    "backspace",
    "delete",
    "tab",
    "shift+tab",
    "ctrl+backspace",
    "ctrl+z",
    "ctrl+y",
    "ctrl+b",
    "ctrl+i",
    "ctrl+l",
    "ctrl+k",
    "ctrl+d",
    "ctrl+shift+k",
    "ctrl+j",
    "alt+up",
    "alt+down",
    "ctrl+a",
    "ctrl+c",
    "ctrl+x",
    "ctrl+v",
    "ctrl+e",
    "alt+e",
    "ctrl+1",
    "ctrl+2",
    "ctrl+0",
    "escape",
    "a",
    "Z",
    "*",
    "[",
    "]",
    "(",
    "`",
    "$",
    "#",
    "|",
    "-",
    ">",
    "space",
    "日",
    "ctrl+f",
    "ctrl+h",
    "ctrl+p",
    "ctrl+o",
    "f1",
    "ctrl+\\",
    "ctrl+shift+\\",
    "alt+enter",
    "alt+left",
    "alt+right",
    "ctrl+g",
    "alt+t",
    "alt+c",
    "alt+q",
    "alt+-",
    "alt+n",
    "f3",
    "ctrl+up",
    "ctrl+down",
    "ctrl+tab",
    "alt+1",
    "alt+2",
];

fn rng(seed: &mut u64, m: usize) -> usize {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) as usize) % m.max(1)
}

fn run(seed0: u64, steps: usize, w: usize, h: usize) {
    let mut seed = seed0;
    let mut hs = Harness::with_caps(w, h, &[("a.md", DOC), ("Other.md", "# Other\nback [[a]]")], kitty_caps(), Config { autosave: false, ..Config::default() });
    for step in 0..steps {
        let r = rng(&mut seed, 100);
        let ev = if r < 80 {
            let k = KEYS[rng(&mut seed, KEYS.len())];
            // keep away from quitting / closing the last tab
            Event::Key(key(k))
        } else if r < 94 {
            let kinds = [MouseKind::Press, MouseKind::Release, MouseKind::Drag, MouseKind::Wheel];
            Event::Mouse(Mouse {
                kind: kinds[rng(&mut seed, 4)],
                button: rng(&mut seed, 3) as u8,
                x: rng(&mut seed, w) as u16,
                y: rng(&mut seed, h) as u16,
                mods: [0, 4, 1][rng(&mut seed, 3)],
            })
        } else if r < 97 {
            Event::Paste(["x", "line\nline2", "- a\n- b", "| c |", "![[pic.png]]", ""][rng(&mut seed, 6)].into())
        } else {
            let (nw, nh) = (5 + rng(&mut seed, 120), 2 + rng(&mut seed, 50));
            hs.app.w = nw;
            hs.app.h = nh;
            hs.vt = glyph::vt::Vt::new(nw, nh);
            hs.app.screen.invalidate();
            Event::Resize
        };
        let desc = format!("{ev:?}");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hs.app.handle(ev);
            if hs.app.quit {
                hs.app.quit = false;
                hs.app.overlay = None;
            }
            hs.app.tick();
            hs.frame();
        }));
        if res.is_err() {
            panic!("seed {seed0} step {step}: panic after {desc}\ntext:\n{}", hs.app.tabs.get(hs.app.cur).map(|t| t.ed.buf.text()).unwrap_or_default());
        }
        if hs.app.tabs.is_empty() {
            hs.app.open_text("");
        }
        let ed = &hs.app.tabs[hs.app.cur].ed;
        let c = ed.cursor;
        assert!(c.line < ed.buf.len(), "seed {seed0} step {step}: cursor line out of range after {desc}");
        assert!(ed.buf.lines[c.line].is_char_boundary(c.col) && c.col <= ed.buf.lines[c.line].len(), "seed {seed0} step {step}: bad cursor col after {desc}");
        if let Some(a) = ed.anchor {
            assert!(
                a.line < ed.buf.len() && a.col <= ed.buf.lines[a.line].len() && ed.buf.lines[a.line].is_char_boundary(a.col),
                "seed {seed0} step {step}: bad anchor after {desc}"
            );
        }
    }
}

#[test]
fn chaos_default_size() {
    let n: u64 = std::env::var("GLYPH_CHAOS_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let off: u64 = std::env::var("GLYPH_CHAOS_OFFSET").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let steps: usize = std::env::var("GLYPH_CHAOS_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    for seed in off..off + n {
        run(seed, steps, 100, 30);
    }
}

#[test]
fn chaos_tiny_terminal() {
    for seed in 100..106 {
        run(seed, 300, 12, 4);
    }
    run(200, 200, 1, 1);
    run(201, 200, 3, 2);
}
