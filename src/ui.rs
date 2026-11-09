//! Drawing of everything around the editor pane: tab bar, file explorer,
//! outline/backlinks/tags panel, status bar, find bar, popups and overlays.

use crate::app::{App, Focus, Overlay};
use crate::commands;
use crate::layout::{HeadingMode, Mode};
use crate::screen::Canvas;
use crate::style::{Color, Style};
use crate::text::{str_width, truncate};
use crate::view::Rect;

impl App {
    /// Proportional (Inter) chrome: on when prose is typeset and placeholders work.
    pub fn ui_prop(&self) -> bool {
        self.typeset_on() && self.screen.placeholders
    }

    fn ui_face(st: Style) -> crate::typeset::Face {
        use crate::typeset::Face;
        match (st.has(crate::style::BOLD), st.has(crate::style::ITALIC)) {
            (true, true) => Face::BoldItalic,
            (true, false) => Face::SemiBold,
            (false, true) => Face::Italic,
            _ => Face::Regular,
        }
    }

    /// A cached one-row image of styled runs in Inter: (image, width in cells).
    fn label_img(&mut self, runs: &[(String, Style)], max_w: usize, caret: Option<usize>) -> Option<(std::sync::Arc<crate::images::Img>, usize)> {
        use crate::typeset::{Fonts, Params, Piece};
        use std::hash::{Hash, Hasher};
        let (cw, ch) = (self.images.cell_w, self.images.cell_h);
        let deffg = self.theme.fg.rgb().or(self.caps.palette.fg).unwrap_or((220, 220, 220));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (max_w, cw, ch, deffg, caret).hash(&mut h);
        for (t, st) in runs {
            (t, st).hash(&mut h);
        }
        let key = h.finish();
        if let Some(v) = self.labels.get(&key) {
            return Some(v.clone());
        }
        let fonts = Fonts::get();
        let font_px = ch as f32 * 0.7;
        let params = Params { width_px: 1.0e6, font_px, line_height: ch as f32 / font_px, pad_top: 0.0, cell_w: cw, cell_h: ch };
        let mut pieces = vec![];
        let mut pos = 0;
        for (t, st) in runs {
            let face = Self::ui_face(*st);
            if !fonts.covers(face, t) {
                return None;
            }
            let fg = st.fg.rgb().unwrap_or(deffg);
            pieces.push(Piece {
                text: t.clone(),
                a: pos,
                b: pos + t.len(),
                src: true,
                face,
                scale: 1.0,
                fg,
                bg: None,
                underline: (st.underline != 0).then_some(fg),
                strike: st.has(crate::style::STRIKE),
                tab: None,
            });
            pos += t.len();
        }
        let width = |t: &crate::typeset::Typeset| t.map.lines.first().and_then(|l| l.stops.last()).map_or(0.0, |s| s.1);
        let limit = (max_w * cw as usize) as f32;
        let mut ts = crate::typeset::layout(pieces.clone(), &params);
        if max_w > 0 && width(&ts) > limit {
            // ellipsize: drop characters from the end until it fits
            while let Some(last) = pieces.last_mut() {
                if last.text.pop().is_none() || last.text.is_empty() {
                    pieces.pop();
                    continue;
                }
                let mut p2 = pieces.clone();
                let l = p2.last_mut().unwrap();
                l.text.push('…');
                l.b = l.a + l.text.len();
                ts = crate::typeset::layout(p2, &params);
                if width(&ts) <= limit {
                    break;
                }
            }
        }
        let caret_col = self.theme.accent.rgb().unwrap_or((140, 100, 240));
        let im = ts.render(&crate::typeset::Marks { caret, caret_color: caret_col, ..Default::default() });
        let cols = (((width(&ts) + if caret.is_some() { 3.0 } else { 0.0 }) / cw as f32).ceil() as usize).max(1);
        let img = self.images.adhoc(&im);
        if self.labels.len() > 4000 {
            self.labels.clear();
        }
        self.labels.insert(key, (img.clone(), cols));
        Some((img, cols))
    }

    /// Draw styled runs as one proportional label (cells when off). Returns x after.
    pub fn text_runs(&mut self, cv: &mut Canvas, x: i32, y: i32, runs: &[(String, Style)], clip: usize) -> i32 {
        let all: String = runs.iter().map(|r| r.0.as_str()).collect();
        if all.trim().is_empty() || !self.ui_prop() || x < 0 || y < 0 || y as usize >= cv.h {
            let mut x = x;
            for (t, st) in runs {
                x = cv.put(x, y, t, *st, clip);
            }
            return x;
        }
        let max_w = clip.min(cv.w).saturating_sub(x as usize);
        let Some((img, cols)) = self.label_img(runs, max_w, None) else {
            let mut x = x;
            for (t, st) in runs {
                x = cv.put(x, y, t, *st, clip);
            }
            return x;
        };
        let data = img.data.clone();
        if !cv.images.iter().any(|d| d.id == data.id) {
            cv.images.push(data.clone());
        }
        let (r, g, b) = crate::graphics::id_color(data.id);
        let cells: String = (0..cols.min(data.cols as usize)).map(|c| crate::graphics::placeholder_cell(0, c)).collect();
        let bg = runs.first().map_or(Color::Default, |r| r.1.bg);
        cv.put(x, y, &cells, Style { fg: Color::Rgb(r, g, b), bg, ..Style::new() }, clip);
        x + cols as i32
    }

    /// Draw UI text: Inter as an image when typeset mode is on, cells otherwise.
    /// Drop-in for `cv.put` (returns the x after the text).
    pub fn text(&mut self, cv: &mut Canvas, x: i32, y: i32, s: &str, st: Style, clip: usize) -> i32 {
        self.text_runs(cv, x, y, &[(s.to_string(), st)], clip)
    }

    /// An editable single-line field: proportional text with the caret drawn
    /// in (typeset mode), or cells with the hardware cursor.
    pub fn text_input(&mut self, cv: &mut Canvas, x: usize, y: usize, text: &str, cur: usize, st: Style, clip: usize) {
        let max_w = clip.min(cv.w).saturating_sub(x);
        if self.ui_prop() && !text.is_empty() {
            if let Some((img, cols)) = self.label_img(&[(text.to_string(), st)], max_w, Some(cur)) {
                let data = img.data.clone();
                if !cv.images.iter().any(|d| d.id == data.id) {
                    cv.images.push(data.clone());
                }
                let (r, g, b) = crate::graphics::id_color(data.id);
                let cells: String = (0..cols.min(data.cols as usize)).map(|c| crate::graphics::placeholder_cell(0, c)).collect();
                cv.put(x as i32, y as i32, &cells, Style { fg: Color::Rgb(r, g, b), bg: st.bg, ..Style::new() }, clip);
                let before = self.text_w(&text[..cur], st);
                cv.cursor = Some(((x + before).min(clip.saturating_sub(1)), y));
                cv.cursor_hidden = true;
                return;
            }
        }
        cv.put(x as i32, y as i32, &truncate(text, max_w), st, clip);
        cv.cursor = Some(((x + str_width(&text[..cur])).min(x + max_w), y));
        cv.cursor_hidden = false;
    }

    /// Width in cells that `text()` will use.
    pub fn text_w(&mut self, s: &str, st: Style) -> usize {
        if self.ui_prop() && !s.trim().is_empty() {
            if let Some((_, c)) = self.label_img(&[(s.to_string(), st)], 1000, None) {
                return c;
            }
        }
        str_width(s)
    }

    pub fn render(&mut self) -> String {
        if self.tabs.is_empty() {
            self.open_text("");
        }
        self.w = self.w.max(1);
        self.h = self.h.max(1);
        self.layout_panes();
        let (w, h) = (self.w, self.h);
        let mut cv = Canvas::new(w, h, self.theme.base());
        self.overlay_hits.clear();
        if self.left_rect.w > 0 {
            self.draw_files(&mut cv);
        }
        if self.right_rect.w > 0 {
            self.draw_panel(&mut cv);
        }
        // editor
        let (matches, cur_match) = match &self.find {
            Some(f) if !f.query.text.is_empty() => {
                let (q, c) = (f.query.text.clone(), f.case);
                let ed = &self.tabs[self.cur].ed;
                let m = ed.find_all(&q, c);
                let sel = ed.selection();
                let cm = m.iter().position(|x| Some(*x) == sel);
                (m, cm)
            }
            _ => (vec![], None),
        };
        let focused = self.focus == Focus::Editor && self.overlay.is_none() && self.find.is_none();
        let placeholders = self.screen.placeholders;
        let cell = (self.images.cell_w, self.images.cell_h);
        let cursor = self.with_env(|env, tab| tab.view.draw(&mut cv, env, &mut tab.ed, env.ctx.theme, &matches, cur_match, focused, placeholders, cell));
        if !focused {
            cv.cursor = None;
        }
        let _ = cursor;
        self.draw_scrollbar(&mut cv);
        self.draw_status(&mut cv, matches.len(), cur_match);
        if self.find.is_some() {
            self.draw_find(&mut cv, matches.len(), cur_match);
        }
        if self.completion.is_some() && focused {
            self.draw_completion(&mut cv);
        }
        if self.overlay.is_some() {
            self.draw_overlay(&mut cv);
        }
        let name = self.tab_name(self.cur);
        let dirty = if self.tabs[self.cur].ed.dirty() { "● " } else { "" };
        cv.title = Some(format!("{dirty}{name} — glyph"));
        let mut out = self.screen.render(cv);
        if !self.extra_out.is_empty() {
            out.push_str(&std::mem::take(&mut self.extra_out));
        }
        out
    }

    fn chrome_bg(&self) -> Style {
        if self.theme.rgb { Style::bg(self.theme.sidebar) } else { Style::new() }
    }

    fn draw_files(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let r = self.left_rect;
        let bg = self.chrome_bg();
        cv.fill(r.x, r.y, r.w, r.h, bg);
        for y in r.y..r.y + r.h {
            cv.put(r.w as i32, y as i32, "│", Style::fg(th.border).with_bg(th.bg), self.w);
        }
        let head = self.vault.root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        self.text(cv, r.x as i32 + 2, r.y as i32, &truncate(&head, r.w.saturating_sub(3)), Style::fg(th.fg).with_bg(bg.bg).bold(), r.x + r.w);
        let items = self.tree_items();
        let vis = r.h.saturating_sub(1);
        if self.focus == Focus::Files {
            if self.tree_sel < self.tree_scroll {
                self.tree_scroll = self.tree_sel;
            } else if self.tree_sel >= self.tree_scroll + vis {
                self.tree_scroll = self.tree_sel + 1 - vis;
            }
        }
        let current = self.tabs[self.cur].ed.path.clone().and_then(|p| p.strip_prefix(&self.vault.root).ok().map(|p| p.to_path_buf()));
        for (k, (depth, p, is_dir)) in items.iter().enumerate().skip(self.tree_scroll).take(vis) {
            let y = r.y + 1 + k - self.tree_scroll;
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let is_md = crate::vault::is_note(p);
            let (icon, label) = if *is_dir {
                (if self.tree_open.contains(p) { "▾ " } else { "▸ " }, name)
            } else if is_md {
                ("  ", p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(name))
            } else {
                ("  ", name)
            };
            let mut st = if *is_dir {
                Style::fg(th.fg)
            } else if is_md {
                Style::fg(th.muted)
            } else {
                Style::fg(th.faint)
            };
            let active = current.as_ref() == Some(p);
            let focused = self.focus == Focus::Files && k == self.tree_sel;
            // Obsidian: neutral highlight for the open file, accent ring for focus
            let row_bg = if active || focused { if th.rgb { th.hover } else { Color::Default } } else { bg.bg };
            if active {
                st = Style::fg(th.fg);
            }
            if focused && !th.rgb {
                st = st.reverse();
            }
            st = st.with_bg(row_bg);
            cv.fill(r.x + 1, y, r.w.saturating_sub(2), 1, Style::bg(row_bg));
            // indent guides under each ancestor folder
            let guide = Style::fg(th.border).with_bg(row_bg);
            for d in 0..*depth {
                cv.put((r.x + 2 + 2 * d) as i32, y as i32, "│", guide, r.x + r.w);
            }
            let x0 = (r.x + 2 + 2 * depth) as i32;
            let x = cv.put(x0, y as i32, icon, Style::fg(th.faint).with_bg(row_bg), r.x + r.w);
            self.text(cv, x, y as i32, &label, if focused { st.with_fg(th.accent) } else { st }, (r.x + r.w).saturating_sub(1));
        }
    }

    fn draw_panel(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let r = self.right_rect;
        let bg = self.chrome_bg();
        cv.fill(r.x, r.y, r.w, r.h, bg);
        for y in r.y..r.y + r.h {
            cv.put(r.x as i32 - 1, y as i32, "│", Style::fg(th.border).with_bg(th.bg), self.w);
        }
        let names = ["Outline", "Links", "Tags"];
        let third = (r.w / 3).max(1);
        for (i, n) in names.iter().enumerate() {
            let active = i == self.right_tab;
            let st = if active { Style::fg(th.fg).bold().underline(crate::style::UL_SINGLE, th.accent) } else { Style::fg(th.faint) };
            let nw = self.text_w(n, st);
            let x = r.x + i * third + (third.saturating_sub(nw)) / 2;
            self.text(cv, x as i32, r.y as i32, n, st.with_bg(bg.bg), r.x + r.w);
        }
        let items: Vec<(String, Style, usize)> = match self.right_tab {
            0 => {
                let cur_line = self.tabs[self.cur].ed.cursor.line;
                let out = self.outline();
                let active = out.iter().rposition(|(l, _, _)| *l <= cur_line);
                let min_level = out.iter().map(|o| o.1).min().unwrap_or(1);
                out.iter()
                    .enumerate()
                    .map(|(k, (_, lvl, t))| {
                        let st = if Some(k) == active {
                            Style::fg(th.accent).bold()
                        } else if *lvl <= 2 {
                            Style::fg(th.fg)
                        } else {
                            Style::fg(th.muted)
                        };
                        (t.clone(), st, (*lvl - min_level) as usize * 2)
                    })
                    .collect()
            }
            1 => {
                let bl = self.current_backlinks();
                if bl.is_empty() {
                    vec![("No backlinks".to_string(), Style::fg(th.faint).italic(), 0)]
                } else {
                    let mut v = vec![];
                    for (p, _, text) in bl {
                        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        v.push((format!("{stem} · {}", crate::markdown::inline_text(&text)), Style::fg(th.fg), 0));
                    }
                    v
                }
            }
            _ => self.vault.tags().into_iter().map(|(t, n)| (format!("#{t}  {n}"), Style::fg(th.tag_fg), 0)).collect(),
        };
        let vis = r.h.saturating_sub(2);
        if self.focus == Focus::Panel {
            if self.right_sel < self.right_scroll {
                self.right_scroll = self.right_sel;
            } else if self.right_sel >= self.right_scroll + vis {
                self.right_scroll = self.right_sel + 1 - vis;
            }
        }
        self.right_scroll = self.right_scroll.min(items.len().saturating_sub(1));
        for (k, (t, st, ind)) in items.iter().enumerate().skip(self.right_scroll).take(vis) {
            let y = r.y + 2 + k - self.right_scroll;
            let mut st = st.with_bg(bg.bg);
            if self.focus == Focus::Panel && k == self.right_sel {
                st = st.with_bg(th.selection);
                if !th.rgb {
                    st = st.reverse();
                }
                cv.fill(r.x, y, r.w, 1, st);
            }
            self.text(cv, (r.x + 1 + ind) as i32, y as i32, t, st, r.x + r.w - 1);
        }
    }

    fn draw_scrollbar(&mut self, cv: &mut Canvas) {
        let r = self.editor_rect;
        let pos_now = (self.cur, self.tabs[self.cur].view.top_line, self.tabs[self.cur].view.top_off);
        if pos_now != self.scroll_seen.0 {
            self.scroll_seen = (pos_now, Some(std::time::Instant::now()));
        }
        // overlay-style: only visible for a moment after the view scrolls
        if !self.scroll_seen.1.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(1200)) {
            return;
        }
        let t = &self.tabs[self.cur];
        let n = t.ed.buf.len();
        if n <= r.h || r.h < 4 {
            return;
        }
        let th = &self.theme;
        let thumb = (r.h * r.h / n).clamp(1, r.h);
        let pos = ((t.view.top_line as f64 / n.saturating_sub(1).max(1) as f64) * (r.h - thumb) as f64).round() as usize;
        let x = r.x + (r.w).saturating_sub(1);
        let col = if th.rgb { th.border } else { th.faint };
        for k in 0..thumb {
            cv.put(x as i32, (r.y + pos + k) as i32, "▕", Style::fg(col).with_bg(th.bg), self.w);
        }
    }

    /// Obsidian's status bar: right-aligned "N backlinks  N words  N characters".
    fn draw_status(&mut self, cv: &mut Canvas, nmatch: usize, cur_match: Option<usize>) {
        let th = self.theme.clone();
        let y = self.h - 1;
        let bg = self.chrome_bg();
        cv.fill(0, y, self.w, 1, bg.with_fg(th.muted));
        let (words, chars) = self.tabs[self.cur].ed.word_count();
        let backlinks = if self.tabs[self.cur].ed.path.is_some() { Some(self.current_backlinks().len()) } else { None };
        let autosave = self.config.autosave;
        let t = &self.tabs[self.cur];
        let mut parts: Vec<(String, Style)> = vec![];
        let muted = Style::fg(th.muted).with_bg(bg.bg);
        if let Some((a, b)) = t.ed.selection() {
            parts.push((format!("{} selected", t.ed.buf.get(a, b).chars().count()), muted));
        }
        if nmatch > 0 {
            parts.push((format!("{} of {nmatch}", cur_match.map_or(0, |m| m + 1)), muted));
        }
        if let Some(n) = backlinks {
            parts.push((format!("{n} backlink{}", if n == 1 { "" } else { "s" }), muted));
        }
        let fmt = |n: usize| {
            let s = n.to_string();
            let mut out = String::new();
            for (i, c) in s.chars().enumerate() {
                if i > 0 && (s.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(c);
            }
            out
        };
        parts.push((format!("{} word{}", fmt(words), if words == 1 { "" } else { "s" }), muted));
        parts.push((format!("{} character{}", fmt(chars), if chars == 1 { "" } else { "s" }), muted));
        if t.view.mode == Mode::Source {
            let c = t.ed.cursor;
            let col = crate::text::str_width(&t.ed.line(c.line)[..c.col]) + 1;
            parts.push((format!("Ln {}, Col {col}", c.line + 1), muted));
        }
        if !autosave {
            if t.ed.dirty() {
                parts.push(("● unsaved".into(), Style::fg(th.accent).with_bg(bg.bg)));
            } else if t.ed.path.is_some() {
                parts.push(("✓ saved".into(), muted));
            }
        }
        match t.view.mode {
            Mode::Read => parts.push(("Reading".into(), Style::fg(th.accent).bold().with_bg(bg.bg))),
            Mode::Source => parts.push(("Source".into(), Style::fg(th.accent).bold().with_bg(bg.bg))),
            Mode::Live => {}
        }
        let gap = if self.ui_prop() { 2 } else { 3 };
        let mut total = 2 + gap * parts.len().saturating_sub(1);
        for p in &parts {
            total += self.text_w(&p.0, p.1);
        }
        let rx = self.w.saturating_sub(total);
        let mut x = rx as i32 + 1;
        for (k, (t, st)) in parts.iter().enumerate() {
            if k > 0 {
                x += gap as i32;
            }
            x = self.text(cv, x, y as i32, t, *st, self.w);
        }
        if let Some((m, _, err)) = self.status.clone() {
            let st = if err { Style::fg(th.red).bold() } else { Style::fg(th.fg) };
            self.text(cv, 1, y as i32, &m, st.with_bg(bg.bg), rx.saturating_sub(1));
        }
    }

    fn draw_find(&mut self, cv: &mut Canvas, nmatch: usize, cur_match: Option<usize>) {
        let th = self.theme.clone();
        let Some(f) = self.find.as_ref().map(|f| crate::app::FindView {
            query: f.query.clone(),
            replace: f.replace.clone(),
            replacing: f.replacing,
            field: f.field,
            case: f.case,
        }) else {
            return;
        };
        let f = &f;
        let rows = if f.replacing { 2 } else { 1 };
        let y0 = self.h.saturating_sub(1 + rows);
        let bg = Style::bg(if th.rgb { th.panel } else { Color::Default });
        cv.fill(0, y0, self.w, rows, bg);
        let label_w = 10;
        let count = if f.query.text.is_empty() {
            String::new()
        } else if nmatch == 0 {
            "no results".into()
        } else {
            format!("{} of {}", cur_match.map_or(0, |m| m + 1), nmatch)
        };
        let case = if f.case { "[Aa]" } else { " Aa " };
        let fields: Vec<(&str, &crate::app::LineInput)> =
            if f.replacing { vec![(" Find", &f.query), (" Replace", &f.replace)] } else { vec![(" Find", &f.query)] };
        let mut cur = None;
        let mut cursor_hidden = false;
        let fields: Vec<(&str, crate::app::LineInput)> = fields.into_iter().map(|(l, i)| (l, i.clone())).collect();
        let case_on = f.case;
        let _ = case_on;
        for (i, (lab, inp)) in fields.iter().enumerate() {
            let y = y0 + i;
            let active = i as u8 == f.field;
            self.text(cv, 1, y as i32, lab.trim(), Style::fg(if active { th.accent } else { th.muted }).bold().with_bg(bg.bg), label_w);
            let fw = self.w.saturating_sub(label_w + 28).max(10);
            let field_bg = if th.rgb { th.canvas_or(th.bg) } else { Color::Default };
            cv.fill(label_w, y, fw, 1, Style::bg(field_bg));
            let (inp_text, inp_cur) = (inp.text.clone(), inp.cur);
            if active {
                let (cx0, cy0, ch0) = (cv.cursor, cv.cursor_hidden, cv.cursor_shape);
                self.text_input(cv, label_w, y, &inp_text, inp_cur, Style::fg(th.fg).with_bg(field_bg), label_w + fw);
                cur = cv.cursor;
                cursor_hidden = cv.cursor_hidden;
                (cv.cursor, cv.cursor_hidden, cv.cursor_shape) = (cx0, cy0, ch0);
            } else {
                self.text(cv, label_w as i32, y as i32, &inp_text, Style::fg(th.fg).with_bg(field_bg), label_w + fw);
            }
            if i == 0 {
                let info = format!("{count}  {case}");
                self.text(
                    cv,
                    (label_w + fw + 2) as i32,
                    y as i32,
                    &info,
                    Style::fg(if nmatch == 0 && !f.query.text.is_empty() { th.red } else { th.muted }).with_bg(bg.bg),
                    self.w,
                );
            } else {
                self.text(cv, (label_w + fw + 2) as i32, y as i32, "↵ one  Alt+↵ all", Style::fg(th.faint).with_bg(bg.bg), self.w);
            }
        }
        if self.overlay.is_none() {
            cv.cursor = cur;
            cv.cursor_hidden = cursor_hidden;
            cv.cursor_shape = 6;
        }
    }

    fn draw_completion(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let Some(c) = self.completion.as_ref().map(|c| (c.items.clone(), c.sel, c.query.clone())) else { return };
        let c = crate::app::CompletionView { items: c.0, sel: c.1, query: c.2 };
        let Some((cx, cy)) = cv.cursor else { return };
        // (name, folder) like Obsidian's link suggester
        let items: Vec<(String, String)> = if c.items.is_empty() {
            vec![(format!("Create “{}”", c.query), String::new())]
        } else {
            c.items
                .iter()
                .map(|it| match it.rfind('/') {
                    Some(i) => (it[i + 1..].to_string(), it[..i].to_string()),
                    None => (it.clone(), String::new()),
                })
                .collect()
        };
        let w = items.iter().map(|(n, d)| str_width(n) + if d.is_empty() { 0 } else { str_width(d) + 2 }).max().unwrap_or(10).clamp(18, 56) + 4;
        let edges = if th.rgb { 2 } else { 0 };
        let h = items.len() + edges;
        let below = cy + 1 + h < self.h.saturating_sub(1);
        let y0 = if below { cy + 1 } else { cy.saturating_sub(h) };
        let x0 = cx.saturating_sub(2).min(self.w.saturating_sub(w + 1));
        let r = Rect { x: x0, y: y0, w: w.min(self.w), h };
        let top = if th.rgb { self.draw_box(cv, r, "") } else { y0 };
        let panel = if th.rgb { th.panel } else { Color::Default };
        for (k, (name, dir)) in items.iter().enumerate() {
            let sel = k == c.sel && !c.items.is_empty();
            let rbg = if sel { th.hover } else { panel };
            let mut st = Style::fg(if c.items.is_empty() { th.faint } else { th.fg }).with_bg(rbg);
            if sel && !th.rgb {
                st = st.reverse();
            }
            let y = top + k;
            cv.fill(r.x + 1, y, r.w.saturating_sub(2), 1, Style::bg(rbg));
            let x = self.text(cv, r.x as i32 + 2, y as i32, name, st, (r.x + r.w).saturating_sub(1));
            if !dir.is_empty() {
                self.text(cv, x + 2, y as i32, dir, Style::fg(th.faint).with_bg(rbg), (r.x + r.w).saturating_sub(1));
            }
        }
    }

    /// A floating panel. With known colours it is a tinted panel with
    /// rounded half-height edges (like Obsidian's modals); otherwise a box.
    /// Returns the first content row.
    fn draw_box(&mut self, cv: &mut Canvas, r: Rect, title: &str) -> usize {
        let th = self.theme.clone();
        if r.h < 2 || r.w < 4 {
            return r.y;
        }
        if th.rgb {
            let bg = th.panel;
            cv.fill(r.x, r.y + 1, r.w, r.h - 2, Style::bg(bg));
            let inner = r.w - 2;
            cv.put(r.x as i32, r.y as i32, &format!("▗{}▖", "▄".repeat(inner)), Style::fg(bg), self.w);
            cv.put(r.x as i32, (r.y + r.h - 1) as i32, &format!("▝{}▘", "▀".repeat(inner)), Style::fg(bg), self.w);
            if !title.is_empty() {
                self.text(cv, r.x as i32 + 3, r.y as i32 + 1, title, Style::fg(th.muted).with_bg(bg).bold(), r.x + r.w - 1);
                return r.y + 2;
            }
            return r.y + 1;
        }
        let bst = Style::fg(th.border);
        cv.fill(r.x, r.y, r.w, r.h, Style::new());
        let horiz = "─".repeat(r.w.saturating_sub(2));
        cv.put(r.x as i32, r.y as i32, &format!("╭{horiz}╮"), bst, self.w);
        cv.put(r.x as i32, (r.y + r.h - 1) as i32, &format!("╰{horiz}╯"), bst, self.w);
        for y in r.y + 1..r.y + r.h - 1 {
            cv.put(r.x as i32, y as i32, "│", bst, self.w);
            cv.put((r.x + r.w - 1) as i32, y as i32, "│", bst, self.w);
        }
        if !title.is_empty() {
            cv.put(r.x as i32 + 2, r.y as i32, &format!(" {title} "), Style::fg(th.muted).bold(), self.w);
        }
        r.y + 1
    }

    /// A key binding rendered as a keycap chip.
    fn chip(&mut self, cv: &mut Canvas, x: i32, y: usize, k: &str, bg: Color, clip: usize) -> i32 {
        let th = self.theme.clone();
        let chip_bg = if bg == th.hover { th.border } else { th.hover };
        let st = if th.rgb { Style::fg(th.muted).with_bg(chip_bg) } else { Style::fg(th.muted) };
        let x = cv.put(x, y as i32, " ", st, clip);
        let x = self.text(cv, x, y as i32, k, st, clip);
        let x = cv.put(x, y as i32, " ", st, clip);
        cv.put(x, y as i32, " ", Style::bg(bg), clip)
    }

    /// Obsidian-style prompt: input with placeholder, results, key-hint footer.
    /// rows: (label, dim detail, key hint, matched char indices, label style)
    #[allow(clippy::too_many_arguments)]
    fn draw_list_overlay(
        &mut self,
        cv: &mut Canvas,
        placeholder: &str,
        input: &crate::app::LineInput,
        rows: Vec<(String, String, String, Vec<usize>, Style)>,
        sel: usize,
        empty: &str,
        footer: &[(&str, &str)],
    ) {
        let th = self.theme.clone();
        let w = self.w.saturating_sub(4).min(78).max(20).min(self.w);
        if w < 12 || self.h < 8 {
            return; // too small to show a prompt; keys still work
        }
        let max_items = self.h.saturating_sub(10).clamp(1, 12);
        let n = rows.len().min(max_items);
        let h = 6 + n.max(1);
        let r = Rect { x: (self.w - w) / 2, y: (self.h / 8).max(1).min(self.h.saturating_sub(h)), w, h };
        let iy = self.draw_box(cv, r, "");
        self.overlay_hits.push((r, usize::MAX));
        let bg = if th.rgb { th.panel } else { Color::Default };
        let right = r.x + r.w - 2;
        // input row
        let ix = r.x + 3;
        let iw = r.w.saturating_sub(6);
        if input.text.is_empty() {
            self.text(cv, ix as i32, iy as i32, placeholder, Style::fg(th.faint).with_bg(bg), right);
        } else {
            self.text_input(cv, ix, iy, &input.text, input.cur, Style::fg(th.fg).with_bg(bg), right);
        }
        if input.text.is_empty() {
            cv.cursor = Some((ix, iy));
            cv.cursor_hidden = false;
        }
        let _ = iw;
        cv.cursor_shape = 6;
        let sep = "─".repeat(r.w.saturating_sub(2));
        let sep_st = Style::fg(if th.rgb { th.hover } else { th.border }).with_bg(bg);
        cv.put(r.x as i32 + 1, (iy + 1) as i32, &sep, sep_st, r.x + r.w - 1);
        let list_y = iy + 2;
        if rows.is_empty() {
            self.text(cv, ix as i32, list_y as i32, empty, Style::fg(th.faint).with_bg(bg), right);
        }
        let start = if sel >= n { sel + 1 - n } else { 0 };
        for (k, (label, detail, hint, m, st)) in rows.iter().enumerate().skip(start).take(n) {
            let y = list_y + k - start;
            let selected = k == sel;
            let rbg = if selected { th.hover } else { bg };
            let mut base = st.with_bg(rbg);
            if selected && !th.rgb {
                base = base.reverse();
            }
            cv.fill(r.x + 1, y, r.w.saturating_sub(2), 1, Style::bg(rbg));
            let hw = if hint.is_empty() { 0 } else { str_width(hint) + 3 };
            let avail = r.w.saturating_sub(6 + hw);
            // label runs, fuzzy-matched characters in accent
            let mut runs: Vec<(String, Style)> = vec![];
            for (ci, ch) in label.chars().enumerate() {
                let s = if m.contains(&ci) { base.with_fg(th.accent).bold() } else { base };
                match runs.last_mut() {
                    Some(r) if r.1 == s => r.0.push(ch),
                    _ => runs.push((ch.to_string(), s)),
                }
            }
            let x = self.text_runs(cv, ix as i32, y as i32, &runs, ix + avail) as usize;
            if !detail.is_empty() && x + 3 < ix + avail {
                self.text(cv, x as i32 + 2, y as i32, detail, Style::fg(th.faint).with_bg(rbg), ix + avail);
            }
            if !hint.is_empty() {
                let hw = self.text_w(hint, Style::fg(th.muted));
                self.chip(cv, right as i32 - hw as i32 - 2, y, hint, rbg, right + 1);
            }
            self.overlay_hits.insert(0, (Rect { x: r.x + 1, y, w: r.w.saturating_sub(2), h: 1 }, k));
        }
        // footer with key hints
        let fy = r.y + r.h - 2;
        cv.put(r.x as i32 + 1, fy as i32 - 1, &sep, sep_st, r.x + r.w - 1);
        let mut x = ix as i32;
        for (k, what) in footer {
            x = self.text_runs(
                cv,
                x,
                fy as i32,
                &[(k.to_string(), Style::fg(th.muted).with_bg(bg).bold()), (format!(" {what}"), Style::fg(th.faint).with_bg(bg))],
                right,
            ) + 3;
        }
    }

    fn draw_overlay(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let Some(ov) = self.overlay.take() else { return };
        match &ov {
            Overlay::Palette { input, sel } => {
                let rows = self
                    .palette_items(&input.text)
                    .into_iter()
                    .map(|(c, m)| {
                        (
                            commands::title(c).to_string(),
                            String::new(),
                            commands::keys_of(c).first().map(|k| commands::pretty(k)).unwrap_or_default(),
                            m,
                            Style::fg(th.fg),
                        )
                    })
                    .collect();
                self.draw_list_overlay(
                    cv,
                    "Select a command...",
                    input,
                    rows,
                    *sel,
                    "No commands found.",
                    &[("↑↓", "to navigate"), ("↵", "to use"), ("esc", "to dismiss")],
                );
            }
            Overlay::Switcher { input, sel } => {
                let rows = self
                    .switcher_items(&input.text)
                    .into_iter()
                    .map(|(l, p, m)| {
                        if p.is_none() {
                            return (l, String::new(), "Enter".to_string(), vec![], Style::fg(th.accent));
                        }
                        // name first, folder dimmed after it (Obsidian's switcher)
                        let l = l.strip_suffix(".md").map(String::from).unwrap_or(l);
                        let (dir, name) = match l.rfind('/') {
                            Some(i) => (l[..i].to_string(), l[i + 1..].to_string()),
                            None => (String::new(), l.clone()),
                        };
                        let off = l.chars().count() - name.chars().count();
                        let m: Vec<usize> = m.into_iter().filter(|&i| i >= off).map(|i| i - off).collect();
                        (name, dir, String::new(), m, Style::fg(th.fg))
                    })
                    .collect();
                self.draw_list_overlay(
                    cv,
                    "Find or create a note...",
                    input,
                    rows,
                    *sel,
                    "No notes found.",
                    &[("↑↓", "to navigate"), ("↵", "to open"), ("esc", "to dismiss")],
                );
            }
            Overlay::Search { input, sel, results, dirty } => {
                let rows = results
                    .iter()
                    .map(|(p, ln, _, text)| {
                        let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        (stem, crate::markdown::inline_text(text.trim()), format!("{}", ln + 1), vec![], Style::fg(th.fg).bold())
                    })
                    .collect();
                let empty = if input.text.is_empty() {
                    "Type to search all notes."
                } else if *dirty {
                    "Press Enter to search."
                } else {
                    "No matches."
                };
                self.draw_list_overlay(cv, "Search all notes...", input, rows, *sel, empty, &[("↑↓", "to navigate"), ("↵", "to open"), ("esc", "to dismiss")]);
            }
            Overlay::Prompt { title, input, .. } => {
                let w = self.w.saturating_sub(4).min(60).max(20).min(self.w);
                let r = Rect { x: (self.w - w) / 2, y: (self.h / 4).max(1), w, h: 4 };
                let y = self.draw_box(cv, r, title);
                self.overlay_hits.push((r, usize::MAX));
                let bg = if th.rgb { th.panel } else { Color::Default };
                let iw = w.saturating_sub(6);
                let _ = iw;
                self.text_input(cv, r.x + 3, y, &input.text, input.cur, Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
            }
            Overlay::Confirm { msg, .. } => {
                let w = (str_width(msg) + 6).min(self.w.saturating_sub(2));
                let r = Rect { x: (self.w - w) / 2, y: (self.h / 3).max(1), w, h: 3 };
                let y = self.draw_box(cv, r, "");
                self.overlay_hits.push((r, usize::MAX));
                let bg = if th.rgb { th.panel } else { Color::Default };
                cv.put(r.x as i32 + 3, y as i32, &truncate(msg, (w).saturating_sub(4)), Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
                cv.cursor = None;
            }
            Overlay::Help { scroll } => {
                let w = self.w.saturating_sub(4).min(84).max(30).min(self.w);
                let h = self.h.saturating_sub(4).max(6).min(self.h);
                let r = Rect { x: (self.w - w) / 2, y: 2.min(self.h.saturating_sub(h)), w, h };
                let y0 = self.draw_box(cv, r, "Hotkeys  ·  esc to close");
                self.overlay_hits.push((r, usize::MAX));
                let bg = if th.rgb { th.panel } else { Color::Default };
                let mut lines: Vec<(String, String)> = vec![];
                for (c, t, keys) in commands::COMMANDS {
                    let _ = c;
                    let k = keys.iter().map(|k| commands::pretty(k)).collect::<Vec<_>>().join(", ");
                    lines.push((t.to_string(), k));
                }
                lines.push((String::new(), String::new()));
                for (t, k) in [
                    ("Move / select", "Arrows, Shift+Arrows, Ctrl+Arrows, Home/End, PgUp/PgDn"),
                    ("Smart lists", "Enter continues lists/quotes/tasks; Enter on empty item ends it"),
                    ("Tables", "Tab / Shift+Tab move between cells and auto-format"),
                    ("Links", "Ctrl+Click or Alt+Enter follows; [[ autocompletes notes"),
                    ("Mouse", "Click, drag, double-click word, triple-click line, click ☐ toggles"),
                    (
                        "Headings",
                        match self.heading_mode {
                            HeadingMode::Osc66 => "rendered with kitty text sizing (OSC 66)",
                            HeadingMode::Image => "rendered as images (kitty graphics)",
                            HeadingMode::Plain => "plain (no OSC 66 / graphics support)",
                        },
                    ),
                ] {
                    lines.push((t.to_string(), k.to_string()));
                }
                let vis = (r.y + h).saturating_sub(y0 + 1);
                let max_scroll = lines.len().saturating_sub(vis);
                let s = (*scroll).min(max_scroll);
                if let Some(Overlay::Help { scroll }) = Some(&ov) {
                    let _ = scroll;
                }
                for (k, (t, keys)) in lines.iter().skip(s).take(vis).enumerate() {
                    let y = y0 + k;
                    self.text(cv, r.x as i32 + 3, y as i32, t, Style::fg(th.fg).with_bg(bg), r.x + w / 2);
                    self.text(cv, (r.x + w / 2 + 1) as i32, y as i32, keys, Style::fg(th.accent).with_bg(bg), r.x + (r.w).saturating_sub(1));
                }
                cv.cursor = None;
            }
        }
        self.overlay = Some(ov);
    }
}
