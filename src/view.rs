//! The editor pane: scrolling, drawing visual rows onto the canvas, cursor
//! placement, selection/search highlighting, images, and hit-testing.

use std::rc::Rc;
use std::sync::Arc;

use crate::buffer::Pos;
use crate::editor::Editor;
use crate::graphics as gfx;
use crate::layout::{self, Ctx, Layout, Mode, RowKind, VRow};
use crate::screen::{Canvas, ImageData, Placement};
use crate::style::{Color, Style};
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone)]
pub struct Hit {
    pub y: usize,
    pub line: usize,
    pub rows: Rc<Vec<VRow>>,
    pub idx: usize,
    /// rows of this visual row hidden above the pane (scaled rows partially visible)
    pub clip_top: usize,
}

impl Hit {
    pub fn row(&self) -> &VRow {
        &self.rows[self.idx]
    }
}

pub struct View {
    /// first visible line, and how many screen rows of it are scrolled away
    pub top_line: usize,
    pub top_off: usize,
    pub mode: Mode,
    pub hits: Vec<Hit>,
    pub rect: Rect,
    pub cx0: usize,
    pub cw: usize,
    pub readable: usize,
    pub line_numbers: bool,
}

pub struct Env<'a> {
    pub layout: &'a Layout,
    pub ctx: Ctx<'a>,
}

pub fn rows_of(env: &Env, ed: &mut Editor, i: usize) -> Rc<Vec<VRow>> {
    let cur = ed.cursor;
    let read = env.ctx.mode == Mode::Read;
    let (doc, lines) = ed.parsed();
    let active_block = doc.infos.get(cur.line).and_then(|x| x.block);
    let ba = !read && doc.infos[i].block.is_some() && doc.infos[i].block == active_block;
    let c = if i == cur.line && !read { Some(cur.col) } else { None };
    env.layout.rows(&env.ctx, lines, doc, i, c, ba)
}

pub fn line_height(env: &Env, ed: &mut Editor, i: usize) -> usize {
    rows_of(env, ed, i).iter().map(|r| r.height()).sum()
}

impl View {
    pub fn new(mode: Mode) -> View {
        View { top_line: 0, top_off: 0, mode, hits: vec![], rect: Rect::default(), cx0: 0, cw: 80, readable: 100, line_numbers: false }
    }

    /// Content width for a pane of width `w` (readable line length, centered).
    pub fn content_width(&self, w: usize) -> usize {
        let gutter = if self.line_numbers { 6 } else { 0 };
        let margin = if w > 60 { 4 } else if w > 30 { 2 } else { 1 };
        w.saturating_sub(2 * margin + gutter).min(self.readable).max(8)
    }

    pub fn layout_origin(&mut self, rect: Rect) {
        self.rect = rect;
        self.cw = self.content_width(rect.w);
        let gutter = if self.line_numbers { 6 } else { 0 };
        self.cx0 = rect.x + gutter + (rect.w.saturating_sub(gutter + self.cw)) / 2;
    }

    // ------------------------------------------------------------- scrolling
    pub fn scroll(&mut self, env: &Env, ed: &mut Editor, delta: isize) {
        let n = ed.buf.len();
        if delta > 0 {
            let mut left = delta as usize;
            loop {
                let h = line_height(env, ed, self.top_line);
                let rem = h.saturating_sub(self.top_off);
                if left < rem {
                    self.top_off += left;
                    break;
                }
                if self.top_line + 1 >= n {
                    self.top_off = h.saturating_sub(1).min(self.top_off + left);
                    break;
                }
                left -= rem;
                self.top_line += 1;
                self.top_off = 0;
            }
            // don't scroll past the end: keep at least a third of the pane filled
            self.clamp_end(env, ed);
        } else {
            let mut left = (-delta) as usize;
            loop {
                if left <= self.top_off {
                    self.top_off -= left;
                    break;
                }
                left -= self.top_off;
                if self.top_line == 0 {
                    self.top_off = 0;
                    break;
                }
                self.top_line -= 1;
                self.top_off = line_height(env, ed, self.top_line);
            }
        }
    }

    fn clamp_end(&mut self, env: &Env, ed: &mut Editor) {
        let n = ed.buf.len();
        let want = self.rect.h.max(3) / 3;
        let mut acc = 0usize;
        let mut i = n;
        while i > 0 {
            i -= 1;
            acc += line_height(env, ed, i);
            if acc >= want {
                break;
            }
        }
        let (max_line, max_off) = (i, acc.saturating_sub(want));
        if (self.top_line, self.top_off) > (max_line, max_off) {
            self.top_line = max_line;
            self.top_off = max_off;
        }
    }

    /// Screen-row offset of the cursor relative to the top of the pane (may be
    /// negative or beyond the pane).
    fn cursor_offset(&self, env: &Env, ed: &mut Editor) -> isize {
        let cur = ed.cursor;
        let rows = rows_of(env, ed, cur.line);
        let ri = cursor_row_index(&rows, ed.line(cur.line), cur.col);
        let within: usize = rows[..ri].iter().map(|r| r.height()).sum();
        if cur.line >= self.top_line {
            let mut off = -(self.top_off as isize);
            for i in self.top_line..cur.line {
                off += line_height(env, ed, i) as isize;
                if off > self.rect.h as isize * 4 {
                    return off;
                }
            }
            off + within as isize
        } else {
            let mut off = -(self.top_off as isize);
            for i in (cur.line..self.top_line).rev() {
                off -= line_height(env, ed, i) as isize;
                if off < -(self.rect.h as isize) * 4 {
                    return off;
                }
            }
            off + within as isize
        }
    }

    pub fn ensure_cursor_visible(&mut self, env: &Env, ed: &mut Editor) {
        let h = self.rect.h as isize;
        if h <= 0 {
            return;
        }
        let margin = (h / 6).clamp(0, 3);
        let cur = ed.cursor;
        let rows = rows_of(env, ed, cur.line);
        let ri = cursor_row_index(&rows, ed.line(cur.line), cur.col);
        let rh = rows.get(ri).map_or(1, |r| r.height()) as isize;
        let off = self.cursor_offset(env, ed);
        if off < margin {
            let far = off < -h * 2;
            if far {
                self.top_line = cur.line;
                self.top_off = rows[..ri].iter().map(|r| r.height()).sum();
                self.scroll(env, ed, -(h / 3));
            } else {
                self.scroll(env, ed, off - margin);
            }
        } else if off + rh > h - margin {
            if off > h * 3 {
                self.top_line = cur.line;
                self.top_off = rows[..ri].iter().map(|r| r.height()).sum();
                self.scroll(env, ed, -(h * 2 / 3));
            } else {
                self.scroll(env, ed, off + rh - (h - margin));
            }
        }
    }

    pub fn center_cursor(&mut self, env: &Env, ed: &mut Editor) {
        let cur = ed.cursor;
        let rows = rows_of(env, ed, cur.line);
        let ri = cursor_row_index(&rows, ed.line(cur.line), cur.col);
        self.top_line = cur.line;
        self.top_off = rows[..ri].iter().map(|r| r.height()).sum();
        self.scroll(env, ed, -(self.rect.h as isize / 3));
    }

    // ------------------------------------------------------------- vertical motion
    /// Move the cursor by `delta` visual rows, keeping the goal x.
    pub fn move_rows(&mut self, env: &Env, ed: &mut Editor, delta: isize, select: bool) {
        let cur = ed.cursor;
        let rows = rows_of(env, ed, cur.line);
        let line = ed.line(cur.line).to_string();
        let mut ri = cursor_row_index(&rows, &line, cur.col);
        let x = ed.goal_x.unwrap_or_else(|| layout::x_of_col(&rows[ri], &line, cur.col));
        let mut ln = cur.line;
        let mut rows = rows;
        let mut steps = delta.unsigned_abs();
        let n = ed.buf.len();
        let mut moved = false;
        while steps > 0 {
            // next cursorable row in direction
            let mut found = false;
            if delta > 0 {
                let mut k = ri + 1;
                loop {
                    while k < rows.len() && !rows[k].cursorable {
                        k += 1;
                    }
                    if k < rows.len() {
                        ri = k;
                        found = true;
                        break;
                    }
                    if ln + 1 >= n {
                        break;
                    }
                    ln += 1;
                    rows = rows_of(env, ed, ln);
                    k = 0;
                }
            } else {
                let mut k = ri as isize - 1;
                loop {
                    while k >= 0 && !rows[k as usize].cursorable {
                        k -= 1;
                    }
                    if k >= 0 {
                        ri = k as usize;
                        found = true;
                        break;
                    }
                    if ln == 0 {
                        break;
                    }
                    ln -= 1;
                    rows = rows_of(env, ed, ln);
                    k = rows.len() as isize - 1;
                }
            }
            if !found {
                // at the edge: go to line start/end
                let p = if delta > 0 { ed.buf.end() } else { Pos::new(0, 0) };
                if !moved || steps > 0 {
                    ed.set_cursor(p, select);
                }
                ed.goal_x = Some(x);
                return;
            }
            moved = true;
            steps -= 1;
        }
        let line = ed.line(ln).to_string();
        let col = layout::col_of_x(&rows[ri], &line, x);
        ed.set_cursor(Pos::new(ln, col), select);
        ed.goal_x = Some(x);
    }

    // ------------------------------------------------------------- drawing
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, cv: &mut Canvas, env: &Env, ed: &mut Editor, theme: &Theme, matches: &[(Pos, Pos)], cur_match: Option<usize>, focused: bool, placeholders: bool, cell: (u32, u32)) -> Option<(usize, usize)> {
        let rect = self.rect;
        self.hits.clear();
        let n = ed.buf.len();
        if self.top_line >= n {
            self.top_line = n - 1;
            self.top_off = 0;
        }
        let sel = ed.selection();
        let cur = ed.cursor;
        let show_cursor = focused && self.mode != Mode::Read;
        let mut cursor_at = None;
        let mut y = rect.y as isize - self.top_off as isize;
        let mut i = self.top_line;
        let bottom = (rect.y + rect.h) as isize;
        // track image placements for classic mode: (img, first visible row, count, x, y)
        let mut place: Vec<(Arc<ImageData>, u32, u32, usize, usize)> = vec![];
        while y < bottom && i < n {
            let rows = rows_of(env, ed, i);
            let line = ed.line(i).to_string();
            let cursor_ri = if i == cur.line { Some(cursor_row_index(&rows, &line, cur.col)) } else { None };
            for (ri, row) in rows.iter().enumerate() {
                let h = row.height() as isize;
                if y + h <= rect.y as isize {
                    y += h;
                    continue;
                }
                if y >= bottom {
                    break;
                }
                let clip_top = (rect.y as isize - y).max(0) as usize;
                let sy = y.max(rect.y as isize) as usize;
                self.hits.push(Hit { y: sy, line: i, rows: rows.clone(), idx: ri, clip_top });
                // line numbers
                if self.line_numbers && ri == 0 && clip_top == 0 {
                    let num = format!("{:>4} ", i + 1);
                    let st = if i == cur.line { Style::fg(theme.muted) } else { Style::fg(theme.faint) };
                    cv.put((self.cx0 as i32) - 6, sy as i32, &num, st, rect.x + rect.w);
                }
                // fill
                if let Some(f) = row.fill {
                    let x1 = row.fill_x1.map(|v| v as usize).unwrap_or(self.cw);
                    for dy in 0..row.height() {
                        let yy = y + dy as isize;
                        if yy >= rect.y as isize && yy < bottom {
                            cv.restyle(self.cx0, yy as usize, x1.min(self.cw + 1), |s| s.over(Style::bg(f.bg)));
                        }
                    }
                }
                let clip1 = self.cx0 + self.cw + 1;
                if let Some((img, irow, ix)) = &row.image {
                    let data = img.data.clone();
                    if !cv.images.iter().any(|d| d.id == data.id) {
                        cv.images.push(data.clone());
                    }
                    let x0 = self.cx0 + *ix as usize;
                    if placeholders {
                        let st = Style::fg(match gfx::id_color(data.id) {
                            (r, g, b) => Color::Rgb(r, g, b),
                        });
                        let mut s = String::new();
                        let cols = (data.cols as usize).min(self.cw.saturating_sub(*ix as usize));
                        for c in 0..cols {
                            s.push_str(&gfx::placeholder_cell(*irow as usize, c));
                        }
                        cv.put(x0 as i32, sy as i32, &s, st, clip1);
                    } else {
                        match place.last_mut() {
                            Some(p) if p.0.id == data.id && p.1 + p.2 == *irow => p.2 += 1,
                            _ => place.push((data.clone(), *irow, 1, x0, sy)),
                        }
                    }
                } else if clip_top == 0 {
                    for seg in &row.segs {
                        if seg.w == 0 {
                            continue;
                        }
                        let mut st = seg.style;
                        if let Some(Color::Default) = Some(st.bg) {
                            if let Some(f) = row.fill {
                                st.bg = f.bg;
                            }
                        }
                        let x = (self.cx0 + seg.x as usize) as i32;
                        if seg.scale > 1 {
                            cv.put_scaled(x, sy as i32, &seg.text, st, seg.scale, clip1);
                        } else {
                            cv.put(x, sy as i32, &seg.text, st, clip1);
                        }
                    }
                    if let Some(u) = row.segs.iter().find(|s| s.style.link != 0) {
                        let _ = u;
                    }
                    // selection + search highlighting
                    let paint_range = |cv: &mut Canvas, a: usize, b: usize, bg: Color| {
                        if b <= row.a && !(a == b) || a > row.b {
                            return;
                        }
                        let a = a.max(row.a);
                        let b = b.min(row.b);
                        let xa = layout::x_of_col(row, &line, a);
                        let xb = if b >= row.b && !row.last { row.end_x as usize } else { layout::x_of_col(row, &line, b) };
                        let w = xb.saturating_sub(xa);
                        if w > 0 {
                            for dy in 0..row.height() {
                                cv.restyle(self.cx0 + xa, sy + dy, w, |s| s.with_bg(bg));
                            }
                        }
                    };
                    for (mi, &(ma, mb)) in matches.iter().enumerate() {
                        if ma.line == i {
                            let bg = if Some(mi) == cur_match { theme.search_cur_bg } else { theme.search_bg };
                            paint_range(cv, ma.col, mb.col, bg);
                        }
                    }
                    if let Some((sa, sb)) = sel {
                        if sa.line <= i && i <= sb.line && row.kind == RowKind::Text {
                            let a = if sa.line == i { sa.col } else { 0 };
                            let b = if sb.line == i { sb.col } else { line.len() };
                            paint_range(cv, a, b, theme.selection);
                            // selected newline: show one cell past EOL
                            if sb.line > i && row.last {
                                let x = self.cx0 + row.end_x as usize;
                                cv.restyle(x, sy, 1, |s| s.with_bg(theme.selection));
                            }
                        }
                    }
                }
                if show_cursor && cursor_ri == Some(ri) && clip_top == 0 {
                    let cx = self.cx0 + layout::x_of_col(row, &line, cur.col);
                    if row.scale > 1 {
                        // soft cursor inside scaled text: invert the glyph cell
                        let cx = cx.min(self.cx0 + self.cw);
                        cv.restyle(cx, sy, 1, |s| s.reverse());
                        if cx < cv.w && cv.at(cx, sy).flag != crate::screen::NORMAL {
                            // cursor at end of scaled text: draw a bar
                        }
                        cursor_at = Some((cx, sy));
                        cv.cursor = None;
                    } else {
                        cursor_at = Some((cx.min(self.cx0 + self.cw), sy));
                        cv.cursor = cursor_at;
                    }
                }
                y += h;
            }
            i += 1;
        }
        if !placeholders {
            for (data, first, count, x, y) in place {
                cv.placements.push(Placement { id: data.id, x: x as u16, y: y as u16, cols: data.cols, rows: count, src_row: first, cell_w: cell.0, cell_h: cell.1 });
            }
        }
        cursor_at
    }

    /// Source position for a click at absolute (x, y); None when outside text.
    pub fn hit(&self, x: usize, y: usize, ed: &Editor) -> Option<Pos> {
        if self.hits.is_empty() {
            return None;
        }
        let mut best: Option<&Hit> = None;
        for h in &self.hits {
            if h.y <= y && y < h.y + h.row().height() {
                best = Some(h);
                break;
            }
            if h.y <= y {
                best = Some(h);
            }
        }
        let h = best?;
        // clicking a non-cursorable row: use the nearest cursorable row of that line
        let rows = &h.rows;
        let mut idx = h.idx;
        if !rows[idx].cursorable {
            idx = (0..=idx).rev().find(|&k| rows[k].cursorable).or_else(|| (idx..rows.len()).find(|&k| rows[k].cursorable)).unwrap_or(idx);
        }
        let row = &rows[idx];
        let line = ed.line(h.line);
        let rx = x.saturating_sub(self.cx0);
        if y >= h.y + h.row().height() {
            // below the last row
            return Some(Pos::new(h.line, line.len()));
        }
        if row.kind == RowKind::Image {
            return Some(Pos::new(h.line, 0));
        }
        Some(Pos::new(h.line, layout::col_of_x(row, line, rx)))
    }
}

/// Which visual row holds the cursor at `col`.
pub fn cursor_row_index(rows: &[VRow], line: &str, col: usize) -> usize {
    let _ = line;
    let mut best = None;
    for (k, r) in rows.iter().enumerate() {
        if !r.cursorable {
            continue;
        }
        if r.a <= col && (col < r.b || (col == r.b && r.last) || (col == r.b && r.a == r.b)) {
            return k;
        }
        if r.a <= col {
            best = Some(k);
        }
    }
    best.or_else(|| rows.iter().position(|r| r.cursorable)).unwrap_or(0)
}
