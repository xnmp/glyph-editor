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
        let head = format!(" {} ", self.vault.root.file_name().map(|s| s.to_string_lossy().to_uppercase()).unwrap_or_default());
        cv.put(r.x as i32, r.y as i32, &truncate(&head, r.w), Style::fg(th.muted).with_bg(bg.bg).bold(), r.x + r.w);
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
            let line = format!("{}{}{}", "  ".repeat(*depth), icon, label);
            let mut st = if *is_dir {
                Style::fg(th.muted)
            } else if is_md {
                Style::fg(th.fg)
            } else {
                Style::fg(th.faint)
            };
            if current.as_ref() == Some(p) {
                st = Style::fg(th.accent).bold();
            }
            st = st.with_bg(bg.bg);
            if self.focus == Focus::Files && k == self.tree_sel {
                st = st.with_bg(th.selection);
                if !th.rgb {
                    st = st.reverse();
                }
            }
            cv.fill(r.x, y, r.w, 1, st);
            cv.put(r.x as i32 + 1, y as i32, &truncate(&line, r.w.saturating_sub(2)), st, r.x + r.w);
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

    fn draw_status(&mut self, cv: &mut Canvas, nmatch: usize, cur_match: Option<usize>) {
        let th = self.theme.clone();
        let y = self.h - 1;
        let bg = self.chrome_bg();
        cv.fill(0, y, self.w, 1, bg.with_fg(th.muted));
        let (words, chars) = self.tabs[self.cur].ed.word_count();
        let t = &self.tabs[self.cur];
        let mode = match t.view.mode {
            Mode::Live => "Live",
            Mode::Source => "Source",
            Mode::Read => "Reading",
        };
        let c = t.ed.cursor;
        let col = crate::text::str_width(&t.ed.line(c.line)[..c.col]) + 1;
        let saved = if t.ed.dirty() {
            "● unsaved"
        } else if t.ed.path.is_some() {
            "✓ saved"
        } else {
            "untitled"
        };
        let sel = t.ed.selection().map(|(a, b)| {
            let s = t.ed.buf.get(a, b);
            format!("{} selected  ", s.chars().count())
        });
        let found = if nmatch > 0 { format!("{} of {nmatch}  ", cur_match.map_or(0, |m| m + 1)) } else { String::new() };
        let right = format!("{}{found}{words} words  {chars} chars  Ln {}, Col {col}  {saved}  {mode} ", sel.unwrap_or_default(), c.line + 1);
        let rx = self.w.saturating_sub(str_width(&right));
        let left = match &self.status {
            Some((m, _, err)) => (format!(" {m}"), if *err { Style::fg(th.red).bold() } else { Style::fg(th.fg) }),
            None => {
                let p = t.ed.path.as_ref().map(|p| self.vault.rel(p)).unwrap_or_else(|| "untitled".into());
                let hint = if self.w > 120 { "   ·  Ctrl+P commands  Ctrl+O open  F1 help" } else { "" };
                (format!(" {p}{hint}"), Style::fg(th.faint))
            }
        };
        cv.put(0, y as i32, &truncate(&left.0, rx.saturating_sub(1)), left.1.with_bg(bg.bg), rx);
        cv.put(rx as i32, y as i32, &right, Style::fg(th.muted).with_bg(bg.bg), self.w);
        // mode in accent
        let mx = self.w as i32 - str_width(mode) as i32 - 1;
        cv.put(mx, y as i32, mode, Style::fg(th.accent).bold().with_bg(bg.bg), self.w);
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
        let items = if c.items.is_empty() { vec![format!("Create “{}”", c.query)] } else { c.items.clone() };
        let w = items.iter().map(|s| str_width(s)).max().unwrap_or(10).clamp(16, 50) + 2;
        let h = items.len();
        let below = cy + 1 + h < self.h - 1;
        let y0 = if below { cy + 1 } else { cy.saturating_sub(h) };
        let x0 = cx.min(self.w.saturating_sub(w + 1));
        let panel = if th.rgb { th.panel.blend(th.fg_or(th.muted), 0.04) } else { Color::Default };
        for (k, it) in items.iter().enumerate() {
            let sel = k == c.sel && !c.items.is_empty();
            let mut st = Style::fg(if c.items.is_empty() { th.faint } else { th.fg }).with_bg(panel);
            if sel {
                st = st.with_bg(th.selection);
                if !th.rgb {
                    st = st.reverse();
                }
            }
            cv.fill(x0, y0 + k, w, 1, st);
            cv.put(x0 as i32 + 1, (y0 + k) as i32, &truncate(it, (w).saturating_sub(2)), st, x0 + w);
        }
    }

    fn draw_box(&self, cv: &mut Canvas, r: Rect, title: &str) {
        let th = &self.theme;
        let bg = if th.rgb { th.panel } else { Color::Default };
        let bst = Style::fg(th.border).with_bg(bg);
        cv.fill(r.x, r.y, r.w, r.h, Style::bg(bg));
        let horiz = "─".repeat(r.w.saturating_sub(2));
        cv.put(r.x as i32, r.y as i32, &format!("╭{horiz}╮"), bst, self.w);
        cv.put(r.x as i32, (r.y + (r.h).saturating_sub(1)) as i32, &format!("╰{horiz}╯"), bst, self.w);
        for y in r.y + 1..r.y + (r.h).saturating_sub(1) {
            cv.put(r.x as i32, y as i32, "│", bst, self.w);
            cv.put((r.x + (r.w).saturating_sub(1)) as i32, y as i32, "│", bst, self.w);
        }
        if !title.is_empty() {
            cv.put(r.x as i32 + 2, r.y as i32, &format!(" {title} "), Style::fg(th.muted).with_bg(bg).bold(), self.w);
        }
    }

    fn draw_list_overlay(
        &mut self,
        cv: &mut Canvas,
        title: &str,
        prompt: &str,
        input: &crate::app::LineInput,
        rows: Vec<(String, String, Vec<usize>, Style)>,
        sel: usize,
        empty: &str,
    ) {
        let th = self.theme.clone();
        let w = (self.w.saturating_sub(4)).min(76).max(20).min(self.w);
        let max_items = (self.h.saturating_sub(8)).clamp(3, 14);
        let n = rows.len().min(max_items);
        let h = 4 + n.max(1);
        let r = Rect { x: (self.w - w) / 2, y: (self.h / 6).max(1).min(self.h.saturating_sub(h)), w, h };
        self.draw_box(cv, r, title);
        self.overlay_hits.push((r, usize::MAX));
        let bg = if th.rgb { th.panel } else { Color::Default };
        // input
        let iy = r.y + 1;
        cv.put(r.x as i32 + 2, iy as i32, prompt, Style::fg(th.accent).bold().with_bg(bg), self.w);
        let ix = r.x + 2 + str_width(prompt) + 1;
        let iw = r.w.saturating_sub(str_width(prompt) + 5);
        cv.put(ix as i32, iy as i32, &truncate(&input.text, iw), Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
        cv.cursor = Some((ix + str_width(&input.text[..input.cur]).min(iw), iy));
        cv.cursor_shape = 6;
        let sep = "─".repeat(r.w.saturating_sub(2));
        cv.put(r.x as i32 + 1, (iy + 1) as i32, &sep, Style::fg(th.border).with_bg(bg), self.w);
        if rows.is_empty() {
            cv.put(r.x as i32 + 3, (iy + 2) as i32, empty, Style::fg(th.faint).italic().with_bg(bg), r.x + (r.w).saturating_sub(1));
            return;
        }
        let start = if sel >= n { sel + 1 - n } else { 0 };
        for (k, (label, hint, m, st)) in rows.iter().enumerate().skip(start).take(n) {
            let y = iy + 2 + k - start;
            let selected = k == sel;
            let rbg = if selected { th.selection } else { bg };
            let mut base = st.with_bg(rbg);
            if selected && !th.rgb {
                base = base.reverse();
            }
            cv.fill(r.x + 1, y, (r.w).saturating_sub(2), 1, base);
            let hw = str_width(hint);
            let lw = r.w.saturating_sub(6 + hw);
            let shown = truncate(label, lw);
            // highlight fuzzy-matched chars
            let mut x = r.x + 3;
            for (ci, ch) in shown.chars().enumerate() {
                let hl = m.contains(&ci);
                let s = if hl { base.with_fg(th.accent).bold() } else { base };
                x = cv.put(x as i32, y as i32, &ch.to_string(), s, r.x + (r.w).saturating_sub(2)) as usize;
            }
            if !hint.is_empty() {
                cv.put((r.x + (r.w).saturating_sub(2)) as i32 - hw as i32, y as i32, hint, Style::fg(th.faint).with_bg(rbg), r.x + (r.w).saturating_sub(1));
            }
            self.overlay_hits.insert(0, (Rect { x: r.x + 1, y, w: (r.w).saturating_sub(2), h: 1 }, k));
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
                        (commands::title(c).to_string(), commands::keys_of(c).first().map(|k| commands::pretty(k)).unwrap_or_default(), m, Style::fg(th.fg))
                    })
                    .collect();
                self.draw_list_overlay(cv, "Commands", "›", input, rows, *sel, "No matching commands");
            }
            Overlay::Switcher { input, sel } => {
                let rows = self
                    .switcher_items(&input.text)
                    .into_iter()
                    .map(|(l, p, m)| {
                        let st = if p.is_none() { Style::fg(th.accent).italic() } else { Style::fg(th.fg) };
                        let label = l.strip_suffix(".md").map(String::from).unwrap_or(l);
                        (label, if p.is_none() { "↵ create".into() } else { String::new() }, m, st)
                    })
                    .collect();
                self.draw_list_overlay(cv, "Open note", "›", input, rows, *sel, "Type to search notes");
            }
            Overlay::Search { input, sel, results, dirty } => {
                let rows = results
                    .iter()
                    .map(|(p, ln, _, text)| {
                        let stem = p.with_extension("").to_string_lossy().to_string();
                        (format!("{}  {}", stem, text.trim()), format!(":{}", ln + 1), vec![], Style::fg(th.fg))
                    })
                    .collect();
                let title = if *dirty { "Search (↵ to run)" } else { "Search notes" };
                self.draw_list_overlay(cv, title, "⌕", input, rows, *sel, if input.text.is_empty() { "Type to search all notes" } else { "No results" });
            }
            Overlay::Prompt { title, input, .. } => {
                let w = self.w.saturating_sub(4).min(60).max(20).min(self.w);
                let r = Rect { x: (self.w - w) / 2, y: (self.h / 4).max(1), w, h: 3 };
                self.draw_box(cv, r, title);
                self.overlay_hits.push((r, usize::MAX));
                let bg = if th.rgb { th.panel } else { Color::Default };
                cv.put(r.x as i32 + 2, r.y as i32 + 1, "›", Style::fg(th.accent).bold().with_bg(bg), self.w);
                cv.put(
                    r.x as i32 + 4,
                    r.y as i32 + 1,
                    &truncate(&input.text, (w).saturating_sub(6)),
                    Style::fg(th.fg).with_bg(bg),
                    r.x + (r.w).saturating_sub(1),
                );
                cv.cursor = Some((r.x + 4 + str_width(&input.text[..input.cur]).min((w).saturating_sub(6)), r.y + 1));
            }
            Overlay::Confirm { msg, .. } => {
                let w = (str_width(msg) + 6).min(self.w.saturating_sub(2));
                let r = Rect { x: (self.w - w) / 2, y: (self.h / 3).max(1), w, h: 3 };
                self.draw_box(cv, r, "Confirm");
                self.overlay_hits.push((r, usize::MAX));
                let bg = if th.rgb { th.panel } else { Color::Default };
                cv.put(r.x as i32 + 3, r.y as i32 + 1, &truncate(msg, (w).saturating_sub(4)), Style::fg(th.fg).with_bg(bg), r.x + (r.w).saturating_sub(1));
                cv.cursor = None;
            }
            Overlay::Help { scroll } => {
                let w = self.w.saturating_sub(4).min(84).max(30).min(self.w);
                let h = self.h.saturating_sub(4).max(6).min(self.h);
                let r = Rect { x: (self.w - w) / 2, y: 2.min(self.h.saturating_sub(h)), w, h };
                self.draw_box(cv, r, "glyph — key bindings (Esc to close)");
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
                let vis = h.saturating_sub(2);
                let max_scroll = lines.len().saturating_sub(vis);
                let s = (*scroll).min(max_scroll);
                if let Some(Overlay::Help { scroll }) = Some(&ov) {
                    let _ = scroll;
                }
                for (k, (t, keys)) in lines.iter().skip(s).take(vis).enumerate() {
                    let y = r.y + 1 + k;
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
