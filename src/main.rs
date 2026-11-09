use std::path::{Path, PathBuf};
use std::time::Duration;

use glyph::app::{App, Config};
use glyph::input::Event;
use glyph::layout::Mode;
use glyph::term::{self, Terminal};
use glyph::vault;

const HELP: &str = "glyph — a WYSIWYG Markdown editor for the terminal (kitty graphics + text sizing)

USAGE:
    glyph [OPTIONS] [FILE|FOLDER]...

    With a folder, it becomes the vault and the quick switcher opens.
    With files, the vault is the nearest ancestor with .obsidian/ or .git/.

OPTIONS:
    --kitty             open in a new kitty window tuned for writing (line
                        spacing, padding, a prose-friendly font if installed)
    --theme NAME        terminal (default), dark, light, nord, gruvbox
    --typography MODE   proportional | terminal | auto (default: proportional
                        Inter text via kitty graphics when supported)
    --headings MODE     osc66 | image | plain   (default: detect)
    --no-images         don't render images
    --no-remote-images  don't fetch http(s) images
    --no-autosave       only save with Ctrl+S
    --source            open in source mode
    --read              open in reading view
    --line-numbers      show line numbers
    --width N           readable line length (0 = full width; default 100)
    -h, --help          show this help
    -V, --version       show version

KEYS: Ctrl+P command palette · Ctrl+O open note · Ctrl+S save · Ctrl+Q quit · F1 help

CONFIG: ~/.config/glyph/config with `key = value` lines
        (theme, autosave, readable_width, headings, images, remote_images, line_numbers,
         typography, prose_size, line_height, inline_title,
         kitty_font, kitty_cell_height, kitty_padding)
";

fn main() {
    let mut config = Config::load();
    let mut paths: Vec<PathBuf> = vec![];
    let mut mode = Mode::Live;
    let mut kitty = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("glyph {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--theme" => config.theme = args.next().unwrap_or_default(),
            "--headings" => {
                let v = args.next().unwrap_or_default();
                config.set("headings", &v);
            }
            "--kitty" => kitty = true,
            "--typography" => config.typography = args.next().unwrap_or_else(|| "auto".into()),
            "--terminal-text" => config.typography = "terminal".into(),
            "--no-images" => config.images = false,
            "--no-remote-images" => config.remote_images = false,
            "--no-autosave" => config.autosave = false,
            "--source" => mode = Mode::Source,
            "--read" => mode = Mode::Read,
            "--line-numbers" => config.line_numbers = true,
            "--width" => config.readable_width = args.next().and_then(|v| v.parse().ok()).unwrap_or(100),
            s if s.starts_with("--") => {
                eprintln!("glyph: unknown option {s}\n\n{HELP}");
                std::process::exit(2);
            }
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if kitty {
        // re-run ourselves inside a kitty window tuned for writing
        let err = launch_kitty(&config);
        eprintln!("glyph: could not start kitty: {err}");
        std::process::exit(1);
    }
    if let Err(e) = run(paths, config, mode) {
        eprintln!("glyph: {e}");
        std::process::exit(1);
    }
}

/// Monospace fonts that read well for prose, best first (used for the cells
/// that stay text: code, sidebars, inputs).
const WRITING_FONTS: &[&str] = &["iA Writer Duo S", "iA Writer Duo V", "iA Writer Mono S", "iA Writer Mono V", "IBM Plex Mono", "JetBrains Mono", "Iosevka"];

fn installed_font(families: &[&str]) -> Option<String> {
    let out = std::process::Command::new("fc-list").args([":", "family"]).output().ok()?;
    let list = String::from_utf8_lossy(&out.stdout).to_lowercase();
    families.iter().find(|f| list.lines().any(|l| l.split(',').any(|x| x.trim() == f.to_lowercase()))).map(|s| s.to_string())
}

/// The kitty arguments for a writing-tuned window running `glyph rest...`.
pub fn kitty_args(config: &Config, exe: &str, rest: &[String], font: Option<String>) -> Vec<String> {
    let mut a = vec!["--title".to_string(), "glyph".into(), "--class".into(), "glyph".into()];
    let mut opt = |k: &str, v: String| {
        a.push("-o".into());
        a.push(format!("{k}={v}"));
    };
    opt("window_padding_width", config.kitty_padding.to_string());
    opt("modify_font", format!("cell_height {}", config.kitty_cell_height));
    if let Some(f) = font {
        opt("font_family", f);
    }
    a.push(exe.to_string());
    a.extend(rest.iter().filter(|r| *r != "--kitty").cloned());
    a
}

fn launch_kitty(config: &Config) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "glyph".into());
    let rest: Vec<String> = std::env::args().skip(1).collect();
    let font = match config.kitty_font.as_str() {
        "auto" => installed_font(WRITING_FONTS),
        "" | "none" | "default" => None,
        f => Some(f.to_string()),
    };
    std::process::Command::new("kitty").args(kitty_args(config, &exe, &rest, font)).exec()
}

fn run(paths: Vec<PathBuf>, config: Config, mode: Mode) -> std::io::Result<()> {
    let first = paths.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    let root = vault::find_root(&first);
    let mut term = Terminal::open()?;
    term::install_panic_hook();
    term.enter();
    let (caps, pending) = if std::env::var_os("GLYPH_NO_PROBE").is_some() { (Default::default(), vec![]) } else { term.probe(Duration::from_millis(600)) };
    let mut caps = caps;
    if caps.cell_w == 0 {
        caps.cell_w = 10;
        caps.cell_h = 20;
    }
    if caps.pixel_mouse {
        term.enable_pixel_mouse();
    }
    let (w, h, _, _) = term.size();
    let mut app = App::new(caps, w, h, &root, config, term.waker());
    let mut opened = false;
    // one note at a time (no tabs): open the first file given
    if let Some(p) = paths.iter().find(|p| !p.is_dir()) {
        app.open(p, false);
        opened = true;
    }
    if !opened {
        // a folder (or nothing): open README/index if present, then the switcher
        let cand = ["README.md", "Readme.md", "readme.md", "index.md", "Home.md"].iter().map(|n| root.join(n)).find(|p| p.is_file());
        match cand {
            Some(p) => app.open(&p, false),
            None => app.open_text(""),
        }
        if app.tabs[0].ed.path.is_none() {
            app.run(glyph::commands::Cmd::QuickSwitch);
        }
    }
    for t in &mut app.tabs {
        t.view.mode = mode;
    }
    for e in pending {
        app.handle(e);
    }
    loop {
        let mut out = app.render();
        if let Some(i) = out.find("\x00suspend") {
            out.replace_range(i..i + 8, "");
            term.write(&out);
            term.suspend();
            app.screen.invalidate();
            app.screen.forget_images();
            continue;
        }
        term.write(&out);
        if app.quit {
            break;
        }
        let timeout = app.tick();
        let events = term.read(timeout);
        if term.take_resized() {
            let (w, h, xp, yp) = term.size();
            app.w = w;
            app.h = h;
            if xp > 0 && yp > 0 {
                app.images.set_cell_size(xp / w as u32, yp / h as u32);
            }
            app.screen.invalidate();
            app.layout.clear();
        }
        for e in events {
            if let Event::Key(k) = &e
                && k.key == "hangup"
            {
                for t in &mut app.tabs {
                    if t.ed.dirty() && t.ed.path.is_some() {
                        let _ = t.ed.save();
                    }
                }
                app.quit = true;
                continue;
            }
            app.handle(e);
        }
    }
    term.leave();
    let _ = Path::new("");
    Ok(())
}
