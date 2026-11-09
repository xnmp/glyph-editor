//! The editing model: a buffer, a cursor/selection, and Markdown-aware
//! editing commands (list continuation, auto-pairs, wrapping, tasks, tables).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::buffer::{Buffer, Pos, Sel};
use crate::markdown::{self as md, Document, LineKind, SpanKind};
use crate::text::{self, leading_ws, next_boundary, prev_boundary};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Url(String),
    Note { target: String, anchor: String },
    Tag(String),
    Footnote(String),
}

pub struct Editor {
    pub buf: Buffer,
    pub path: Option<PathBuf>,
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    /// desired x when moving vertically
    pub goal_x: Option<usize>,
    pub saved_version: u64,
    pub crlf: bool,
    pub disk_mtime: Option<SystemTime>,
    doc: Document,
    doc_version: u64,
    wc: (u64, usize, usize, Option<std::time::Instant>),
    pub indent_unit: String,
    pub autopair: bool,
}

const PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}'), ('`', '`'), ('"', '"')];
const WRAPPERS: &[&str] = &["*", "_", "`", "=", "~", "[", "(", "\"", "$"];

impl Editor {
    pub fn new(text: &str) -> Editor {
        let crlf = text.contains("\r\n");
        let buf = Buffer::new(text);
        let indent_unit = if buf.lines.iter().any(|l| l.starts_with('\t') && !l.trim().is_empty()) { "\t".to_string() } else { "    ".to_string() };
        let doc = md::parse_document(&buf.lines);
        let mut buf = buf;
        buf.take_dirty();
        Editor { buf, path: None, cursor: Pos::default(), anchor: None, goal_x: None, saved_version: 0, crlf, disk_mtime: None, doc, doc_version: 0, wc: (u64::MAX, 0, 0, None), indent_unit, autopair: true }
    }

    pub fn open(path: &Path) -> std::io::Result<Editor> {
        let text = match std::fs::read(path) {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let mut ed = Editor::new(&text);
        ed.path = Some(path.to_path_buf());
        ed.disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(ed)
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(std::io::Error::other("no file name"));
        };
        let mut text = self.buf.text();
        if self.crlf {
            text = text.replace('\n', "\r\n");
        }
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        // atomic write: temp file + rename (preserving permissions)
        let tmp = path.with_file_name(format!(".{}.glyph-tmp", path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()));
        std::fs::write(&tmp, text.as_bytes())?;
        if let Ok(meta) = std::fs::metadata(&path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, &path)?;
        self.saved_version = self.buf.version;
        self.disk_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        self.buf.seal();
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.buf.version != self.saved_version
    }

    /// True when the file changed on disk since we loaded/saved it.
    pub fn changed_on_disk(&self) -> bool {
        match (&self.path, self.disk_mtime) {
            (Some(p), Some(t)) => std::fs::metadata(p).and_then(|m| m.modified()).map(|m| m != t).unwrap_or(false),
            _ => false,
        }
    }

    pub fn reload(&mut self) -> std::io::Result<()> {
        let Some(p) = self.path.clone() else { return Ok(()) };
        let text = String::from_utf8_lossy(&std::fs::read(&p)?).to_string();
        let cur = self.cursor;
        self.buf.set_text(&text);
        self.saved_version = self.buf.version;
        self.disk_mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        self.cursor = self.buf.clamp(cur);
        self.anchor = None;
        Ok(())
    }

    pub fn doc(&mut self) -> &Document {
        if self.doc_version != self.buf.version {
            match self.buf.take_dirty() {
                Some((s, oe, ne)) => md::parse_incremental(&mut self.doc, &self.buf.lines, s, oe, ne),
                None => self.doc = md::parse_document(&self.buf.lines),
            }
            self.doc_version = self.buf.version;
        }
        &self.doc
    }

    /// Parsed document and lines together (avoids borrow juggling).
    pub fn parsed(&mut self) -> (&Document, &[String]) {
        self.doc();
        (&self.doc, &self.buf.lines)
    }

    pub fn lines(&self) -> &[String] {
        &self.buf.lines
    }

    pub fn line(&self, i: usize) -> &str {
        &self.buf.lines[i]
    }

    // ------------------------------------------------------------- selection
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some(if a < self.cursor { (a, self.cursor) } else { (self.cursor, a) })
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(a, b)| self.buf.get(a, b))
    }

    pub fn sel(&self) -> Sel {
        (self.anchor.unwrap_or(self.cursor), self.cursor)
    }

    pub fn set_cursor(&mut self, p: Pos, select: bool) {
        let p = self.buf.clamp(p);
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = p;
    }

    pub fn select_range(&mut self, a: Pos, b: Pos) {
        self.anchor = Some(self.buf.clamp(a));
        self.cursor = self.buf.clamp(b);
    }

    fn restore(&mut self, s: Sel) {
        let (a, c) = (self.buf.clamp(s.0), self.buf.clamp(s.1));
        self.cursor = c;
        self.anchor = if a != c { Some(a) } else { None };
    }

    // ------------------------------------------------------------- motion
    pub fn left(&mut self, select: bool) {
        if !select {
            if let Some((a, _)) = self.selection() {
                self.set_cursor(a, false);
                return;
            }
        }
        let p = self.cursor;
        let np = if p.col > 0 { Pos::new(p.line, prev_boundary(self.line(p.line), p.col)) } else if p.line > 0 { Pos::new(p.line - 1, self.line(p.line - 1).len()) } else { p };
        self.set_cursor(np, select);
        self.goal_x = None;
    }

    pub fn right(&mut self, select: bool) {
        if !select {
            if let Some((_, b)) = self.selection() {
                self.set_cursor(b, false);
                return;
            }
        }
        let p = self.cursor;
        let len = self.line(p.line).len();
        let np = if p.col < len { Pos::new(p.line, next_boundary(self.line(p.line), p.col)) } else if p.line + 1 < self.buf.len() { Pos::new(p.line + 1, 0) } else { p };
        self.set_cursor(np, select);
        self.goal_x = None;
    }

    pub fn word_left(&mut self, select: bool) {
        let p = self.cursor;
        let np = if p.col == 0 && p.line > 0 { Pos::new(p.line - 1, self.line(p.line - 1).len()) } else { Pos::new(p.line, text::prev_word(self.line(p.line), p.col)) };
        self.set_cursor(np, select);
        self.goal_x = None;
    }

    pub fn word_right(&mut self, select: bool) {
        let p = self.cursor;
        let l = self.line(p.line);
        let np = if p.col >= l.len() && p.line + 1 < self.buf.len() { Pos::new(p.line + 1, 0) } else { Pos::new(p.line, text::next_word(l, p.col)) };
        self.set_cursor(np, select);
        self.goal_x = None;
    }

    /// Smart home: content start (after list/quote/heading markers) ↔ col 0.
    pub fn home(&mut self, select: bool) {
        let ln = self.cursor.line;
        let content = self.content_start(ln);
        let col = if self.cursor.col != content && content > 0 { content } else { 0 };
        self.set_cursor(Pos::new(ln, col), select);
        self.goal_x = None;
    }

    pub fn end(&mut self, select: bool) {
        let ln = self.cursor.line;
        let len = self.line(ln).len();
        self.set_cursor(Pos::new(ln, len), select);
        self.goal_x = None;
    }

    pub fn doc_start(&mut self, select: bool) {
        self.set_cursor(Pos::new(0, 0), select);
    }
    pub fn doc_end(&mut self, select: bool) {
        let e = self.buf.end();
        self.set_cursor(e, select);
    }

    pub fn paragraph(&mut self, down: bool, select: bool) {
        let mut l = self.cursor.line;
        let n = self.buf.len();
        let blank = |e: &Editor, i: usize| e.line(i).trim().is_empty();
        if down {
            while l + 1 < n && blank(self, l) {
                l += 1;
            }
            while l + 1 < n && !blank(self, l) {
                l += 1;
            }
            let col = if l + 1 == n { self.line(l).len() } else { 0 };
            self.set_cursor(Pos::new(l, col), select);
        } else {
            while l > 0 && blank(self, l) {
                l -= 1;
            }
            while l > 0 && !blank(self, l) {
                l -= 1;
            }
            self.set_cursor(Pos::new(l, 0), select);
        }
    }

    pub fn content_start(&mut self, ln: usize) -> usize {
        let info = self.doc().infos[ln].clone();
        match info.kind {
            LineKind::Code | LineKind::Front | LineKind::Math => leading_ws(self.line(ln)),
            _ => info.content.min(self.line(ln).len()),
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(Pos::new(0, 0));
        self.cursor = self.buf.end();
    }

    pub fn select_word(&mut self) {
        let p = self.cursor;
        let (a, b) = text::word_bounds(self.line(p.line), p.col);
        self.select_range(Pos::new(p.line, a), Pos::new(p.line, b));
    }

    pub fn select_line(&mut self) {
        let (a, b) = match self.selection() {
            Some((a, b)) => (a.line, b.line),
            None => (self.cursor.line, self.cursor.line),
        };
        let end = if b + 1 < self.buf.len() { Pos::new(b + 1, 0) } else { Pos::new(b, self.line(b).len()) };
        self.select_range(Pos::new(a, 0), end);
    }

    // ------------------------------------------------------------- editing primitives
    fn begin(&mut self, kind: &'static str) {
        let s = self.sel();
        self.buf.begin(s, kind);
    }
    fn commit(&mut self) {
        let s = self.sel();
        self.buf.commit(s);
        self.goal_x = None;
    }

    fn replace(&mut self, a: Pos, b: Pos, t: &str) -> Pos {
        self.buf.replace(a, b, t)
    }

    /// Replace the selection (if any) with `t`, leaving the cursor after it.
    pub fn insert(&mut self, t: &str) {
        self.begin(if t.chars().count() == 1 && t != "\n" { "type" } else { "" });
        self.insert_raw(t);
        self.commit();
    }

    fn insert_raw(&mut self, t: &str) {
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        let end = self.replace(a, b, t);
        self.cursor = end;
        self.anchor = None;
    }

    /// Typing a character: auto-pairs, skipping over closers, wrapping selections.
    pub fn type_char(&mut self, c: char) {
        let s = c.to_string();
        if let Some((a, b)) = self.selection() {
            if self.autopair && WRAPPERS.contains(&s.as_str()) && a.line == b.line {
                let close = PAIRS.iter().find(|p| p.0 == c).map_or(c, |p| p.1);
                self.begin("");
                let inner = self.buf.get(a, b);
                self.replace(a, b, &format!("{c}{inner}{close}"));
                self.select_range(Pos::new(a.line, a.col + c.len_utf8()), Pos::new(b.line, b.col + c.len_utf8()));
                self.commit();
                return;
            }
            self.insert(&s);
            return;
        }
        let p = self.cursor;
        let line = self.line(p.line).to_string();
        let next = line[p.col..].chars().next();
        let prev = line[..p.col].chars().next_back();
        // skip over an auto-inserted closer
        if self.autopair && next == Some(c) && PAIRS.iter().any(|q| q.1 == c) {
            self.cursor = Pos::new(p.line, p.col + c.len_utf8());
            self.buf.seal();
            return;
        }
        if self.autopair {
            if let Some(&(o, cl)) = PAIRS.iter().find(|q| q.0 == c) {
                let next_ok = next.is_none_or(|n| n.is_whitespace() || ")]}.,;:!?".contains(n));
                let symmetric = o == cl;
                let prev_ok = !symmetric || prev.is_none_or(|p| p.is_whitespace() || "([{".contains(p));
                let in_code = self.in_code_block(p.line) && c != '(' && c != '[' && c != '{';
                if next_ok && prev_ok && !in_code {
                    self.begin("");
                    self.replace(p, p, &format!("{o}{cl}"));
                    self.cursor = Pos::new(p.line, p.col + o.len_utf8());
                    self.commit();
                    self.buf.seal();
                    return;
                }
            }
        }
        self.insert(&s);
    }

    pub fn in_code_block(&mut self, ln: usize) -> bool {
        matches!(self.doc().infos[ln].kind, LineKind::Code | LineKind::Fence)
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            self.begin("");
            self.insert_raw("");
            self.commit();
            return;
        }
        let p = self.cursor;
        if p.col == 0 {
            if p.line == 0 {
                return;
            }
            let prev_len = self.line(p.line - 1).len();
            self.begin("backspace");
            self.replace(Pos::new(p.line - 1, prev_len), p, "");
            self.cursor = Pos::new(p.line - 1, prev_len);
            self.commit();
            return;
        }
        let info = self.doc().infos[p.line].clone();
        let line = self.line(p.line).to_string();
        // backspace right after a list marker removes the marker (keeps indent)
        if let Some(lm) = &info.lst {
            if p.col == lm.end && info.kind == LineKind::Text {
                self.begin("");
                let keep = if lm.task.is_some() { lm.task_start } else { lm.start };
                let a = Pos::new(p.line, if lm.task.is_some() { lm.task_start } else { lm.start });
                let _ = keep;
                self.replace(a, p, "");
                self.cursor = a;
                self.commit();
                return;
            }
        }
        // after "> " of a quote: remove one level
        if info.quote_depth > 0 && p.col == info.quote_end && info.lst.is_none() {
            self.begin("");
            let gt = line[..info.quote_end].rfind('>').unwrap_or(0);
            self.replace(Pos::new(p.line, gt), p, "");
            self.cursor = Pos::new(p.line, gt);
            self.commit();
            return;
        }
        // heading markers: "## |" → "# |"
        if info.kind == LineKind::Heading && !info.setext && p.col == info.content {
            self.begin("");
            let hashes = line[..info.content].trim_end();
            let a = Pos::new(p.line, hashes.len() - 1);
            let new = if info.level > 1 { " " } else { "" };
            self.replace(a, p, new);
            self.cursor = Pos::new(p.line, a.col + new.len());
            self.commit();
            return;
        }
        let pc = prev_boundary(&line, p.col);
        let prev = &line[pc..p.col];
        let next = line[p.col..].chars().next();
        self.begin("backspace");
        let mut end = p;
        if self.autopair {
            if let (Some(o), Some(n)) = (prev.chars().next(), next) {
                if PAIRS.iter().any(|q| q.0 == o && q.1 == n) {
                    end = Pos::new(p.line, p.col + n.len_utf8());
                }
            }
        }
        // backspace in leading indentation removes a whole indent unit
        if line[..p.col].chars().all(|c| c == ' ') && p.col >= 2 && end == p {
            let unit = self.indent_unit.len().min(4).max(2);
            let back = ((p.col - 1) % unit) + 1;
            self.replace(Pos::new(p.line, p.col - back), end, "");
            self.cursor = Pos::new(p.line, p.col - back);
        } else {
            self.replace(Pos::new(p.line, pc), end, "");
            self.cursor = Pos::new(p.line, pc);
        }
        self.commit();
    }

    pub fn delete(&mut self) {
        if self.selection().is_some() {
            self.backspace();
            return;
        }
        let p = self.cursor;
        let len = self.line(p.line).len();
        self.begin("delete");
        if p.col >= len {
            if p.line + 1 < self.buf.len() {
                self.replace(p, Pos::new(p.line + 1, 0), "");
            }
        } else {
            let nb = next_boundary(self.line(p.line), p.col);
            self.replace(p, Pos::new(p.line, nb), "");
        }
        self.commit();
    }

    pub fn delete_word(&mut self, back: bool) {
        if self.selection().is_none() {
            let start = self.cursor;
            if back {
                self.word_left(false);
            } else {
                self.word_right(false);
            }
            self.anchor = Some(start);
        }
        self.begin("");
        self.insert_raw("");
        self.commit();
    }

    pub fn delete_to_line_start(&mut self) {
        let p = self.cursor;
        self.begin("");
        self.replace(Pos::new(p.line, 0), p, "");
        self.cursor = Pos::new(p.line, 0);
        self.commit();
    }

    // ------------------------------------------------------------- smart newline
    pub fn newline(&mut self) {
        if self.selection().is_some() {
            self.begin("");
            self.insert_raw("");
            self.commit();
        }
        let p = self.cursor;
        let info = self.doc().infos[p.line].clone();
        let line = self.line(p.line).to_string();
        let indent = &line[..leading_ws(&line)];
        self.begin("");
        match info.kind {
            LineKind::Code | LineKind::Fence | LineKind::Front | LineKind::Math => {
                let ind = indent.to_string();
                let extra = if info.kind == LineKind::Code && line[..p.col].trim_end().ends_with([':', '{', '(', '[']) { self.indent_unit.clone() } else { String::new() };
                self.insert_raw(&format!("\n{ind}{extra}"));
            }
            LineKind::TableHead | LineKind::TableRow | LineKind::TableDelim if p.col >= line.trim_end().len() => {
                let ncols = md::split_table_row(&line).len().max(1);
                let row = format!("\n|{}", " |".repeat(ncols));
                self.cursor = Pos::new(p.line, line.len());
                self.insert_raw(&row);
                self.cursor = Pos::new(p.line + 1, 2);
            }
            _ => {
                let qprefix = line[..info.quote_end].to_string();
                if let Some(lm) = info.lst.clone() {
                    let content_empty = line[lm.end..].trim().is_empty();
                    if content_empty && p.col >= lm.end {
                        // empty item: outdent, or end the list
                        let ind_len = lm.start - info.quote_end;
                        if ind_len > 0 {
                            let cut = self.indent_unit.len().min(ind_len);
                            let cut = if line[info.quote_end..].starts_with('\t') { 1 } else { cut };
                            self.replace(Pos::new(p.line, info.quote_end), Pos::new(p.line, info.quote_end + cut), "");
                            self.cursor = Pos::new(p.line, line.len() - cut);
                        } else {
                            self.replace(Pos::new(p.line, info.quote_end), Pos::new(p.line, line.len()), "");
                            self.cursor = Pos::new(p.line, info.quote_end);
                        }
                    } else if p.col < lm.end {
                        self.insert_raw("\n");
                    } else {
                        let lead = &line[info.quote_end..lm.start];
                        let marker = if lm.ordered { format!("{}{}", lm.number + 1, lm.delim as char) } else { (lm.bullet as char).to_string() };
                        let task = if lm.task.is_some() { "[ ] " } else { "" };
                        let ins = format!("\n{qprefix}{lead}{marker} {task}");
                        self.insert_raw(&ins);
                        if lm.ordered {
                            self.renumber_after(self.cursor.line, &lm);
                        }
                    }
                } else if info.quote_depth > 0 {
                    if line[info.quote_end..].trim().is_empty() && p.col >= line.len() {
                        self.replace(Pos::new(p.line, 0), Pos::new(p.line, line.len()), "");
                        self.cursor = Pos::new(p.line, 0);
                    } else {
                        let q = qprefix.trim_end().to_string() + " ";
                        self.insert_raw(&format!("\n{q}"));
                    }
                } else if !indent.is_empty() && p.col >= indent.len() {
                    let ind = indent.to_string();
                    self.insert_raw(&format!("\n{ind}"));
                } else {
                    self.insert_raw("\n");
                }
            }
        }
        self.commit();
    }

    /// Plain newline without continuation (shift+enter).
    pub fn newline_plain(&mut self) {
        self.insert("\n");
    }

    /// Renumber following siblings of an ordered list after inserting `at`.
    fn renumber_after(&mut self, at: usize, lm: &md::ListMarker) {
        let mut n = lm.number + 1;
        let indent = lm.start;
        let mut i = at + 1;
        loop {
            if i >= self.buf.len() {
                break;
            }
            let doc = self.doc().clone();
            let Some(m) = doc.infos[i].lst.clone() else {
                if self.line(i).trim().is_empty() || leading_ws(self.line(i)) <= indent {
                    break;
                }
                i += 1;
                continue;
            };
            if m.start < indent {
                break;
            }
            if m.start == indent && m.ordered {
                n += 1;
                let new = format!("{}{}", n, m.delim as char);
                self.replace(Pos::new(i, m.start), Pos::new(i, m.marker_end), &new);
            }
            i += 1;
        }
    }

    // ------------------------------------------------------------- indentation
    fn line_range(&self) -> (usize, usize) {
        match self.selection() {
            Some((a, b)) => (a.line, if b.col == 0 && b.line > a.line { b.line - 1 } else { b.line }),
            None => (self.cursor.line, self.cursor.line),
        }
    }

    pub fn indent(&mut self) {
        let p = self.cursor;
        let info = self.doc().infos[p.line].clone();
        if matches!(info.kind, LineKind::TableHead | LineKind::TableRow | LineKind::TableDelim) && self.selection().is_none_or(|(a, b)| a.line == b.line) {
            self.table_next_cell(false);
            return;
        }
        if self.selection().is_none() && info.lst.is_none() && info.kind != LineKind::Code {
            let u = self.indent_unit.clone();
            let n = if u == "\t" { 1 } else { u.len() - (self.cursor.col - leading_ws(self.line(p.line)).min(self.cursor.col)) % u.len() };
            let t = if u == "\t" { "\t".to_string() } else { " ".repeat(n) };
            self.insert(&t);
            return;
        }
        let (a, b) = self.line_range();
        let unit = self.indent_unit.clone();
        self.begin("");
        for ln in a..=b {
            if self.line(ln).is_empty() && a != b {
                continue;
            }
            let qe = self.doc().infos[ln].quote_end;
            self.replace(Pos::new(ln, qe), Pos::new(ln, qe), &unit);
        }
        self.shift_sel(a, b, unit.len() as isize);
        self.commit();
    }

    pub fn outdent(&mut self) {
        let cl = self.cursor.line;
        let info = self.doc().infos[cl].clone();
        if matches!(info.kind, LineKind::TableHead | LineKind::TableRow | LineKind::TableDelim) && self.selection().is_none_or(|(a, b)| a.line == b.line) {
            self.table_next_cell(true);
            return;
        }
        let (a, b) = self.line_range();
        let unit = self.indent_unit.len();
        self.begin("");
        let mut removed_cursor = 0isize;
        for ln in a..=b {
            let qe = self.doc().infos[ln].quote_end;
            let l = self.line(ln);
            let rest = &l[qe..];
            let n = if rest.starts_with('\t') { 1 } else { rest.bytes().take(unit).take_while(|&c| c == b' ').count() };
            if n > 0 {
                self.replace(Pos::new(ln, qe), Pos::new(ln, qe + n), "");
                if ln == self.cursor.line {
                    removed_cursor = n as isize;
                }
            }
        }
        self.shift_sel(a, b, -removed_cursor);
        self.commit();
    }

    fn shift_sel(&mut self, a: usize, b: usize, d: isize) {
        let f = |p: Pos, e: &Editor| if p.line >= a && p.line <= b && p.col > 0 { e.buf.clamp(Pos::new(p.line, (p.col as isize + d).max(0) as usize)) } else { p };
        self.cursor = f(self.cursor, self);
        if let Some(an) = self.anchor {
            self.anchor = Some(f(an, self));
        }
    }

    // ------------------------------------------------------------- tables
    fn table_bounds(&mut self, ln: usize) -> Option<(usize, usize)> {
        let b = self.doc().block_of(ln)?.clone();
        (b.kind == md::BlockKind::Table).then_some((b.start, b.end))
    }

    /// Reformat the table around `ln` so its pipes line up.
    pub fn format_table(&mut self, ln: usize) {
        let Some((s, e)) = self.table_bounds(ln) else { return };
        let rows: Vec<Vec<String>> = (s..e).map(|i| md::split_table_row(self.line(i)).iter().map(|&(a, b)| self.line(i)[a..b].to_string()).collect()).collect();
        let aligns = md::table_aligns(self.line(s + 1));
        let ncols = rows[0].len();
        let mut widths = vec![3usize; ncols];
        for (r, row) in rows.iter().enumerate() {
            if r == 1 {
                continue;
            }
            for (c, cell) in row.iter().enumerate().take(ncols) {
                widths[c] = widths[c].max(text::str_width(cell));
            }
        }
        let mut out = vec![];
        for (r, row) in rows.iter().enumerate() {
            let mut l = String::from("|");
            for c in 0..ncols {
                if r == 1 {
                    let a = aligns.get(c).copied().unwrap_or(b'l');
                    let dash = "-".repeat(widths[c].saturating_sub(match a { b'c' => 2, b'r' => 1, _ => 0 }));
                    let cell = match a {
                        b'c' => format!(":{dash}:"),
                        b'r' => format!("{dash}:"),
                        _ => dash,
                    };
                    l.push_str(&format!(" {cell} |"));
                } else {
                    let cell = row.get(c).map(|s| s.as_str()).unwrap_or("");
                    let pad = widths[c] - text::str_width(cell);
                    let a = aligns.get(c).copied().unwrap_or(b'l');
                    let (lp, rp) = match a {
                        b'r' => (pad, 0),
                        b'c' => (pad / 2, pad - pad / 2),
                        _ => (0, pad),
                    };
                    l.push_str(&format!(" {}{}{} |", " ".repeat(lp), cell, " ".repeat(rp)));
                }
            }
            out.push(l);
        }
        // keep the cursor in the same cell
        let cur_cell = self.cell_index(self.cursor);
        let new = out.join("\n");
        let old_end = Pos::new(e - 1, self.line(e - 1).len());
        if self.buf.get(Pos::new(s, 0), old_end) != new {
            self.buf.begin(self.sel(), "");
            self.replace(Pos::new(s, 0), old_end, &new);
            if let Some((ln, c)) = cur_cell {
                self.move_to_cell(ln, c);
            }
            self.commit();
        }
    }

    fn cell_index(&self, p: Pos) -> Option<(usize, usize)> {
        let cells = md::split_table_row(self.line(p.line));
        let idx = cells.iter().position(|&(_, b)| p.col <= b).unwrap_or(cells.len().saturating_sub(1));
        Some((p.line, idx))
    }

    fn move_to_cell(&mut self, ln: usize, c: usize) {
        let cells = md::split_table_row(self.line(ln));
        if let Some(&(a, b)) = cells.get(c) {
            self.select_range(Pos::new(ln, a), Pos::new(ln, b));
            if a == b {
                self.anchor = None;
            }
        }
    }

    pub fn table_next_cell(&mut self, back: bool) {
        let p = self.cursor;
        let Some((s, e)) = self.table_bounds(p.line) else { return };
        self.format_table(p.line);
        let (ln, c) = self.cell_index(self.cursor).unwrap();
        let ncols = md::split_table_row(self.line(s)).len();
        let (mut nl, mut nc) = (ln, c as isize + if back { -1 } else { 1 });
        if nc >= ncols as isize {
            nc = 0;
            nl += 1;
        } else if nc < 0 {
            nc = ncols as isize - 1;
            nl = nl.saturating_sub(1);
        }
        if nl == s + 1 {
            nl = if back { s } else { s + 2 };
        }
        if nl >= e {
            // add a row at the end
            self.buf.begin(self.sel(), "");
            let end = Pos::new(e - 1, self.line(e - 1).len());
            self.replace(end, end, &format!("\n|{}", " |".repeat(ncols)));
            self.commit();
            self.format_table(s);
            nl = e;
            nc = 0;
        }
        self.move_to_cell(nl, nc as usize);
    }

    // ------------------------------------------------------------- formatting
    /// Wrap the selection (or word under cursor) in `m`, or unwrap if wrapped.
    pub fn toggle_wrap(&mut self, m: &str) {
        let (a, b) = match self.selection() {
            Some(s) => s,
            None => {
                let p = self.cursor;
                let (wa, wb) = text::word_bounds(self.line(p.line), p.col);
                let l = self.line(p.line);
                if wa < wb && l[wa..wb].chars().all(text::is_word_char) {
                    (Pos::new(p.line, wa), Pos::new(p.line, wb))
                } else {
                    self.begin("");
                    self.replace(p, p, &format!("{m}{m}"));
                    self.cursor = Pos::new(p.line, p.col + m.len());
                    self.commit();
                    return;
                }
            }
        };
        self.begin("");
        let ml = m.len();
        let line_a = self.line(a.line).to_string();
        let line_b = self.line(b.line).to_string();
        let inner = self.buf.get(a, b);
        if inner.len() >= 2 * ml && inner.starts_with(m) && inner.ends_with(m) && a.line == b.line {
            self.replace(a, b, &inner[ml..inner.len() - ml]);
            self.select_range(a, Pos::new(b.line, b.col - 2 * ml));
        } else if a.col >= ml && line_a[..a.col].ends_with(m) && line_b[b.col..].starts_with(m) {
            self.replace(Pos::new(b.line, b.col), Pos::new(b.line, b.col + ml), "");
            self.replace(Pos::new(a.line, a.col - ml), a, "");
            let nb = if a.line == b.line { Pos::new(b.line, b.col - ml) } else { b };
            self.select_range(Pos::new(a.line, a.col - ml), nb);
        } else {
            self.replace(b, b, m);
            self.replace(a, a, m);
            let nb = if a.line == b.line { Pos::new(b.line, b.col + ml) } else { b };
            self.select_range(Pos::new(a.line, a.col + ml), nb);
        }
        self.commit();
    }

    pub fn insert_link(&mut self) {
        match self.selection() {
            Some((a, b)) if a.line == b.line => {
                let t = self.buf.get(a, b);
                self.begin("");
                let is_url = t.starts_with("http://") || t.starts_with("https://");
                let new = if is_url { format!("[]({t})") } else { format!("[{t}]()") };
                self.replace(a, b, &new);
                self.cursor = if is_url { Pos::new(a.line, a.col + 1) } else { Pos::new(a.line, a.col + t.len() + 3) };
                self.anchor = None;
                self.commit();
            }
            _ => {
                let p = self.cursor;
                self.begin("");
                self.replace(p, p, "[]()");
                self.cursor = Pos::new(p.line, p.col + 1);
                self.anchor = None;
                self.commit();
            }
        }
    }

    pub fn insert_wikilink(&mut self) {
        let p = self.cursor;
        self.begin("");
        let t = self.selected_text().unwrap_or_default();
        self.insert_raw(&format!("[[{t}]]"));
        self.cursor = Pos::new(p.line.max(self.cursor.line), self.cursor.col - 2);
        self.commit();
    }

    /// Cycle: text → "- [ ] text" → "- [x] text" → "- [ ] text".
    pub fn toggle_task(&mut self) {
        let (a, b) = self.line_range();
        self.begin("");
        let col_before = self.cursor.col;
        let mut delta = 0isize;
        for ln in a..=b {
            let info = self.doc().infos[ln].clone();
            let line = self.line(ln).to_string();
            if matches!(info.kind, LineKind::Code | LineKind::Fence | LineKind::TableRow | LineKind::TableHead | LineKind::TableDelim) {
                continue;
            }
            let d: isize;
            match &info.lst {
                Some(lm) if lm.task.is_some() => {
                    let tk = lm.task.unwrap();
                    let new = if tk == ' ' { "x" } else { " " };
                    self.replace(Pos::new(ln, lm.task_start + 1), Pos::new(ln, lm.task_start + 2), new);
                    d = 0;
                }
                Some(lm) => {
                    let ins = if lm.end == line.len() && !line.ends_with(' ') { " [ ] " } else { "[ ] " };
                    self.replace(Pos::new(ln, lm.end), Pos::new(ln, lm.end), ins);
                    d = ins.len() as isize;
                }
                None => {
                    let at = info.quote_end + leading_ws(&line[info.quote_end..]);
                    let at = if info.kind == LineKind::Heading { info.content } else { at };
                    self.replace(Pos::new(ln, at), Pos::new(ln, at), "- [ ] ");
                    d = 6;
                }
            }
            if ln == self.cursor.line {
                delta = d;
            }
        }
        let c = Pos::new(self.cursor.line, (col_before as isize + delta).max(0) as usize);
        self.cursor = self.buf.clamp(c);
        self.commit();
    }

    /// Toggle the task checkbox on line `ln` (mouse click).
    pub fn toggle_task_at(&mut self, ln: usize) {
        let info = self.doc().infos[ln].clone();
        if let Some(lm) = info.lst {
            if let Some(tk) = lm.task {
                self.buf.begin(self.sel(), "");
                let new = if tk == ' ' { "x" } else { " " };
                self.replace(Pos::new(ln, lm.task_start + 1), Pos::new(ln, lm.task_start + 2), new);
                self.commit();
            }
        }
    }

    /// Set heading level (0 = paragraph) on the cursor line(s).
    pub fn set_heading(&mut self, level: usize) {
        let (a, b) = self.line_range();
        self.begin("");
        for ln in a..=b {
            let info = self.doc().infos[ln].clone();
            let line = self.line(ln).to_string();
            let start = info.quote_end;
            let content = if info.kind == LineKind::Heading && !info.setext { info.content } else { start + leading_ws(&line[start..]) };
            let new = if level == 0 { String::new() } else { format!("{} ", "#".repeat(level)) };
            let old_len = content - start;
            self.replace(Pos::new(ln, start), Pos::new(ln, content), &new);
            if ln == self.cursor.line {
                let c = (self.cursor.col as isize + new.len() as isize - old_len as isize).max(start as isize) as usize;
                self.cursor = self.buf.clamp(Pos::new(ln, c));
            }
        }
        self.anchor = None;
        self.commit();
    }

    pub fn cycle_heading(&mut self) {
        let cl = self.cursor.line;
        let info = self.doc().infos[cl].clone();
        let lvl = if info.kind == LineKind::Heading { info.level as usize } else { 0 };
        self.set_heading(if lvl >= 6 { 0 } else { lvl + 1 });
    }

    pub fn toggle_quote(&mut self) {
        let (a, b) = self.line_range();
        let all_quoted = (a..=b).all(|l| self.line(l).trim_start().starts_with('>'));
        self.begin("");
        for ln in a..=b {
            let line = self.line(ln).to_string();
            if all_quoted {
                let (_, qe) = md::parse_quote(&line);
                let gt = line.find('>').unwrap();
                let cut = if qe > gt + 1 { gt + 2 } else { gt + 1 };
                self.replace(Pos::new(ln, 0), Pos::new(ln, cut.min(line.len())), &line[..gt]);
            } else {
                self.replace(Pos::new(ln, 0), Pos::new(ln, 0), "> ");
            }
        }
        self.commit();
    }

    pub fn toggle_list(&mut self, ordered: bool) {
        let (a, b) = self.line_range();
        self.begin("");
        let mut n = 1;
        for ln in a..=b {
            let info = self.doc().infos[ln].clone();
            let line = self.line(ln).to_string();
            match info.lst {
                Some(lm) if lm.ordered == ordered => {
                    self.replace(Pos::new(ln, lm.start), Pos::new(ln, lm.end), "");
                }
                Some(lm) => {
                    let m = if ordered { format!("{n}. ") } else { "- ".to_string() };
                    self.replace(Pos::new(ln, lm.start), Pos::new(ln, lm.marker_end + 1), &m);
                }
                None if !line.trim().is_empty() => {
                    let at = info.quote_end + leading_ws(&line[info.quote_end..]);
                    let m = if ordered { format!("{n}. ") } else { "- ".to_string() };
                    self.replace(Pos::new(ln, at), Pos::new(ln, at), &m);
                }
                None => {}
            }
            n += 1;
        }
        self.cursor = self.buf.clamp(self.cursor);
        self.commit();
    }

    pub fn insert_code_block(&mut self) {
        let sel = self.selected_text();
        self.begin("");
        match sel {
            Some(t) => {
                let t = t.trim_end_matches('\n').to_string();
                self.insert_raw(&format!("```\n{t}\n```"));
            }
            None => {
                let p = self.cursor;
                let prefix = if self.line(p.line).trim().is_empty() { "" } else { "\n" };
                self.insert_raw(&format!("{prefix}```\n\n```"));
                self.cursor = Pos::new(self.cursor.line - 1, 0);
            }
        }
        self.commit();
    }

    pub fn insert_table(&mut self) {
        self.begin("");
        let p = self.cursor;
        let prefix = if self.line(p.line).trim().is_empty() { "" } else { "\n\n" };
        self.insert_raw(&format!("{prefix}|     |     |\n| --- | --- |\n|     |     |"));
        let l = self.cursor.line - 2;
        self.cursor = Pos::new(l, 2);
        self.commit();
    }

    pub fn insert_hr(&mut self) {
        self.begin("");
        let p = self.cursor;
        let prefix = if self.line(p.line).trim().is_empty() { "" } else { "\n" };
        self.insert_raw(&format!("{prefix}---\n"));
        self.commit();
    }

    // ------------------------------------------------------------- line operations
    pub fn move_lines(&mut self, down: bool) {
        let (a, b) = self.line_range();
        if (!down && a == 0) || (down && b + 1 >= self.buf.len()) {
            return;
        }
        self.begin("");
        let block: Vec<String> = self.buf.lines[a..=b].to_vec();
        let (sa, sb) = (self.anchor, self.cursor);
        if down {
            let next = self.line(b + 1).to_string();
            let mut new = vec![next];
            new.extend(block);
            self.replace(Pos::new(a, 0), Pos::new(b + 1, self.line(b + 1).len()), &new.join("\n"));
        } else {
            let prev = self.line(a - 1).to_string();
            let mut new = block;
            new.push(prev);
            self.replace(Pos::new(a - 1, 0), Pos::new(b, self.line(b).len()), &new.join("\n"));
        }
        let mv = |p: Pos| if down { Pos::new(p.line + 1, p.col) } else { Pos::new(p.line - 1, p.col) };
        self.cursor = self.buf.clamp(mv(sb));
        self.anchor = sa.map(|p| self.buf.clamp(mv(p)));
        self.commit();
    }

    pub fn duplicate(&mut self) {
        if let Some((a, b)) = self.selection() {
            let t = self.buf.get(a, b);
            self.begin("");
            self.replace(b, b, &t);
            self.commit();
            return;
        }
        let (a, b) = self.line_range();
        let block = self.buf.lines[a..=b].join("\n");
        self.begin("");
        let end = Pos::new(b, self.line(b).len());
        self.replace(end, end, &format!("\n{block}"));
        self.cursor = Pos::new(self.cursor.line + (b - a + 1), self.cursor.col);
        self.commit();
    }

    pub fn delete_lines(&mut self) {
        let (a, b) = self.line_range();
        self.begin("");
        if b + 1 < self.buf.len() {
            self.replace(Pos::new(a, 0), Pos::new(b + 1, 0), "");
        } else if a > 0 {
            let pl = self.line(a - 1).len();
            self.replace(Pos::new(a - 1, pl), Pos::new(b, self.line(b).len()), "");
        } else {
            self.replace(Pos::new(a, 0), Pos::new(b, self.line(b).len()), "");
        }
        self.anchor = None;
        self.cursor = self.buf.clamp(Pos::new(a, self.cursor.col));
        self.commit();
    }

    pub fn join_lines(&mut self) {
        let ln = self.cursor.line;
        if ln + 1 >= self.buf.len() {
            return;
        }
        let next = self.line(ln + 1).to_string();
        let info_next = self.doc().infos[ln + 1].clone();
        let strip = if info_next.lst.is_some() { info_next.content } else { leading_ws(&next) };
        let len = self.line(ln).len();
        self.begin("");
        let sep = if self.line(ln).ends_with(' ') || next.trim().is_empty() { "" } else { " " };
        self.replace(Pos::new(ln, len), Pos::new(ln + 1, strip), sep);
        self.cursor = Pos::new(ln, len);
        self.anchor = None;
        self.commit();
    }

    pub fn undo(&mut self) -> bool {
        match self.buf.undo() {
            Some(s) => {
                self.restore(s);
                true
            }
            None => false,
        }
    }
    pub fn redo(&mut self) -> bool {
        match self.buf.redo() {
            Some(s) => {
                self.restore(s);
                true
            }
            None => false,
        }
    }

    /// Paste: adapts list/quote prefixes of the first line when pasting into a list.
    pub fn paste(&mut self, t: &str) {
        let t = t.replace("\r\n", "\n");
        self.begin("");
        self.insert_raw(&t);
        self.commit();
        self.buf.seal();
    }

    // ------------------------------------------------------------- search
    pub fn find_all(&self, q: &str, case: bool) -> Vec<(Pos, Pos)> {
        if q.is_empty() {
            return vec![];
        }
        let mut out = vec![];
        let ql = if case { q.to_string() } else { q.to_lowercase() };
        for (i, l) in self.buf.lines.iter().enumerate() {
            let hay = if case { l.clone() } else { l.to_lowercase() };
            if hay.len() != l.len() {
                // lowercasing changed byte lengths: fall back to char scan
                for (k, _) in l.char_indices() {
                    if l[k..].to_lowercase().starts_with(&ql) {
                        let mut e = k;
                        let mut taken = 0;
                        for c in l[k..].chars() {
                            if taken >= q.chars().count() {
                                break;
                            }
                            e += c.len_utf8();
                            taken += 1;
                        }
                        out.push((Pos::new(i, k), Pos::new(i, e)));
                    }
                }
                continue;
            }
            let mut start = 0;
            while let Some(f) = hay[start..].find(&ql) {
                let a = start + f;
                out.push((Pos::new(i, a), Pos::new(i, a + ql.len())));
                start = a + ql.len().max(1);
                while start < hay.len() && !hay.is_char_boundary(start) {
                    start += 1;
                }
                if start >= hay.len() {
                    break;
                }
            }
        }
        out
    }

    pub fn find_next(&mut self, q: &str, case: bool, back: bool) -> bool {
        let all = self.find_all(q, case);
        if all.is_empty() {
            return false;
        }
        let here = self.selection().map(|s| s.0).unwrap_or(self.cursor);
        let m = if back {
            all.iter().rev().find(|m| m.0 < here).or(all.last())
        } else {
            let after = self.selection().map(|s| s.0).unwrap_or(self.cursor);
            all.iter().find(|m| m.0 > after || (self.selection().is_none() && m.0 >= after)).or(all.first())
        };
        if let Some(&(a, b)) = m {
            self.select_range(a, b);
            return true;
        }
        false
    }

    pub fn replace_current(&mut self, q: &str, with: &str, case: bool) -> bool {
        if let Some(t) = self.selected_text() {
            let eq = if case { t == q } else { t.to_lowercase() == q.to_lowercase() };
            if eq {
                self.insert(with);
            }
        }
        self.find_next(q, case, false)
    }

    pub fn replace_all(&mut self, q: &str, with: &str, case: bool) -> usize {
        let all = self.find_all(q, case);
        if all.is_empty() {
            return 0;
        }
        self.begin("");
        for &(a, b) in all.iter().rev() {
            self.replace(a, b, with);
        }
        self.anchor = None;
        self.cursor = self.buf.clamp(self.cursor);
        self.commit();
        all.len()
    }

    // ------------------------------------------------------------- links
    pub fn link_at(&mut self, p: Pos) -> Option<LinkTarget> {
        let line = self.line(p.line).to_string();
        let spans = md::parse_inline(&line, 0, line.len());
        let mut best: Option<&md::Span> = None;
        for s in &spans {
            if s.start <= p.col && p.col <= s.end && matches!(s.kind, SpanKind::Link | SpanKind::WikiLink | SpanKind::Url | SpanKind::Embed | SpanKind::Image | SpanKind::Tag | SpanKind::FootRef) {
                if best.is_none_or(|b| s.end - s.start < b.end - b.start) {
                    best = Some(s);
                }
            }
        }
        let s = best?;
        Some(match s.kind {
            SpanKind::Url => LinkTarget::Url(s.target.clone()),
            SpanKind::Tag => LinkTarget::Tag(s.target.clone()),
            SpanKind::FootRef => LinkTarget::Footnote(s.target.clone()),
            SpanKind::WikiLink | SpanKind::Embed => LinkTarget::Note { target: s.target.clone(), anchor: s.anchor.clone() },
            _ => {
                let t = s.target.clone();
                if t.contains("://") || t.starts_with("mailto:") {
                    LinkTarget::Url(t)
                } else if let Some(h) = t.strip_prefix('#') {
                    LinkTarget::Note { target: String::new(), anchor: h.to_string() }
                } else {
                    let (path, anchor) = t.split_once('#').unwrap_or((&t, ""));
                    LinkTarget::Note { target: percent_decode(path), anchor: anchor.to_string() }
                }
            }
        })
    }

    pub fn word_count(&mut self) -> (usize, usize) {
        if self.wc.0 == self.buf.version {
            return (self.wc.1, self.wc.2);
        }
        // huge documents: refresh the count at most a few times per second
        if self.buf.lines.len() > 20_000 && self.wc.3.is_some_and(|t| t.elapsed().as_millis() < 400) {
            return (self.wc.1, self.wc.2);
        }
        let mut words = 0;
        let mut chars = 0;
        for l in &self.buf.lines {
            words += l.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count();
            chars += l.chars().count();
        }
        self.wc = (self.buf.version, words, chars, Some(std::time::Instant::now()));
        (words, chars)
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(s: &str) -> Editor {
        // '|' marks the cursor
        let at = s.find('|').unwrap();
        let text = s.replacen('|', "", 1);
        let mut e = Editor::new(&text);
        let before = &text[..at];
        let line = before.matches('\n').count();
        let col = at - before.rfind('\n').map_or(0, |i| i + 1);
        e.cursor = Pos::new(line, col);
        e
    }
    fn show(e: &Editor) -> String {
        let mut lines = e.buf.lines.clone();
        lines[e.cursor.line].insert(e.cursor.col, '|');
        lines.join("\n")
    }

    #[test]
    fn list_continuation() {
        let mut e = ed("- one|");
        e.newline();
        assert_eq!(show(&e), "- one\n- |");
        e.newline();
        assert_eq!(show(&e), "- one\n|");
        let mut e = ed("1. a|\n2. b");
        e.newline();
        assert_eq!(show(&e), "1. a\n2. |\n3. b");
        let mut e = ed("- [x] done|");
        e.newline();
        assert_eq!(show(&e), "- [x] done\n- [ ] |");
        let mut e = ed("    - nested|");
        e.newline();
        e.newline();
        assert_eq!(show(&e), "    - nested\n- |");
        let mut e = ed("> quote|");
        e.newline();
        assert_eq!(show(&e), "> quote\n> |");
        e.newline();
        assert_eq!(show(&e), "> quote\n|");
    }

    #[test]
    fn backspace_markers() {
        let mut e = ed("- |item");
        e.backspace();
        assert_eq!(show(&e), "|item");
        let mut e = ed("## |Head");
        e.backspace();
        assert_eq!(show(&e), "# |Head");
        let mut e = ed("(|)");
        e.backspace();
        assert_eq!(show(&e), "|");
        let mut e = ed("        |x");
        e.backspace();
        assert_eq!(show(&e), "    |x");
    }

    #[test]
    fn autopair_and_wrap() {
        let mut e = ed("a |");
        e.type_char('(');
        assert_eq!(show(&e), "a (|)");
        e.type_char('x');
        e.type_char(')');
        assert_eq!(show(&e), "a (x)|");
        let mut e = ed("hello |world");
        e.select_word();
        e.type_char('*');
        assert_eq!(e.buf.text(), "hello *world*");
        let mut e = ed("make |bold here");
        e.toggle_wrap("**");
        assert_eq!(e.buf.text(), "make **bold** here");
        e.toggle_wrap("**");
        assert_eq!(e.buf.text(), "make bold here");
    }

    #[test]
    fn tasks_and_headings() {
        let mut e = ed("buy |milk");
        e.toggle_task();
        assert_eq!(show(&e), "- [ ] buy |milk");
        e.toggle_task();
        assert_eq!(e.buf.text(), "- [x] buy milk");
        e.toggle_task();
        assert_eq!(e.buf.text(), "- [ ] buy milk");
        let mut e = ed("ti|tle");
        e.set_heading(2);
        assert_eq!(show(&e), "## ti|tle");
        e.set_heading(0);
        assert_eq!(show(&e), "ti|tle");
    }

    #[test]
    fn table_tab_formats() {
        let mut e = ed("|a|b|\n|-|-|\n|1|2|");
        e.cursor = Pos::new(0, 1);
        e.indent();
        assert_eq!(e.buf.text(), "| a   | b   |\n| --- | --- |\n| 1   | 2   |");
        assert_eq!(e.selected_text().as_deref(), Some("b"));
        e.indent();
        assert_eq!(e.selected_text().as_deref(), Some("1"));
        e.indent();
        e.indent();
        assert_eq!(e.buf.lines.len(), 4);
        assert_eq!(e.cursor.line, 3);
    }

    #[test]
    fn line_ops_and_undo() {
        let mut e = ed("a|\nb\nc");
        e.move_lines(true);
        assert_eq!(show(&e), "b\na|\nc");
        e.duplicate();
        assert_eq!(e.buf.text(), "b\na\na\nc");
        e.undo();
        e.undo();
        assert_eq!(e.buf.text(), "a\nb\nc");
        assert_eq!(e.cursor, Pos::new(0, 1));
        e.redo();
        assert_eq!(e.buf.text(), "b\na\nc");
    }

    #[test]
    fn search_and_replace() {
        let mut e = ed("|Foo foo FOO");
        assert_eq!(e.find_all("foo", false).len(), 3);
        assert_eq!(e.find_all("foo", true).len(), 1);
        assert!(e.find_next("foo", false, false));
        assert_eq!(e.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert!(e.find_next("foo", false, false));
        assert_eq!(e.selection().unwrap().0, Pos::new(0, 4));
        assert_eq!(e.replace_all("foo", "x", false), 3);
        assert_eq!(e.buf.text(), "x x x");
    }

    #[test]
    fn links() {
        let mut e = ed("|see [[Other Note#Sec|alias]] and [x](sub/My%20File.md) ");
        assert_eq!(e.link_at(Pos::new(0, 8)), Some(LinkTarget::Note { target: "Other Note".into(), anchor: "Sec".into() }));
        assert_eq!(e.link_at(Pos::new(0, 36)), Some(LinkTarget::Note { target: "sub/My File.md".into(), anchor: "".into() }));
    }

    #[test]
    fn save_roundtrip_crlf() {
        let dir = std::env::temp_dir().join(format!("glyph-ed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.md");
        std::fs::write(&p, "a\r\nb\r\n").unwrap();
        let mut e = Editor::open(&p).unwrap();
        assert_eq!(e.buf.lines, ["a", "b", ""]);
        e.cursor = Pos::new(1, 1);
        e.insert("!");
        assert!(e.dirty());
        e.save().unwrap();
        assert!(!e.dirty());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\r\nb!\r\n");
    }
}
