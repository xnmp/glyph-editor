//! Drawing of everything around the editor pane: tab bar, file explorer,
//! outline/backlinks/tags panel, status bar, find bar, popups and overlays.

use crate::app::{App, Focus, Overlay};
use crate::commands;
use crate::layout::{HeadingMode, Mode};
use crate::screen::Canvas;
use crate::style::{Color, Style};
use crate::text::{pad_to, str_width, truncate};
use crate::view::Rect;

impl App {
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
        self.draw_tabbar(&mut cv);
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

    fn draw_tabbar(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let bar = self.chrome_bg().with_fg(th.muted);
        cv.fill(0, 0, self.w, 1, bar);
        self.tab_hits.clear();
        let mut x = 1usize;
        let n = self.tabs.len();
        for i in 0..n {
            let name = self.tab_name(i);
            let active = i == self.cur;
            let dirty = self.tabs[i].ed.dirty();
            let label = format!(" {} ", truncate(&name, 24));
            let lw = str_width(&label);
            let close = if dirty { "●" } else { "×" };
            let total = lw + 2;
            if x + total + 1 >= self.w.saturating_sub(12) && i > self.cur {
                cv.put(x as i32, 0, "…", bar, self.w);
                break;
            }
            let st = if active { Style::fg(th.fg).with_bg(if th.rgb { th.canvas_or(th.bg) } else { Color::Default }).bold() } else { bar };
            let st = if active && !th.rgb { st.reverse() } else { st };
            cv.put(x as i32, 0, &label, st, self.w);
            let close_st = if dirty { Style::fg(th.accent).with_bg(st.bg) } else { Style::fg(th.faint).with_bg(st.bg) };
            cv.put((x + lw) as i32, 0, close, if active && !th.rgb { close_st.reverse() } else { close_st }, self.w);
            cv.put((x + lw + 1) as i32, 0, " ", st, self.w);
            self.tab_hits.push((x, x + lw, i, false));
            self.tab_hits.push((x + lw, x + lw + 2, i, true));
            x += total;
            cv.put(x as i32, 0, "│", Style::fg(th.border).with_bg(bar.bg), self.w);
            x += 1;
        }
        // right side: vault name
        let vname = self.vault.root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let vn = format!(" {} ", truncate(&vname, 20));
        let vx = self.w.saturating_sub(str_width(&vn) + 1);
        if vx > x + 2 {
            cv.put(vx as i32, 0, &vn, Style::fg(th.faint).with_bg(bar.bg), self.w);
        }
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
        cv.put(r.x as i32 + 2, r.y as i32, &truncate(&head, r.w.saturating_sub(3)), Style::fg(th.fg).with_bg(bg.bg).bold(), r.x + r.w);
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
            cv.put(
                x,
                y as i32,
                &truncate(&label, (r.x + r.w).saturating_sub(x as usize + 1)),
                if focused { st.with_fg(th.accent) } else { st },
                (r.x + r.w).saturating_sub(1),
            );
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
            let x = r.x + i * third + (third.saturating_sub(n.len())) / 2;
            cv.put(x as i32, r.y as i32, n, st.with_bg(bg.bg), r.x + r.w);
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
            cv.put((r.x + 1 + ind) as i32, y as i32, &truncate(t, r.w.saturating_sub(2 + ind)), st, r.x + r.w);
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
        let total: usize = parts.iter().map(|p| str_width(&p.0)).sum::<usize>() + 3 * parts.len().saturating_sub(1) + 2;
        let rx = self.w.saturating_sub(total);
        let mut x = rx as i32 + 1;
        for (k, (t, st)) in parts.iter().enumerate() {
            if k > 0 {
                x += 3;
            }
            x = cv.put(x, y as i32, t, *st, self.w);
        }
        if let Some((m, _, err)) = &self.status {
            let st = if *err { Style::fg(th.red).bold() } else { Style::fg(th.fg) };
            cv.put(1, y as i32, &truncate(m, rx.saturating_sub(2)), st.with_bg(bg.bg), rx);
        }
    }

    fn draw_find(&mut self, cv: &mut Canvas, nmatch: usize, cur_match: Option<usize>) {
        let th = self.theme.clone();
        let Some(f) = &self.find else { return };
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
        for (i, (lab, inp)) in fields.iter().enumerate() {
            let y = y0 + i;
            let active = i as u8 == f.field;
            cv.put(0, y as i32, lab, Style::fg(if active { th.accent } else { th.muted }).bold().with_bg(bg.bg), self.w);
            let fw = self.w.saturating_sub(label_w + 28).max(10);
            let txt = truncate(&inp.text, fw);
            cv.put(label_w as i32, y as i32, &pad_to(&txt, fw), Style::fg(th.fg).with_bg(if th.rgb { th.canvas_or(th.bg) } else { Color::Default }), self.w);
            if active {
                cur = Some((label_w + str_width(&inp.text[..inp.cur]).min(fw), y));
            }
            if i == 0 {
                let info = format!("{count}  {case}");
                cv.put(
                    (label_w + fw + 2) as i32,
                    y as i32,
                    &info,
                    Style::fg(if nmatch == 0 && !f.query.text.is_empty() { th.red } else { th.muted }).with_bg(bg.bg),
                    self.w,
                );
            } else {
                cv.put((label_w + fw + 2) as i32, y as i32, "↵ one  Alt+↵ all", Style::fg(th.faint).with_bg(bg.bg), self.w);
            }
        }
        if self.overlay.is_none() {
            cv.cursor = cur;
            cv.cursor_shape = 6;
        }
    }

    fn draw_completion(&mut self, cv: &mut Canvas) {
        let th = self.theme.clone();
        let Some(c) = &self.completion else { return };
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
            let x = cv.put(r.x as i32 + 2, y as i32, &truncate(name, r.w.saturating_sub(4)), st, (r.x + r.w).saturating_sub(1));
            if !dir.is_empty() {
                cv.put(
                    x + 2,
                    y as i32,
                    &truncate(dir, (r.x + r.w).saturating_sub(x as usize + 4)),
                    Style::fg(th.faint).with_bg(rbg),
                    (r.x + r.w).saturating_sub(1),
                );
            }
        }
    }

    /// A floating panel. With known colours it is a tinted panel with
    /// rounded half-height edges (like Obsidian's modals); otherwise a box.
    /// Returns the first content row.
    fn draw_box(&self, cv: &mut Canvas, r: Rect, title: &str) -> usize {
        let th = &self.theme;
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
                cv.put(r.x as i32 + 3, r.y as i32 + 1, title, Style::fg(th.muted).with_bg(bg).bold(), r.x + r.w - 1);
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
    fn chip(&self, cv: &mut Canvas, x: i32, y: usize, k: &str, bg: Color, clip: usize) -> i32 {
        let th = &self.theme;
        let st = if th.rgb { Style::fg(th.muted).with_bg(th.hover) } else { Style::fg(th.muted) };
        let x = cv.put(x, y as i32, &format!(" {k} "), st, clip);
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
            cv.put(ix as i32, iy as i32, placeholder, Style::fg(th.faint).with_bg(bg), right);
        } else {
            cv.put(ix as i32, iy as i32, &truncate(&input.text, iw), Style::fg(th.fg).with_bg(bg), right);
        }
        cv.cursor = Some((ix + str_width(&input.text[..input.cur]).min(iw), iy));
        cv.cursor_shape = 6;
        let sep = "─".repeat(r.w.saturating_sub(2));
        let sep_st = Style::fg(if th.rgb { th.hover } else { th.border }).with_bg(bg);
        cv.put(r.x as i32 + 1, (iy + 1) as i32, &sep, sep_st, r.x + r.w - 1);
        let list_y = iy + 2;
        if rows.is_empty() {
            cv.put(ix as i32, list_y as i32, empty, Style::fg(th.faint).with_bg(bg), right);
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
            let shown = truncate(label, avail);
            let mut x = ix;
            for (ci, ch) in shown.chars().enumerate() {
                let s = if m.contains(&ci) { base.with_fg(th.accent).bold() } else { base };
                x = cv.put(x as i32, y as i32, &ch.to_string(), s, right) as usize;
            }
            if !detail.is_empty() && x + 3 < ix + avail {
                let d = truncate(detail, (ix + avail).saturating_sub(x + 2));
                cv.put(x as i32 + 2, y as i32, &d, Style::fg(th.faint).with_bg(rbg), right);
            }
            if !hint.is_empty() {
                self.chip(cv, right as i32 - str_width(hint) as i32 - 2, y, hint, rbg, right + 1);
            }
            self.overlay_hits.insert(0, (Rect { x: r.x + 1, y, w: r.w.saturating_sub(2), h: 1 }, k));
        }
        // footer with key hints
        let fy = r.y + r.h - 2;
        cv.put(r.x as i32 + 1, fy as i32 - 1, &sep, sep_st, r.x + r.w - 1);
        let mut x = ix as i32;
        for (k, what) in footer {
            x = cv.put(x, fy as i32, k, Style::fg(th.muted).with_bg(bg).bold(), right);
            x = cv.put(x + 1, fy as i32, what, Style::fg(th.faint).with_bg(bg), right) + 3;
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
                    &[("↑↓", "to navigate"), ("↵", "to open"), ("ctrl ↵", "new tab"), ("esc", "to dismiss")],
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
                cv.put(r.x as i32 + 3, y as i32, &truncate(&input.text, iw), Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
                cv.cursor = Some((r.x + 3 + str_width(&input.text[..input.cur]).min(iw), y));
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
                    cv.put(r.x as i32 + 3, y as i32, &truncate(t, w / 2), Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
                    cv.put(
                        (r.x + w / 2 + 1) as i32,
                        y as i32,
                        &truncate(keys, (w / 2).saturating_sub(3)),
                        Style::fg(th.accent).with_bg(bg),
                        r.x + (r.w).saturating_sub(1),
                    );
                }
                cv.cursor = None;
            }
        }
        self.overlay = Some(ov);
    }
}
