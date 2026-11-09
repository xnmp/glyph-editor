//! Application state and event handling. Drawing lives in `ui.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::buffer::Pos;
use crate::commands::{self, Cmd};
use crate::editor::{Editor, LinkTarget};
use crate::fuzzy;
use crate::images::ImageStore;
use crate::input::{ALT, CTRL, Event, Key, Mouse, MouseKind, Reply, SHIFT};
use crate::layout::{Ctx, HeadingMode, Layout, Mode};
use crate::markdown::{self as md, LineKind};
use crate::screen::Screen;
use crate::term::Caps;
use crate::theme::Theme;
use crate::vault::{self, Vault};
use crate::view::{Env, Rect, View};

pub struct Tab {
    pub ed: Editor,
    pub view: View,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Files,
    Panel,
}

#[derive(Clone, Debug, Default)]
pub struct LineInput {
    pub text: String,
    pub cur: usize,
}

impl LineInput {
    pub fn new(t: &str) -> LineInput {
        LineInput { text: t.to_string(), cur: t.len() }
    }
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.text.insert_str(self.cur, &s);
        self.cur += s.len();
    }
    /// Returns true if the key was an editing key.
    pub fn handle(&mut self, k: &Key) -> bool {
        let n = k.name();
        match n.as_str() {
            "left" => self.cur = crate::text::prev_boundary(&self.text, self.cur),
            "right" => self.cur = crate::text::next_boundary(&self.text, self.cur),
            "home" | "ctrl+a" => self.cur = 0,
            "end" | "ctrl+e" => self.cur = self.text.len(),
            "ctrl+left" | "alt+b" => self.cur = crate::text::prev_word(&self.text, self.cur),
            "ctrl+right" | "alt+f" => self.cur = crate::text::next_word(&self.text, self.cur),
            "backspace" => {
                if self.cur > 0 {
                    let p = crate::text::prev_boundary(&self.text, self.cur);
                    self.text.replace_range(p..self.cur, "");
                    self.cur = p;
                }
            }
            "delete" => {
                if self.cur < self.text.len() {
                    let nb = crate::text::next_boundary(&self.text, self.cur);
                    self.text.replace_range(self.cur..nb, "");
                }
            }
            "ctrl+backspace" | "ctrl+w" | "alt+backspace" => {
                let p = crate::text::prev_word(&self.text, self.cur);
                self.text.replace_range(p..self.cur, "");
                self.cur = p;
            }
            "ctrl+u" => {
                self.text.replace_range(..self.cur, "");
                self.cur = 0;
            }
            "ctrl+k" => self.text.truncate(self.cur),
            _ => {
                if !k.text.is_empty() && k.mods & (CTRL | ALT) == 0 {
                    let t = k.text.clone();
                    self.insert(&t);
                } else {
                    return false;
                }
            }
        }
        true
    }
}

pub enum PromptKind {
    GoToLine,
    Rename,
    NewNote,
}

pub enum ConfirmKind {
    Quit,
    CloseTab(usize),
}

pub enum Overlay {
    Palette { input: LineInput, sel: usize },
    Switcher { input: LineInput, sel: usize },
    Search { input: LineInput, sel: usize, results: Vec<(PathBuf, usize, usize, String)>, dirty: bool },
    Prompt { title: String, input: LineInput, kind: PromptKind },
    Confirm { msg: String, kind: ConfirmKind },
    Help { scroll: usize },
}

pub struct FindBar {
    pub query: LineInput,
    pub replace: LineInput,
    pub replacing: bool,
    pub field: u8,
    pub case: bool,
}

pub struct Completion {
    /// where the query starts (just after "[[")
    pub start: Pos,
    pub query: String,
    pub items: Vec<String>,
    pub sel: usize,
}

pub struct Config {
    pub theme: String,
    pub autosave: bool,
    pub readable_width: usize,
    pub heading_mode: Option<HeadingMode>,
    pub images: bool,
    pub remote_images: bool,
    pub line_numbers: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { theme: "terminal".into(), autosave: true, readable_width: 100, heading_mode: None, images: true, remote_images: true, line_numbers: false }
    }
}

impl Config {
    /// `~/.config/glyph/config` — simple `key = value` lines.
    pub fn load() -> Config {
        let mut c = Config::default();
        let dir = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        if let Some(d) = dir
            && let Ok(t) = std::fs::read_to_string(d.join("glyph").join("config"))
        {
            for line in t.lines() {
                let line = line.trim();
                if line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    c.set(k.trim(), v.trim().trim_matches('"'));
                }
            }
        }
        c
    }
    pub fn set(&mut self, k: &str, v: &str) {
        let b = matches!(v, "true" | "yes" | "on" | "1");
        match k {
            "theme" => self.theme = v.into(),
            "autosave" => self.autosave = b,
            "readable_width" | "readable-width" => self.readable_width = v.parse().unwrap_or(100),
            "headings" | "heading_mode" => {
                self.heading_mode = match v {
                    "plain" => Some(HeadingMode::Plain),
                    "osc66" | "scaled" => Some(HeadingMode::Osc66),
                    "image" => Some(HeadingMode::Image),
                    _ => None,
                }
            }
            "images" => self.images = b,
            "remote_images" | "remote-images" => self.remote_images = b,
            "line_numbers" | "line-numbers" => self.line_numbers = b,
            _ => {}
        }
    }
}

pub const THEMES: &[&str] = &["terminal", "dark", "light", "nord", "gruvbox"];

pub struct App {
    pub w: usize,
    pub h: usize,
    pub caps: Caps,
    pub theme: Theme,
    pub tabs: Vec<Tab>,
    pub cur: usize,
    pub vault: Vault,
    pub images: ImageStore,
    pub layout: Layout,
    pub screen: Screen,
    pub heading_mode: HeadingMode,
    pub show_left: bool,
    pub show_right: bool,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub find: Option<FindBar>,
    pub completion: Option<Completion>,
    pub status: Option<(String, Instant, bool)>,
    pub clipboard: String,
    pub extra_out: String,
    pub quit: bool,
    pub config: Config,
    clicks: (Instant, usize, usize, u8),
    dragging: bool,
    drag_y: Option<isize>,
    history: Vec<(PathBuf, Pos)>,
    hist_idx: usize,
    last_edit: Option<Instant>,
    last_version: u64,
    pub tree_open: HashSet<PathBuf>,
    pub tree_sel: usize,
    pub tree_scroll: usize,
    pub right_tab: usize,
    pub right_sel: usize,
    pub right_scroll: usize,
    pub backlinks: Option<(PathBuf, Instant, Vec<(PathBuf, usize, String)>)>,
    pub editor_rect: Rect,
    pub left_rect: Rect,
    pub right_rect: Rect,
    pub tab_hits: Vec<(usize, usize, usize, bool)>,
    pub overlay_hits: Vec<(Rect, usize)>,
    pub recent: Vec<PathBuf>,
    last_disk_check: Instant,
    pub theme_idx: usize,
    /// bumped when notes change on disk (invalidates rendered embeds)
    pub embed_gen: u64,
    pub scroll_seen: ((usize, usize, usize), Option<Instant>),
    pub open_url: Box<dyn Fn(&str)>,
    pub system_paste: Box<dyn Fn() -> Option<String>>,
}

impl App {
    pub fn new(caps: Caps, w: usize, h: usize, root: &Path, config: Config, waker: std::sync::Arc<dyn Fn() + Send + Sync>) -> App {
        let theme = match Theme::named(&config.theme) {
            Some(t) => t,
            None => Theme::terminal(&caps.palette),
        };
        let theme_idx = THEMES.iter().position(|t| *t == config.theme).unwrap_or(0);
        let vault = Vault::open(root);
        let mut images = ImageStore::new(caps.cell_w, caps.cell_h, waker);
        images.vault_root = vault.root.clone();
        images.by_name = vault.by_name.clone();
        images.allow_remote = config.remote_images;
        let heading_mode = config.heading_mode.unwrap_or(if caps.text_sizing {
            HeadingMode::Osc66
        } else if caps.graphics && images.has_font() {
            HeadingMode::Image
        } else {
            HeadingMode::Plain
        });
        let placeholders = caps.placeholders;
        let mut tree_open = HashSet::new();
        tree_open.insert(PathBuf::new());
        App {
            w,
            h,
            caps,
            theme,
            tabs: vec![],
            cur: 0,
            vault,
            images,
            layout: Layout::new(),
            screen: Screen::new(placeholders),
            heading_mode,
            show_left: false,
            show_right: false,
            focus: Focus::Editor,
            overlay: None,
            find: None,
            completion: None,
            status: None,
            clipboard: String::new(),
            extra_out: String::new(),
            quit: false,
            config,
            clicks: (Instant::now() - Duration::from_secs(5), 0, 0, 0),
            dragging: false,
            drag_y: None,
            history: vec![],
            hist_idx: 0,
            last_edit: None,
            last_version: 0,
            tree_open,
            tree_sel: 0,
            tree_scroll: 0,
            right_tab: 0,
            right_sel: 0,
            right_scroll: 0,
            backlinks: None,
            editor_rect: Rect::default(),
            left_rect: Rect::default(),
            right_rect: Rect::default(),
            tab_hits: vec![],
            overlay_hits: vec![],
            recent: vec![],
            last_disk_check: Instant::now(),
            theme_idx,
            embed_gen: 0,
            scroll_seen: ((0, 0, 0), None),
            open_url: Box::new(open_external),
            system_paste: Box::new(system_paste),
        }
    }
}

/// Relative path from `base` to `p` (both absolute).
pub fn pathdiff(p: &Path, base: &Path) -> Option<PathBuf> {
    let pc: Vec<_> = p.components().collect();
    let bc: Vec<_> = base.components().collect();
    let common = pc.iter().zip(&bc).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..bc.len() {
        out.push("..");
    }
    for c in &pc[common..] {
        out.push(c);
    }
    Some(out)
}

fn open_external(url: &str) {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn system_paste() -> Option<String> {
    let cands: &[(&str, &[&str])] = &[("wl-paste", &["-n"]), ("xclip", &["-o", "-selection", "clipboard"]), ("xsel", &["-ob"]), ("pbpaste", &[])];
    for (c, args) in cands {
        if let Ok(out) = std::process::Command::new(c).args(*args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output()
            && out.status.success()
        {
            return Some(String::from_utf8_lossy(&out.stdout).to_string());
        }
    }
    None
}

impl App {
    // ------------------------------------------------------------------ helpers
    pub fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.cur]
    }

    pub fn ed(&mut self) -> &mut Editor {
        &mut self.tabs[self.cur].ed
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), false));
    }
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now(), true));
    }

    pub fn ctx_epoch(&self) -> u64 {
        self.images.generation()
    }

    /// Run `f` with a layout environment and the current tab.
    pub fn with_env<R>(&mut self, f: impl FnOnce(&Env, &mut Tab) -> R) -> R {
        let App { theme, images, vault, layout, tabs, cur, heading_mode, config, caps, embed_gen, .. } = self;
        let tab = &mut tabs[*cur];
        let le = |t: &str| vault.exists(t);
        let from = tab.ed.path.clone();
        let embed = |t: &str, anchor: &str| -> Option<Vec<String>> {
            let p = vault.resolve(t, from.as_deref())?;
            if !crate::vault::is_note(&p) {
                return None;
            }
            let text = std::fs::read_to_string(&p).ok()?;
            let lines: Vec<String> = text.lines().map(String::from).collect();
            if anchor.is_empty() {
                return Some(lines);
            }
            // a heading section, or a ^block reference
            if let Some(b) = anchor.strip_prefix('^') {
                return lines
                    .iter()
                    .find(|l| l.trim_end().ends_with(&format!("^{b}")))
                    .map(|l| vec![l.trim_end().trim_end_matches(&format!("^{b}")).to_string()]);
            }
            let doc = md::parse_document(&lines);
            let heads = md::outline(&lines, &doc);
            let a = md::slug(anchor);
            let k = heads.iter().position(|(_, _, h)| md::slug(h) == a)?;
            let (start, lvl, _) = heads[k];
            let end = heads[k + 1..].iter().find(|(_, l, _)| *l <= lvl).map_or(lines.len(), |h| h.0);
            Some(lines[start..end].to_vec())
        };
        let epoch = images.generation() + *embed_gen * 7919;
        let env = Env {
            layout,
            ctx: Ctx {
                theme,
                width: tab.view.cw,
                mode: tab.view.mode,
                heading_mode: *heading_mode,
                images: if config.images && caps.graphics { Some(images) } else { None },
                link_exists: &le,
                embed: &embed,
                max_image_rows: (tab.view.rect.h.saturating_sub(2) as u32).clamp(4, 40),
                epoch,
            },
        };
        f(&env, tab)
    }

    pub fn layout_panes(&mut self) {
        let (w, h) = (self.w, self.h);
        let top = 1usize;
        let find_h = match &self.find {
            Some(f) if f.replacing => 2,
            Some(_) => 1,
            None => 0,
        };
        let bottom = h.saturating_sub(1 + find_h);
        let body_h = bottom.saturating_sub(top);
        let lw = if self.show_left && w >= 50 { 30.min(w / 3) } else { 0 };
        let rw = if self.show_right && w >= 70 { 32.min(w / 3) } else { 0 };
        self.left_rect = Rect { x: 0, y: top, w: lw, h: body_h };
        self.right_rect = Rect { x: w - rw, y: top, w: rw, h: body_h };
        let ex = if lw > 0 { lw + 1 } else { 0 };
        let ew = w.saturating_sub(ex + if rw > 0 { rw + 1 } else { 0 });
        self.editor_rect = Rect { x: ex, y: top, w: ew, h: body_h };
        let rect = self.editor_rect;
        let ln = self.config.line_numbers;
        let rw_ = if self.config.readable_width == 0 { usize::MAX } else { self.config.readable_width };
        for t in &mut self.tabs {
            t.view.line_numbers = ln || t.view.mode == Mode::Source && ln;
            t.view.readable = rw_;
            t.view.layout_origin(rect);
        }
    }

    fn after_move(&mut self) {
        // safety net: every command leaves a valid cursor/anchor
        {
            let ed = &mut self.tabs[self.cur].ed;
            ed.cursor = ed.buf.clamp(ed.cursor);
            ed.anchor = ed.anchor.map(|a| ed.buf.clamp(a));
        }
        self.layout_panes();
        self.with_env(|env, tab| tab.view.ensure_cursor_visible(env, &mut tab.ed));
    }

    fn after_edit(&mut self) {
        {
            let ed = &mut self.tabs[self.cur].ed;
            ed.cursor = ed.buf.clamp(ed.cursor);
            ed.anchor = ed.anchor.map(|a| ed.buf.clamp(a));
        }
        self.last_edit = Some(Instant::now());
        self.update_completion();
        self.after_move();
    }

    // ------------------------------------------------------------------ files & tabs
    pub fn open(&mut self, path: &Path, new_tab: bool) {
        let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
        let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
        if let Some(i) = self.tabs.iter().position(|t| t.ed.path.as_deref() == Some(abs.as_path())) {
            self.cur = i;
            self.after_open();
            return;
        }
        if !vault::is_note(&abs) && abs.exists() {
            // non-markdown files (images etc.) open externally
            (self.open_url)(&abs.to_string_lossy());
            return;
        }
        let ed = match Editor::open(&abs) {
            Ok(e) => e,
            Err(e) => {
                self.set_error(format!("Cannot open {}: {e}", abs.display()));
                return;
            }
        };
        let tab = Tab { ed, view: View::new(Mode::Live) };
        let replace = !new_tab && !self.tabs.is_empty() && {
            let t = &self.tabs[self.cur];
            !t.ed.dirty() || self.config.autosave
        };
        if replace {
            if self.tabs[self.cur].ed.dirty() {
                let _ = self.tabs[self.cur].ed.save();
            }
            let mode = self.tabs[self.cur].view.mode;
            self.tabs[self.cur] = tab;
            self.tabs[self.cur].view.mode = if mode == Mode::Source { Mode::Source } else { Mode::Live };
        } else {
            self.tabs.push(tab);
            self.cur = self.tabs.len() - 1;
        }
        self.after_open();
    }

    fn after_open(&mut self) {
        // fresh documents start below the frontmatter, like Obsidian's properties
        {
            let ed = &mut self.tabs[self.cur].ed;
            if ed.cursor == Pos::new(0, 0) && ed.buf.version == 0 {
                let doc = ed.doc().clone();
                if let Some(b) = doc.blocks.first().filter(|b| b.kind == md::BlockKind::Front) {
                    let l = b.end.min(ed.buf.len() - 1);
                    let l = (l..ed.buf.len()).find(|&i| !ed.line(i).trim().is_empty()).unwrap_or(l);
                    ed.cursor = Pos::new(l, 0);
                }
            }
        }
        if let Some(p) = self.tabs[self.cur].ed.path.clone() {
            self.recent.retain(|r| *r != p);
            self.recent.insert(0, p.clone());
            self.recent.truncate(50);
            if let Some(dir) = p.parent() {
                self.images.base_dir = dir.to_path_buf();
                // expand the file tree to the file
                let mut rel = p.strip_prefix(&self.vault.root).map(Path::to_path_buf).unwrap_or_default();
                while rel.pop() {
                    self.tree_open.insert(rel.clone());
                }
            }
        }
        self.completion = None;
        self.backlinks = None;
        self.last_version = self.tabs[self.cur].ed.buf.version;
        self.layout_panes();
        self.after_move();
    }

    pub fn open_text(&mut self, text: &str) {
        self.tabs.push(Tab { ed: Editor::new(text), view: View::new(Mode::Live) });
        self.cur = self.tabs.len() - 1;
        self.layout_panes();
    }

    fn push_history(&mut self) {
        if let Some(p) = self.tabs[self.cur].ed.path.clone() {
            let pos = self.tabs[self.cur].ed.cursor;
            self.history.truncate(self.hist_idx);
            self.history.push((p, pos));
            self.hist_idx = self.history.len();
        }
    }

    fn close_tab(&mut self, i: usize, force: bool) {
        if i >= self.tabs.len() {
            return;
        }
        if self.tabs[i].ed.dirty() && !force {
            if self.config.autosave && self.tabs[i].ed.path.is_some() {
                let _ = self.tabs[i].ed.save();
            } else {
                let name = self.tab_name(i);
                self.overlay =
                    Some(Overlay::Confirm { msg: format!("\"{name}\" has unsaved changes. Save before closing? (y/n/esc)"), kind: ConfirmKind::CloseTab(i) });
                return;
            }
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.quit = true;
            return;
        }
        if self.cur >= self.tabs.len() {
            self.cur = self.tabs.len() - 1;
        } else if self.cur > i {
            self.cur -= 1;
        }
        self.after_open();
    }

    pub fn tab_name(&self, i: usize) -> String {
        match &self.tabs[i].ed.path {
            Some(p) => p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            None => "untitled".into(),
        }
    }

    pub fn save(&mut self) {
        let r = self.ed().save();
        match r {
            Ok(()) => {
                self.set_status("Saved");
                self.vault_changed();
            }
            Err(e) => self.set_error(format!("Save failed: {e}")),
        }
    }

    fn vault_changed(&mut self) {
        self.embed_gen += 1;
        let before = self.vault.notes.len();
        self.vault.rescan();
        self.images.by_name = self.vault.by_name.clone();
        self.backlinks = None;
        if self.vault.notes.len() != before {
            self.layout.clear();
        }
    }

    pub fn new_note(&mut self, name: &str) {
        let from = self.tabs.get(self.cur).and_then(|t| t.ed.path.clone());
        let mut path = self.vault.new_note_path(name, from.as_deref());
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = self.vault.new_note_path(&format!("{name} {n}"), from.as_deref());
        }
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Err(e) = std::fs::write(&path, "") {
            self.set_error(format!("Cannot create note: {e}"));
            return;
        }
        self.vault_changed();
        self.push_history();
        self.open(&path, false);
        self.set_status(format!("Created {}", self.vault.rel(&path)));
    }

    // ------------------------------------------------------------------ links
    pub fn follow(&mut self, target: LinkTarget, new_tab: bool) {
        match target {
            LinkTarget::Url(u) => {
                (self.open_url)(&u);
                self.set_status(format!("Opening {u}"));
            }
            LinkTarget::Tag(t) => {
                let q = format!("#{t}");
                let results = self.vault.search(&q, 500);
                self.overlay = Some(Overlay::Search { input: LineInput::new(&q), sel: 0, results, dirty: false });
            }
            LinkTarget::Footnote(id) => {
                let needle = format!("[^{id}]:");
                if let Some(i) = self.ed().buf.lines.iter().position(|l| l.starts_with(&needle)) {
                    self.push_history();
                    let col = needle.len();
                    self.ed().set_cursor(Pos::new(i, col), false);
                    self.after_move();
                }
            }
            LinkTarget::Note { target, anchor } => {
                let from = self.tabs[self.cur].ed.path.clone();
                let path = if target.is_empty() { from.clone() } else { self.vault.resolve(&target, from.as_deref()) };
                self.push_history();
                match path {
                    Some(p) => {
                        if Some(&p) != from.as_ref() || new_tab {
                            self.open(&p, new_tab);
                        }
                    }
                    None => {
                        if target.contains("://") {
                            return;
                        }
                        self.new_note(&target);
                    }
                }
                if !anchor.is_empty() {
                    self.jump_to_anchor(&anchor);
                }
            }
        }
    }

    fn jump_to_anchor(&mut self, anchor: &str) {
        let ed = self.ed();
        let lines = ed.buf.lines.clone();
        let target = if let Some(b) = anchor.strip_prefix('^') {
            lines.iter().position(|l| l.trim_end().ends_with(&format!("^{b}")))
        } else {
            let doc = ed.doc().clone();
            let a = md::slug(anchor);
            md::outline(&lines, &doc).into_iter().find(|(_, _, t)| md::slug(t) == a || t.eq_ignore_ascii_case(anchor)).map(|x| x.0)
        };
        if let Some(i) = target {
            self.ed().set_cursor(Pos::new(i, lines[i].len()), false);
            self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed));
        }
    }

    fn nav(&mut self, back: bool) {
        if back {
            if self.hist_idx == 0 || self.history.is_empty() {
                return;
            }
            if self.hist_idx >= self.history.len() {
                // remember where we are so Forward can return here
                let before = self.history.len();
                self.push_history();
                if self.history.len() > before {
                    self.hist_idx = self.history.len() - 1;
                } else {
                    self.hist_idx = self.history.len();
                }
            }
            if self.hist_idx == 0 {
                return;
            }
            self.hist_idx -= 1;
        } else {
            if self.hist_idx + 1 >= self.history.len() {
                return;
            }
            self.hist_idx += 1;
        }
        let (p, pos) = self.history[self.hist_idx].clone();
        self.open(&p, false);
        let pos = self.ed().buf.clamp(pos);
        self.ed().set_cursor(pos, false);
        self.after_move();
    }

    // ------------------------------------------------------------------ completion
    fn update_completion(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let ed = &self.tabs[self.cur].ed;
        let p = ed.cursor;
        let line = ed.line(p.line);
        let before = &line[..p.col];
        let Some(open) = before.rfind("[[") else {
            self.completion = None;
            return;
        };
        let q = &before[open + 2..];
        if q.contains("]]") || q.contains('|') || q.contains('#') || q.len() > 80 {
            self.completion = None;
            return;
        }
        let q = q.to_string();
        let cands: Vec<String> = self.vault.notes.iter().map(|p| p.with_extension("").to_string_lossy().to_string()).collect();
        let ranked = fuzzy::rank(&q, &cands);
        let items: Vec<String> = ranked.into_iter().take(8).map(|(i, _, _)| cands[i].clone()).collect();
        let start = Pos::new(p.line, open + 2);
        let sel = match &self.completion {
            Some(c) if c.start == start => c.sel.min(items.len().saturating_sub(1)),
            _ => 0,
        };
        self.completion = Some(Completion { start, query: q, items, sel });
    }

    fn accept_completion(&mut self) {
        let Some(c) = self.completion.take() else { return };
        // items are vault-relative paths without extension; prefer a bare name when unique
        let Some(item) = c.items.get(c.sel).cloned() else { return };
        let name = Path::new(&item).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(item.clone());
        let unique = self.vault.notes.iter().filter(|p| p.file_stem().map(|s| s.to_string_lossy() == name).unwrap_or(false)).count() <= 1;
        let ins = if unique { name } else { item };
        let ed = self.ed();
        let p = ed.cursor;
        let line = ed.line(p.line).to_string();
        let after = &line[p.col..];
        let has_close = after.starts_with("]]");
        ed.buf.begin(ed.sel(), "");
        let new = if has_close { ins.clone() } else { format!("{ins}]]") };
        ed.buf.replace(c.start, p, &new);
        ed.anchor = None;
        ed.cursor = ed.buf.clamp(Pos::new(p.line, c.start.col + ins.len() + 2));
        let s = ed.sel();
        ed.buf.commit(s);
        self.after_edit();
        self.completion = None;
    }

    // ------------------------------------------------------------------ events
    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => self.key(&k),
            Event::Mouse(m) => self.mouse(m),
            Event::Paste(t) => self.paste_text(&t),
            Event::Resize => {}
            Event::Reply(Reply::Focus(true)) => self.check_disk(true),
            Event::Reply(_) => {}
        }
    }

    fn paste_text(&mut self, t: &str) {
        if let Some(ov) = &mut self.overlay {
            match ov {
                Overlay::Palette { input, .. } | Overlay::Switcher { input, .. } | Overlay::Prompt { input, .. } => input.insert(t),
                Overlay::Search { input, dirty, .. } => {
                    input.insert(t);
                    *dirty = true;
                }
                _ => {}
            }
            return;
        }
        if let Some(f) = &mut self.find {
            if f.field == 0 {
                f.query.insert(t)
            } else {
                f.replace.insert(t)
            }
            self.find_refresh();
            return;
        }
        if self.tabs[self.cur].view.mode == Mode::Read {
            return;
        }
        // dropped/pasted file paths (kitty drag & drop pastes paths or file:// URIs)
        let mut embeds = vec![];
        for l in t.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let p = l.strip_prefix("file://").unwrap_or(l);
            let p = crate::editor::percent_decode(p.trim_matches(['\'', '"']));
            let path = std::path::PathBuf::from(&p);
            if path.is_absolute() && path.is_file() {
                embeds.push(path);
            } else {
                embeds.clear();
                break;
            }
        }
        if !embeds.is_empty() {
            let from = self.tabs[self.cur].ed.path.clone();
            let base = from.as_ref().and_then(|f| f.parent().map(Path::to_path_buf)).unwrap_or_else(|| self.vault.root.clone());
            let mut out = vec![];
            for p in &embeds {
                let rel = pathdiff(p, &base).unwrap_or_else(|| p.clone());
                let rel = rel.to_string_lossy().replace(' ', "%20");
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let img = crate::layout::is_image_target(&name);
                if img {
                    out.push(format!("![{}]({rel})", p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                } else {
                    out.push(format!("[{name}]({rel})"));
                }
            }
            let s = out.join("\n");
            self.ed().paste(&s);
            self.after_edit();
            return;
        }
        self.ed().paste(t);
        self.after_edit();
    }

    pub fn key(&mut self, k: &Key) {
        let name = k.name();
        if self.overlay.is_some() {
            self.overlay_key(k, &name);
            return;
        }
        if self.completion.is_some() && self.focus == Focus::Editor {
            let c = self.completion.as_mut().unwrap();
            match name.as_str() {
                "down" | "ctrl+n" => {
                    c.sel = (c.sel + 1) % c.items.len().max(1);
                    return;
                }
                "up" | "ctrl+p" => {
                    c.sel = (c.sel + c.items.len().max(1) - 1) % c.items.len().max(1);
                    return;
                }
                "enter" | "tab" if !c.items.is_empty() => {
                    self.accept_completion();
                    return;
                }
                "escape" => {
                    self.completion = None;
                    return;
                }
                _ => {}
            }
        }
        if let Some(f) = &mut self.find {
            match name.as_str() {
                "escape" => {
                    self.find = None;
                    self.layout_panes();
                    return;
                }
                "enter" | "f3" => {
                    if f.field == 1 {
                        let (q, r, c) = (f.query.text.clone(), f.replace.text.clone(), f.case);
                        self.ed().replace_current(&q, &r, c);
                    } else {
                        let (q, c) = (f.query.text.clone(), f.case);
                        if !self.ed().find_next(&q, c, false) {
                            self.set_status("No matches");
                        }
                    }
                    self.after_move();
                    return;
                }
                "shift+enter" | "shift+f3" => {
                    let (q, c) = (f.query.text.clone(), f.case);
                    self.ed().find_next(&q, c, true);
                    self.after_move();
                    return;
                }
                "tab" | "shift+tab" | "down" | "up" if f.replacing => {
                    f.field = 1 - f.field;
                    return;
                }
                "alt+c" => {
                    f.case = !f.case;
                    self.find_refresh();
                    return;
                }
                "alt+enter" | "ctrl+alt+enter" if f.replacing => {
                    let (q, r, c) = (f.query.text.clone(), f.replace.text.clone(), f.case);
                    let n = self.ed().replace_all(&q, &r, c);
                    self.set_status(format!("Replaced {n} occurrence{}", if n == 1 { "" } else { "s" }));
                    self.after_edit();
                    return;
                }
                _ => {
                    let input = if f.field == 0 { &mut f.query } else { &mut f.replace };
                    if input.handle(k) {
                        if f.field == 0 {
                            self.find_refresh();
                        }
                        return;
                    }
                }
            }
        }
        if let Some(cmd) = commands::lookup(&name) {
            self.run(cmd);
            return;
        }
        match self.focus {
            Focus::Files => {
                self.files_key(&name);
                return;
            }
            Focus::Panel => {
                self.panel_key(&name);
                return;
            }
            Focus::Editor => {}
        }
        self.editor_key(k, &name);
    }

    fn find_refresh(&mut self) {
        let Some(f) = &self.find else { return };
        let (q, c) = (f.query.text.clone(), f.case);
        if q.is_empty() {
            return;
        }
        // jump to the first match at/after the selection start
        let ed = self.ed();
        let from = ed.selection().map(|s| s.0).unwrap_or(ed.cursor);
        let all = ed.find_all(&q, c);
        if let Some(&(a, b)) = all.iter().find(|m| m.0 >= from).or(all.first()) {
            ed.select_range(a, b);
        }
        self.after_move();
    }

    fn editor_key(&mut self, k: &Key, name: &str) {
        let read = self.tabs[self.cur].view.mode == Mode::Read;
        let shift = k.mods & SHIFT != 0;
        let base = name.trim_start_matches("shift+");
        let page = self.editor_rect.h.saturating_sub(2).max(1) as isize;
        if read {
            let d = match base {
                "up" | "k" => -1,
                "down" | "j" | "enter" => 1,
                "pageup" => -page,
                "pagedown" | "space" => page,
                "home" | "g" => -1_000_000,
                "end" => 1_000_000,
                "escape" => {
                    self.run(Cmd::ToggleReading);
                    return;
                }
                _ => 0,
            };
            if d != 0 {
                self.with_env(|env, tab| tab.view.scroll(env, &mut tab.ed, d));
            }
            return;
        }
        let ed_move = |app: &mut App, f: &dyn Fn(&mut Editor)| {
            f(app.ed());
            app.completion = None;
            app.after_move();
        };
        match base {
            "left" => ed_move(self, &|e| e.left(shift)),
            "right" => ed_move(self, &|e| e.right(shift)),
            "ctrl+left" | "alt+b" => ed_move(self, &|e| e.word_left(shift)),
            "ctrl+right" | "alt+f" => ed_move(self, &|e| e.word_right(shift)),
            "home" => ed_move(self, &|e| e.home(shift)),
            "end" => ed_move(self, &|e| e.end(shift)),
            "ctrl+home" => ed_move(self, &|e| e.doc_start(shift)),
            "ctrl+end" => ed_move(self, &|e| e.doc_end(shift)),
            "ctrl+shift+up" | "alt+[" => ed_move(self, &|e| e.paragraph(false, shift)),
            "ctrl+shift+down" | "alt+]" => ed_move(self, &|e| e.paragraph(true, shift)),
            "up" | "down" => {
                let d = if base == "up" { -1 } else { 1 };
                self.layout_panes();
                self.with_env(|env, tab| tab.view.move_rows(env, &mut tab.ed, d, shift));
                self.completion = None;
                self.after_move();
            }
            "pageup" | "pagedown" => {
                let d = if base == "pageup" { -page } else { page };
                self.layout_panes();
                self.with_env(|env, tab| {
                    tab.view.scroll(env, &mut tab.ed, d);
                    tab.view.move_rows(env, &mut tab.ed, d, shift);
                });
                self.after_move();
            }
            "escape" => {
                self.ed().anchor = None;
                self.completion = None;
            }
            _ => {
                let edited = match name {
                    "enter" => {
                        self.ed().newline();
                        true
                    }
                    "shift+enter" => {
                        self.ed().newline_plain();
                        true
                    }
                    "backspace" | "shift+backspace" => {
                        self.ed().backspace();
                        true
                    }
                    "delete" => {
                        self.ed().delete();
                        true
                    }
                    "ctrl+backspace" | "alt+backspace" | "ctrl+w" => {
                        self.ed().delete_word(true);
                        true
                    }
                    "ctrl+delete" | "alt+delete" | "alt+d" => {
                        self.ed().delete_word(false);
                        true
                    }
                    "ctrl+u" => {
                        self.ed().delete_to_line_start();
                        true
                    }
                    "tab" => {
                        self.ed().indent();
                        true
                    }
                    "shift+tab" => {
                        self.ed().outdent();
                        true
                    }
                    _ => {
                        if !k.text.is_empty() && k.mods & (CTRL | ALT) == 0 {
                            let t = k.text.clone();
                            let mut ch = t.chars();
                            match (ch.next(), ch.next()) {
                                (Some(c), None) => self.ed().type_char(c),
                                _ => self.ed().insert(&t),
                            }
                            true
                        } else {
                            false
                        }
                    }
                };
                if edited {
                    self.after_edit();
                }
            }
        }
    }

    // ------------------------------------------------------------------ sidebars
    pub fn tree_items(&self) -> Vec<(usize, PathBuf, bool)> {
        // (depth, relative path, is_dir)
        let mut out = vec![];
        let mut dirs_seen: HashSet<PathBuf> = HashSet::new();
        let mut entries: Vec<(PathBuf, bool)> = vec![];
        for f in &self.vault.files {
            let mut acc = PathBuf::new();
            let comps: Vec<_> = f.components().collect();
            for (k, c) in comps.iter().enumerate() {
                acc.push(c);
                if k + 1 < comps.len() {
                    if dirs_seen.insert(acc.clone()) {
                        entries.push((acc.clone(), true));
                    }
                } else {
                    entries.push((acc.clone(), false));
                }
            }
        }
        // sort: within a directory, folders first, then case-insensitive name
        entries.sort_by(|a, b| {
            let pa: Vec<String> = a.0.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
            let pb: Vec<String> = b.0.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
            let n = pa.len().min(pb.len());
            for i in 0..n {
                if pa[i] != pb[i] {
                    let a_dir = i + 1 < pa.len() || a.1;
                    let b_dir = i + 1 < pb.len() || b.1;
                    return b_dir.cmp(&a_dir).then(pa[i].cmp(&pb[i]));
                }
            }
            pa.len().cmp(&pb.len())
        });
        for (p, is_dir) in entries {
            let parent_open = {
                let mut ok = true;
                let mut q = p.clone();
                while q.pop() {
                    if !q.as_os_str().is_empty() && !self.tree_open.contains(&q) {
                        ok = false;
                        break;
                    }
                }
                ok
            };
            if parent_open {
                out.push((p.components().count() - 1, p, is_dir));
            }
        }
        out
    }

    fn tree_activate(&mut self, idx: usize, new_tab: bool) {
        let items = self.tree_items();
        let Some((_, p, is_dir)) = items.get(idx).cloned() else { return };
        self.tree_sel = idx;
        if is_dir {
            if !self.tree_open.remove(&p) {
                self.tree_open.insert(p);
            }
        } else {
            let abs = self.vault.root.join(&p);
            self.push_history();
            self.open(&abs, new_tab);
        }
    }

    fn files_key(&mut self, name: &str) {
        let items = self.tree_items();
        let n = items.len();
        match name {
            "up" | "k" => self.tree_sel = self.tree_sel.saturating_sub(1),
            "down" | "j" => self.tree_sel = (self.tree_sel + 1).min(n.saturating_sub(1)),
            "home" => self.tree_sel = 0,
            "end" => self.tree_sel = n.saturating_sub(1),
            "pageup" => self.tree_sel = self.tree_sel.saturating_sub(self.left_rect.h),
            "pagedown" => self.tree_sel = (self.tree_sel + self.left_rect.h).min(n.saturating_sub(1)),
            "enter" | "space" | "l" => {
                self.tree_activate(self.tree_sel, false);
                if !items.get(self.tree_sel).is_some_and(|x| x.2) {
                    self.focus = Focus::Editor;
                }
            }
            "right" => {
                if let Some((_, p, true)) = items.get(self.tree_sel) {
                    self.tree_open.insert(p.clone());
                }
            }
            "left" | "h" => {
                if let Some((_, p, d)) = items.get(self.tree_sel).cloned() {
                    if d && self.tree_open.contains(&p) {
                        self.tree_open.remove(&p);
                    } else if let Some(parent) = p.parent()
                        && let Some(i) = items.iter().position(|x| x.1 == parent)
                    {
                        self.tree_sel = i;
                    }
                }
            }
            "escape" | "tab" => self.focus = Focus::Editor,
            _ => {}
        }
    }

    pub fn outline(&mut self) -> Vec<(usize, u8, String)> {
        if self.tabs.is_empty() {
            return vec![];
        }
        let ed = self.ed();
        let lines = ed.buf.lines.clone();
        let doc = ed.doc().clone();
        md::outline(&lines, &doc)
    }

    pub fn current_backlinks(&mut self) -> Vec<(PathBuf, usize, String)> {
        let Some(p) = self.tabs[self.cur].ed.path.clone() else { return vec![] };
        if let Some((bp, t, v)) = &self.backlinks
            && *bp == p
            && t.elapsed() < Duration::from_secs(5)
        {
            return v.clone();
        }
        let v = self.vault.backlinks(&p);
        self.backlinks = Some((p, Instant::now(), v.clone()));
        v
    }

    pub fn panel_len(&mut self) -> usize {
        match self.right_tab {
            0 => self.outline().len(),
            1 => self.current_backlinks().len(),
            _ => self.vault.tags().len(),
        }
    }

    fn panel_activate(&mut self, idx: usize) {
        match self.right_tab {
            0 => {
                if let Some((ln, _, _)) = self.outline().get(idx).cloned() {
                    self.push_history();
                    let len = self.ed().line(ln).len();
                    self.ed().set_cursor(Pos::new(ln, len), false);
                    self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed));
                    self.after_move();
                }
            }
            1 => {
                if let Some((p, ln, _)) = self.current_backlinks().get(idx).cloned() {
                    self.push_history();
                    let abs = self.vault.root.join(p);
                    self.open(&abs, false);
                    let pos = self.ed().buf.clamp(Pos::new(ln, 0));
                    self.ed().set_cursor(pos, false);
                    self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed));
                }
            }
            _ => {
                if let Some((t, _)) = self.vault.tags().get(idx).cloned() {
                    self.follow(LinkTarget::Tag(t), false);
                }
            }
        }
    }

    fn panel_key(&mut self, name: &str) {
        let n = self.panel_len();
        match name {
            "up" | "k" => self.right_sel = self.right_sel.saturating_sub(1),
            "down" | "j" => self.right_sel = (self.right_sel + 1).min(n.saturating_sub(1)),
            "left" | "h" => {
                self.right_tab = (self.right_tab + 2) % 3;
                self.right_sel = 0;
            }
            "right" | "l" => {
                self.right_tab = (self.right_tab + 1) % 3;
                self.right_sel = 0;
            }
            "enter" | "space" => {
                self.panel_activate(self.right_sel);
                self.focus = Focus::Editor;
            }
            "escape" | "tab" => self.focus = Focus::Editor,
            _ => {}
        }
    }

    // ------------------------------------------------------------------ overlays
    pub fn palette_items(&self, q: &str) -> Vec<(Cmd, Vec<usize>)> {
        let titles: Vec<&str> = commands::COMMANDS.iter().map(|c| c.1).collect();
        fuzzy::rank(q, &titles).into_iter().map(|(i, _, m)| (commands::COMMANDS[i].0, m)).collect()
    }

    /// Switcher entries: (label, path or None for "create")
    pub fn switcher_items(&self, q: &str) -> Vec<(String, Option<PathBuf>, Vec<usize>)> {
        let mut out = vec![];
        if q.trim().is_empty() {
            for p in &self.recent {
                out.push((self.vault.rel(p), Some(p.clone()), vec![]));
            }
            for n in &self.vault.notes {
                let abs = self.vault.root.join(n);
                if !self.recent.contains(&abs) {
                    out.push((n.to_string_lossy().to_string(), Some(abs), vec![]));
                }
            }
        } else {
            let cands: Vec<String> = self.vault.notes.iter().map(|p| p.to_string_lossy().to_string()).collect();
            for (i, _, m) in fuzzy::rank(q, &cands).into_iter().take(200) {
                out.push((cands[i].clone(), Some(self.vault.root.join(&self.vault.notes[i])), m));
            }
            let exact = self.vault.notes.iter().any(|n| n.with_extension("").to_string_lossy().eq_ignore_ascii_case(q.trim()));
            if !exact {
                out.push((format!("Create note: {}", q.trim()), None, vec![]));
            }
        }
        out
    }

    fn overlay_key(&mut self, k: &Key, name: &str) {
        let Some(mut ov) = self.overlay.take() else { return };
        let keep = match &mut ov {
            Overlay::Help { scroll } => match name {
                "up" | "k" => {
                    *scroll = scroll.saturating_sub(1);
                    true
                }
                "down" | "j" => {
                    *scroll += 1;
                    true
                }
                "pageup" => {
                    *scroll = scroll.saturating_sub(10);
                    true
                }
                "pagedown" | "space" => {
                    *scroll += 10;
                    true
                }
                _ => false,
            },
            Overlay::Confirm { kind, .. } => {
                match (name, &kind) {
                    ("y" | "Y" | "enter", ConfirmKind::Quit) => {
                        for t in &mut self.tabs {
                            if t.ed.dirty() {
                                let _ = t.ed.save();
                            }
                        }
                        self.quit = true;
                    }
                    ("n" | "N", ConfirmKind::Quit) => self.quit = true,
                    ("y" | "Y" | "enter", ConfirmKind::CloseTab(i)) => {
                        let i = *i;
                        let _ = self.tabs[i].ed.save();
                        self.close_tab(i, true);
                    }
                    ("n" | "N", ConfirmKind::CloseTab(i)) => {
                        let i = *i;
                        self.close_tab(i, true);
                    }
                    _ => {}
                }
                false
            }
            Overlay::Palette { input, sel } => {
                let n = self.palette_items(&input.text).len();
                match name {
                    "escape" => false,
                    "up" | "ctrl+p" | "shift+tab" => {
                        *sel = (*sel + n.max(1) - 1) % n.max(1);
                        true
                    }
                    "down" | "ctrl+n" | "tab" => {
                        *sel = (*sel + 1) % n.max(1);
                        true
                    }
                    "enter" => {
                        let items = self.palette_items(&input.text);
                        if let Some((c, _)) = items.get(*sel) {
                            let c = *c;
                            self.overlay = None;
                            self.run(c);
                            return;
                        }
                        false
                    }
                    _ => {
                        if input.handle(k) {
                            *sel = 0;
                        }
                        true
                    }
                }
            }
            Overlay::Switcher { input, sel } => {
                let n = self.switcher_items(&input.text).len();
                match name {
                    "escape" => false,
                    "up" | "ctrl+p" | "shift+tab" => {
                        *sel = (*sel + n.max(1) - 1) % n.max(1);
                        true
                    }
                    "down" | "ctrl+n" | "tab" => {
                        *sel = (*sel + 1) % n.max(1);
                        true
                    }
                    "enter" | "ctrl+enter" | "alt+enter" => {
                        let items = self.switcher_items(&input.text);
                        let q = input.text.trim().to_string();
                        self.overlay = None;
                        match items.get(*sel) {
                            Some((_, Some(p), _)) => {
                                let p = p.clone();
                                self.push_history();
                                self.open(&p, name != "enter");
                            }
                            Some((_, None, _)) if !q.is_empty() => self.new_note(&q),
                            _ => {}
                        }
                        return;
                    }
                    _ => {
                        if input.handle(k) {
                            *sel = 0;
                        }
                        true
                    }
                }
            }
            Overlay::Search { input, sel, results, dirty } => match name {
                "escape" => false,
                "up" | "ctrl+p" => {
                    *sel = sel.saturating_sub(1);
                    true
                }
                "down" | "ctrl+n" => {
                    *sel = (*sel + 1).min(results.len().saturating_sub(1));
                    true
                }
                "enter" => {
                    if *dirty {
                        *results = self.vault.search(&input.text, 500);
                        *dirty = false;
                        *sel = 0;
                        true
                    } else if let Some((p, ln, col, _)) = results.get(*sel).cloned() {
                        self.overlay = None;
                        self.push_history();
                        let abs = self.vault.root.join(p);
                        self.open(&abs, false);
                        let q = input.text.clone();
                        let pos = self.ed().buf.clamp(Pos::new(ln, col));
                        let end = self.ed().buf.clamp(Pos::new(ln, col + q.len()));
                        self.ed().select_range(pos, end);
                        self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed));
                        return;
                    } else {
                        true
                    }
                }
                _ => {
                    if input.handle(k) {
                        // live search for short vaults, otherwise on Enter
                        if self.vault.notes.len() <= 2000 {
                            *results = self.vault.search(&input.text, 500);
                            *dirty = false;
                            *sel = 0;
                        } else {
                            *dirty = true;
                        }
                    }
                    true
                }
            },
            Overlay::Prompt { input, kind, .. } => match name {
                "escape" => false,
                "enter" => {
                    let t = input.text.trim().to_string();
                    match kind {
                        PromptKind::GoToLine => {
                            if let Ok(n) = t.parse::<usize>() {
                                let p = self.ed().buf.clamp(Pos::new(n.saturating_sub(1), 0));
                                self.ed().set_cursor(p, false);
                                self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed));
                            }
                        }
                        PromptKind::NewNote => {
                            if !t.is_empty() {
                                self.overlay = None;
                                self.new_note(&t);
                                return;
                            }
                        }
                        PromptKind::Rename => {
                            self.overlay = None;
                            self.rename_note(&t);
                            return;
                        }
                    }
                    false
                }
                _ => {
                    input.handle(k);
                    true
                }
            },
        };
        if keep && self.overlay.is_none() {
            self.overlay = Some(ov);
        }
    }

    fn rename_note(&mut self, new_name: &str) {
        let Some(old) = self.tabs[self.cur].ed.path.clone() else { return };
        if new_name.is_empty() {
            return;
        }
        let old_stem = old.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let new_path = old.with_file_name(format!("{}.md", new_name.trim_end_matches(".md")));
        if new_path.exists() {
            self.set_error("A note with that name already exists");
            return;
        }
        if self.ed().dirty() {
            let _ = self.ed().save();
        }
        if let Err(e) = std::fs::rename(&old, &new_path) {
            self.set_error(format!("Rename failed: {e}"));
            return;
        }
        let new_stem = new_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        // update [[links]] across the vault
        let mut changed = 0;
        for rel in self.vault.notes.clone() {
            let abs = self.vault.root.join(&rel);
            let abs = if abs == old { new_path.clone() } else { abs };
            let Ok(text) = std::fs::read_to_string(&abs) else { continue };
            let mut out = String::with_capacity(text.len());
            let mut n = 0;
            for (li, line) in text.split('\n').enumerate() {
                if li > 0 {
                    out.push('\n');
                }
                let spans = md::parse_inline(line, 0, line.len());
                let mut last = 0;
                for s in spans.iter().filter(|s| matches!(s.kind, md::SpanKind::WikiLink | md::SpanKind::Embed) && s.target.eq_ignore_ascii_case(&old_stem)) {
                    let tstart = s.start + if s.kind == md::SpanKind::Embed { 3 } else { 2 };
                    out.push_str(&line[last..tstart]);
                    out.push_str(&new_stem);
                    last = tstart + s.target.len();
                    n += 1;
                }
                out.push_str(&line[last..]);
            }
            if n > 0 && std::fs::write(&abs, out).is_ok() {
                changed += n;
            }
        }
        for t in &mut self.tabs {
            if t.ed.path.as_deref() == Some(old.as_path()) {
                t.ed.path = Some(new_path.clone());
            }
            if let Some(p) = t.ed.path.clone() {
                if !t.ed.dirty() && t.ed.changed_on_disk() {
                    let _ = t.ed.reload();
                }
                let _ = p;
            }
        }
        self.vault_changed();
        self.layout.clear();
        self.set_status(format!("Renamed to {new_stem} · updated {changed} link{}", if changed == 1 { "" } else { "s" }));
    }

    // ------------------------------------------------------------------ commands
    pub fn run(&mut self, cmd: Cmd) {
        use Cmd::*;
        let read = self.tabs.get(self.cur).is_some_and(|t| t.view.mode == Mode::Read);
        let editing = matches!(
            cmd,
            Undo | Redo
                | Cut
                | Paste
                | Bold
                | Italic
                | Strike
                | Highlight
                | InlineCode
                | Link
                | WikiLink
                | ToggleTask
                | Paragraph
                | H1
                | H2
                | H3
                | H4
                | H5
                | H6
                | CycleHeading
                | Quote
                | BulletList
                | NumberedList
                | CodeBlock
                | InsertTable
                | InsertHr
                | MoveLineUp
                | MoveLineDown
                | Duplicate
                | DeleteLine
                | JoinLines
                | Indent
                | Outdent
                | FormatTable
                | Replace
        );
        if editing && read {
            self.set_status("Reading view — press Ctrl+E to edit");
            return;
        }
        let mut edited = false;
        match cmd {
            Save => self.save(),
            SaveAll => {
                for t in &mut self.tabs {
                    if t.ed.dirty() {
                        let _ = t.ed.save();
                    }
                }
                self.set_status("Saved all");
            }
            Quit => {
                let dirty = self.tabs.iter().any(|t| t.ed.dirty());
                if dirty && self.config.autosave {
                    for t in &mut self.tabs {
                        if t.ed.dirty() && t.ed.path.is_some() {
                            let _ = t.ed.save();
                        }
                    }
                }
                if self.tabs.iter().any(|t| t.ed.dirty()) {
                    self.overlay = Some(Overlay::Confirm { msg: "Unsaved changes. Save before quitting? (y/n/esc)".into(), kind: ConfirmKind::Quit });
                } else {
                    self.quit = true;
                }
            }
            Undo => {
                if !self.ed().undo() {
                    self.set_status("Nothing to undo");
                }
                edited = true;
            }
            Redo => {
                if !self.ed().redo() {
                    self.set_status("Nothing to redo");
                }
                edited = true;
            }
            Copy | Cut => {
                let ed = self.ed();
                let line_mode = ed.selection().is_none();
                if line_mode {
                    ed.select_line();
                }
                let t = ed.selected_text().unwrap_or_default();
                if cmd == Cut {
                    ed.backspace();
                    edited = true;
                } else if line_mode {
                    ed.anchor = None;
                    let c = ed.cursor;
                    ed.cursor = Pos::new(c.line.saturating_sub(if t.ends_with('\n') { 1 } else { 0 }), 0);
                }
                self.set_clipboard(&t);
            }
            Paste => {
                let t = (self.system_paste)().filter(|s| !s.is_empty()).unwrap_or_else(|| self.clipboard.clone());
                if !t.is_empty() {
                    self.ed().paste(&t);
                    edited = true;
                }
            }
            SelectAll => self.ed().select_all(),
            SelectLine => self.ed().select_line(),
            SelectWord => self.ed().select_word(),
            Find | Replace => {
                let sel = self.ed().selected_text().filter(|t| !t.contains('\n'));
                let replacing = cmd == Replace;
                match &mut self.find {
                    Some(f) => {
                        f.replacing |= replacing;
                        f.field = if replacing && !f.query.text.is_empty() { 1 } else { 0 };
                        if let Some(s) = sel {
                            f.query = LineInput::new(&s);
                        }
                    }
                    None => {
                        let q = sel.unwrap_or_default();
                        self.find = Some(FindBar { query: LineInput::new(&q), replace: LineInput::default(), replacing, field: 0, case: false });
                    }
                }
                self.layout_panes();
            }
            FindNext | FindPrev => {
                if let Some(f) = &self.find {
                    let (q, c) = (f.query.text.clone(), f.case);
                    self.ed().find_next(&q, c, cmd == FindPrev);
                }
            }
            QuickSwitch => self.overlay = Some(Overlay::Switcher { input: LineInput::default(), sel: if self.recent.len() > 1 { 1 } else { 0 } }),
            Palette => self.overlay = Some(Overlay::Palette { input: LineInput::default(), sel: 0 }),
            NewNote => self.new_note("Untitled"),
            NewTab => {
                self.tabs.push(Tab { ed: Editor::new(""), view: View::new(Mode::Live) });
                self.cur = self.tabs.len() - 1;
                self.overlay = Some(Overlay::Switcher { input: LineInput::default(), sel: 0 });
            }
            CloseTab => self.close_tab(self.cur, false),
            NextTab => {
                self.cur = (self.cur + 1) % self.tabs.len();
                self.after_open();
            }
            PrevTab => {
                self.cur = (self.cur + self.tabs.len() - 1) % self.tabs.len();
                self.after_open();
            }
            ToggleReading => {
                let t = self.tab();
                t.view.mode = if t.view.mode == Mode::Read { Mode::Live } else { Mode::Read };
                let m = t.view.mode;
                self.set_status(if m == Mode::Read { "Reading view" } else { "Live preview" });
            }
            ToggleSource => {
                let t = self.tab();
                t.view.mode = if t.view.mode == Mode::Source { Mode::Live } else { Mode::Source };
                let m = t.view.mode;
                self.set_status(if m == Mode::Source { "Source mode" } else { "Live preview" });
            }
            ToggleLeft => {
                self.show_left = !self.show_left;
                if !self.show_left && self.focus == Focus::Files {
                    self.focus = Focus::Editor;
                }
            }
            ToggleRight => {
                self.show_right = !self.show_right;
                if !self.show_right && self.focus == Focus::Panel {
                    self.focus = Focus::Editor;
                }
            }
            FocusFiles => {
                self.show_left = true;
                self.focus = if self.focus == Focus::Files { Focus::Editor } else { Focus::Files };
                if let Some(p) = self.tabs[self.cur].ed.path.clone() {
                    let rel = p.strip_prefix(&self.vault.root).map(Path::to_path_buf).unwrap_or_default();
                    if let Some(i) = self.tree_items().iter().position(|x| x.1 == rel) {
                        self.tree_sel = i;
                    }
                }
            }
            FocusOutline => {
                self.show_right = true;
                self.focus = if self.focus == Focus::Panel { Focus::Editor } else { Focus::Panel };
            }
            Bold | Italic | Strike | Highlight | InlineCode => {
                let m = match cmd {
                    Bold => "**",
                    Italic => "*",
                    Strike => "~~",
                    Highlight => "==",
                    _ => "`",
                };
                self.ed().toggle_wrap(m);
                edited = true;
            }
            Link => {
                self.ed().insert_link();
                edited = true;
            }
            WikiLink => {
                self.ed().insert_wikilink();
                edited = true;
            }
            ToggleTask => {
                self.ed().toggle_task();
                edited = true;
            }
            Paragraph | H1 | H2 | H3 | H4 | H5 | H6 => {
                let lvl = match cmd {
                    H1 => 1,
                    H2 => 2,
                    H3 => 3,
                    H4 => 4,
                    H5 => 5,
                    H6 => 6,
                    _ => 0,
                };
                self.ed().set_heading(lvl);
                edited = true;
            }
            CycleHeading => {
                self.ed().cycle_heading();
                edited = true;
            }
            Quote => {
                self.ed().toggle_quote();
                edited = true;
            }
            BulletList | NumberedList => {
                self.ed().toggle_list(cmd == NumberedList);
                edited = true;
            }
            CodeBlock => {
                self.ed().insert_code_block();
                edited = true;
            }
            InsertTable => {
                self.ed().insert_table();
                edited = true;
            }
            InsertHr => {
                self.ed().insert_hr();
                edited = true;
            }
            MoveLineUp | MoveLineDown => {
                self.ed().move_lines(cmd == MoveLineDown);
                edited = true;
            }
            Duplicate => {
                self.ed().duplicate();
                edited = true;
            }
            DeleteLine => {
                self.ed().delete_lines();
                edited = true;
            }
            JoinLines => {
                self.ed().join_lines();
                edited = true;
            }
            Indent => {
                self.ed().indent();
                edited = true;
            }
            Outdent => {
                self.ed().outdent();
                edited = true;
            }
            FormatTable => {
                let l = self.ed().cursor.line;
                self.ed().format_table(l);
                edited = true;
            }
            FollowLink | FollowLinkNewTab => {
                let p = self.ed().cursor;
                match self.ed().link_at(p) {
                    Some(t) => self.follow(t, cmd == FollowLinkNewTab),
                    None => self.set_status("No link under cursor"),
                }
            }
            Back => self.nav(true),
            Forward => self.nav(false),
            GoToLine => self.overlay = Some(Overlay::Prompt { title: "Go to line".into(), input: LineInput::default(), kind: PromptKind::GoToLine }),
            SearchVault => {
                let q = self.ed().selected_text().filter(|t| !t.contains('\n')).unwrap_or_default();
                let results = self.vault.search(&q, 500);
                self.overlay = Some(Overlay::Search { input: LineInput::new(&q), sel: 0, results, dirty: false });
            }
            Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            ToggleLineNumbers => self.config.line_numbers = !self.config.line_numbers,
            ToggleReadable => self.config.readable_width = if self.config.readable_width == 0 { 100 } else { 0 },
            CycleTheme => {
                self.theme_idx = (self.theme_idx + 1) % THEMES.len();
                let name = THEMES[self.theme_idx];
                self.theme = Theme::named(name).unwrap_or_else(|| Theme::terminal(&self.caps.palette));
                self.layout.clear();
                self.screen.invalidate();
                self.set_status(format!("Theme: {name}"));
            }
            ReloadFile => {
                let r = self.ed().reload();
                match r {
                    Ok(()) => self.set_status("Reloaded from disk"),
                    Err(e) => self.set_error(format!("Reload failed: {e}")),
                }
            }
            RenameNote => {
                let cur = self.tab_name(self.cur);
                self.overlay = Some(Overlay::Prompt { title: "Rename note".into(), input: LineInput::new(&cur), kind: PromptKind::Rename });
            }
            ToggleAutosave => {
                self.config.autosave = !self.config.autosave;
                let s = if self.config.autosave { "Autosave on" } else { "Autosave off" };
                self.set_status(s);
            }
            Suspend => self.extra_out.push_str("\x00suspend"),
            CenterCursor => self.with_env(|env, tab| tab.view.center_cursor(env, &mut tab.ed)),
            ScrollUp | ScrollDown => {
                self.layout_panes();
                let d = if cmd == ScrollUp { -3 } else { 3 };
                self.with_env(|env, tab| tab.view.scroll(env, &mut tab.ed, d));
                return;
            }
            OpenDaily => {
                let name = today();
                let from = self.tabs[self.cur].ed.path.clone();
                match self.vault.resolve(&name, from.as_deref()) {
                    Some(p) => {
                        self.push_history();
                        self.open(&p, false);
                    }
                    None => self.new_note(&name),
                }
            }
        }
        if edited {
            self.after_edit();
        } else {
            self.after_move();
        }
    }

    pub fn set_clipboard(&mut self, t: &str) {
        use base64::Engine;
        self.clipboard = t.to_string();
        let b = base64::engine::general_purpose::STANDARD.encode(t.as_bytes());
        if b.len() < 100_000 {
            self.extra_out.push_str(&format!("\x1b]52;c;{b}\x07"));
        }
    }

    // ------------------------------------------------------------------ mouse
    pub fn mouse(&mut self, m: Mouse) {
        let (x, y) = (m.x as usize, m.y as usize);
        if self.overlay.is_some() {
            if m.kind == MouseKind::Press && m.button == 0 {
                let hit = self.overlay_hits.iter().find(|(r, _)| r.contains(x, y)).map(|h| h.1);
                match hit {
                    Some(usize::MAX) => {}
                    Some(i) => {
                        match &mut self.overlay {
                            Some(Overlay::Palette { sel, .. }) | Some(Overlay::Switcher { sel, .. }) | Some(Overlay::Search { sel, .. }) => *sel = i,
                            _ => {}
                        }
                        let k = Key::new("enter", 0);
                        self.key(&k);
                    }
                    None => self.overlay = None,
                }
            } else if m.kind == MouseKind::Wheel {
                let k = Key::new(if m.button == 0 { "up" } else { "down" }, 0);
                self.key(&k);
            }
            return;
        }
        if m.kind == MouseKind::Wheel {
            let d: isize = if m.button == 0 {
                -3
            } else if m.button == 1 {
                3
            } else {
                0
            };
            if self.left_rect.contains(x, y) {
                let n = self.tree_items().len();
                self.tree_scroll = (self.tree_scroll as isize + d).clamp(0, n.saturating_sub(1) as isize) as usize;
            } else if self.right_rect.contains(x, y) {
                self.right_scroll = (self.right_scroll as isize + d).max(0) as usize;
            } else {
                self.layout_panes();
                self.with_env(|env, tab| tab.view.scroll(env, &mut tab.ed, d));
            }
            return;
        }
        if y == 0 && m.kind == MouseKind::Press {
            if let Some(&(_, _, i, close)) = self.tab_hits.iter().find(|h| h.0 <= x && x < h.1) {
                if close || m.button == 1 {
                    self.close_tab(i, false);
                } else {
                    self.cur = i;
                    self.after_open();
                }
            }
            return;
        }
        if self.left_rect.contains(x, y) && m.kind == MouseKind::Press {
            self.focus = Focus::Files;
            let idx = self.tree_scroll + (y - self.left_rect.y).saturating_sub(1);
            if y > self.left_rect.y {
                self.tree_activate(idx, m.mods & CTRL != 0 || m.button == 1);
                if !self.tree_items().get(idx).is_some_and(|x| x.2) {
                    self.focus = Focus::Editor;
                }
            }
            return;
        }
        if self.right_rect.contains(x, y) && m.kind == MouseKind::Press {
            self.focus = Focus::Panel;
            if y == self.right_rect.y {
                let third = (self.right_rect.w / 3).max(1);
                self.right_tab = ((x - self.right_rect.x) / third).min(2);
                self.right_sel = 0;
                self.right_scroll = 0;
            } else if y >= self.right_rect.y + 2 {
                let idx = self.right_scroll + (y - self.right_rect.y - 2);
                self.right_sel = idx;
                self.panel_activate(idx);
                self.focus = Focus::Editor;
            }
            return;
        }
        // completion popup clicks
        if !self.editor_rect.contains(x, y) && !self.dragging {
            return;
        }
        self.focus = Focus::Editor;
        let read = self.tabs[self.cur].view.mode == Mode::Read;
        let pos = {
            let t = &self.tabs[self.cur];
            t.view.hit(x, y, &t.ed)
        };
        match m.kind {
            MouseKind::Press if m.button == 0 => {
                let Some(p) = pos else { return };
                let now = Instant::now();
                let (t0, x0, y0, n0) = self.clicks;
                let count = if now.duration_since(t0) < Duration::from_millis(450) && x0.abs_diff(x) <= 1 && y0 == y { (n0 % 3) + 1 } else { 1 };
                self.clicks = (now, x, y, count);
                // links: ctrl+click (or plain click in reading view)
                if m.mods & CTRL != 0 || read {
                    if let Some(t) = self.ed().link_at(p) {
                        self.follow(t, m.mods & SHIFT != 0);
                        return;
                    }
                    if read {
                        return;
                    }
                }
                // checkbox toggle
                if count == 1 && m.mods == 0 {
                    let info = self.ed().doc().infos[p.line].clone();
                    if let Some(lm) = info.lst
                        && lm.task.is_some()
                        && p.col >= lm.task_start
                        && p.col < lm.task_start + 3
                        && self.ed().cursor.line != p.line
                    {
                        self.ed().toggle_task_at(p.line);
                        self.after_edit();
                        return;
                    }
                }
                self.completion = None;
                match count {
                    2 => {
                        self.ed().set_cursor(p, false);
                        self.ed().select_word();
                    }
                    3 => {
                        self.ed().set_cursor(p, false);
                        self.ed().select_line();
                    }
                    _ => {
                        let shift = m.mods & SHIFT != 0;
                        self.ed().set_cursor(p, shift);
                        self.ed().goal_x = None;
                    }
                }
                self.dragging = true;
                self.drag_y = None;
            }
            MouseKind::Drag if self.dragging && m.button == 0 => {
                let r = self.editor_rect;
                if y < r.y {
                    self.drag_y = Some(-1);
                } else if y >= r.y + r.h {
                    self.drag_y = Some(1);
                } else {
                    self.drag_y = None;
                }
                let yy = y.clamp(r.y, r.y + r.h.saturating_sub(1));
                let p = {
                    let t = &self.tabs[self.cur];
                    t.view.hit(x, yy, &t.ed)
                };
                if let Some(p) = p {
                    if self.ed().anchor.is_none() {
                        let c = self.ed().cursor;
                        self.ed().anchor = Some(c);
                    }
                    self.ed().cursor = p;
                }
            }
            MouseKind::Release => {
                self.dragging = false;
                self.drag_y = None;
                let ed = self.ed();
                if ed.anchor == Some(ed.cursor) {
                    ed.anchor = None;
                }
            }
            MouseKind::Press if m.button == 1 => {
                // middle click: paste primary/internal clipboard
                if let Some(p) = pos {
                    self.ed().set_cursor(p, false);
                    let t = self.clipboard.clone();
                    self.ed().paste(&t);
                    self.after_edit();
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ time
    /// Periodic work. Returns how soon we'd like to be called again.
    pub fn tick(&mut self) -> Option<Duration> {
        let mut next: Option<Duration> = None;
        let mut want = |d: Duration| next = Some(next.map_or(d, |n| n.min(d)));
        if let Some(d) = self.drag_y {
            let p = self.ed().cursor;
            self.with_env(|env, tab| {
                tab.view.scroll(env, &mut tab.ed, d);
                tab.view.move_rows(env, &mut tab.ed, d, true);
            });
            let _ = p;
            want(Duration::from_millis(50));
        }
        if let Some((_, t, _)) = &self.status {
            let left = Duration::from_secs(4).saturating_sub(t.elapsed());
            if left.is_zero() {
                self.status = None;
            } else {
                want(left);
            }
        }
        if self.config.autosave
            && let Some(t) = self.last_edit
        {
            let wait = Duration::from_millis(1200);
            if t.elapsed() >= wait {
                self.last_edit = None;
                let mut saved = false;
                for tab in &mut self.tabs {
                    if tab.ed.dirty() && tab.ed.path.is_some() && tab.ed.save().is_ok() {
                        saved = true;
                    }
                }
                if saved {
                    self.vault_changed();
                }
            } else {
                want(wait - t.elapsed());
            }
        }
        if let Some(t) = self.scroll_seen.1 {
            let left = Duration::from_millis(1250).saturating_sub(t.elapsed());
            if left.is_zero() {
                self.scroll_seen.1 = None;
            } else {
                want(left);
            }
        }
        if self.last_disk_check.elapsed() > Duration::from_secs(2) {
            self.check_disk(false);
        }
        want(Duration::from_secs(2));
        next
    }

    fn check_disk(&mut self, focus: bool) {
        self.last_disk_check = Instant::now();
        for i in 0..self.tabs.len() {
            if self.tabs[i].ed.changed_on_disk() {
                if !self.tabs[i].ed.dirty() {
                    let _ = self.tabs[i].ed.reload();
                    if i == self.cur {
                        self.set_status("File changed on disk — reloaded");
                    }
                } else if i == self.cur {
                    self.set_error("File changed on disk! Your unsaved edits differ (Reload file from disk to discard)");
                    self.tabs[i].ed.disk_mtime = None;
                }
            }
        }
        if focus {
            self.vault_changed();
        }
    }
}

pub fn today() -> String {
    // civil date from the system clock (UTC offset from TZ is not available
    // without a tz database; use localtime_r)
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
}

pub fn is_editable_line(kind: LineKind) -> bool {
    !matches!(kind, LineKind::Front)
}
