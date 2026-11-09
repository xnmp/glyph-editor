//! Line buffer with grouped, coalescing undo/redo.

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub line: usize,
    /// byte offset within the line
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

pub fn end_of(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(start.line, start.col + text.len()),
        Some(i) => Pos::new(start.line + text.matches('\n').count(), text.len() - i - 1),
    }
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub start: Pos,
    pub removed: String,
    pub inserted: String,
}

/// Selection snapshot: (anchor, cursor)
pub type Sel = (Pos, Pos);

#[derive(Clone, Debug)]
pub struct Transaction {
    pub edits: Vec<Edit>,
    pub sel_before: Sel,
    pub sel_after: Sel,
    pub kind: &'static str,
    pub t: Instant,
}

pub struct Buffer {
    pub lines: Vec<String>,
    pub version: u64,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    open: Option<Transaction>,
    /// incremental-parse bookkeeping: lines before `dirty_head` and the last
    /// `dirty_tail` lines are unchanged since `take_dirty` (usize::MAX = clean)
    dirty_head: usize,
    dirty_tail: usize,
    dirty_len: usize,
    pub dirty_from: usize,
}

impl Buffer {
    pub fn new(text: &str) -> Buffer {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let n = lines.len();
        Buffer { lines, version: 0, undo: vec![], redo: vec![], open: None, dirty_head: usize::MAX, dirty_tail: usize::MAX, dirty_len: n, dirty_from: 0 }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }
    pub fn line(&self, i: usize) -> &str {
        &self.lines[i]
    }
    pub fn end(&self) -> Pos {
        Pos::new(self.lines.len() - 1, self.lines.last().map_or(0, |l| l.len()))
    }
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        let l = &self.lines[line];
        Pos::new(line, crate::text::snap(l, p.col.min(l.len())))
    }

    pub fn get(&self, a: Pos, b: Pos) -> String {
        let (a, b) = if b < a { (b, a) } else { (a, b) };
        if a.line == b.line {
            return self.lines[a.line][a.col..b.col].to_string();
        }
        let mut s = String::new();
        s.push_str(&self.lines[a.line][a.col..]);
        for l in &self.lines[a.line + 1..b.line] {
            s.push('\n');
            s.push_str(l);
        }
        s.push('\n');
        s.push_str(&self.lines[b.line][..b.col]);
        s
    }

    /// (start, old_end, new_end) of the region changed since the last call.
    pub fn take_dirty(&mut self) -> Option<(usize, usize, usize)> {
        if self.dirty_head == usize::MAX {
            self.dirty_len = self.lines.len();
            return None;
        }
        let tail = self.dirty_tail.min(self.dirty_len).min(self.lines.len());
        let r = (self.dirty_head, self.dirty_len - tail, self.lines.len() - tail);
        self.dirty_head = usize::MAX;
        self.dirty_tail = usize::MAX;
        self.dirty_len = self.lines.len();
        Some(r)
    }

    fn raw_replace(&mut self, a: Pos, b: Pos, text: &str) -> Pos {
        let unchanged_tail = self.lines.len() - b.line - 1;
        self.dirty_head = self.dirty_head.min(a.line);
        self.dirty_tail = self.dirty_tail.min(unchanged_tail);
        let tail = self.lines[b.line][b.col..].to_string();
        let mut head = self.lines[a.line][..a.col].to_string();
        let mut parts = text.split('\n');
        head.push_str(parts.next().unwrap_or(""));
        let mut new_lines = vec![head];
        for p in parts {
            new_lines.push(p.to_string());
        }
        let n = new_lines.len();
        let last = new_lines.last_mut().unwrap();
        let end = Pos::new(a.line + n - 1, last.len());
        last.push_str(&tail);
        self.lines.splice(a.line..=b.line, new_lines);
        self.version += 1;
        self.dirty_from = self.dirty_from.min(a.line);
        end
    }

    /// Start (or continue, if coalescible) an undo group.
    pub fn begin(&mut self, sel: Sel, kind: &'static str) {
        if self.open.is_some() {
            return;
        }
        if !kind.is_empty()
            && matches!(kind, "type" | "backspace" | "delete")
            && let Some(last) = self.undo.last()
            && last.kind == kind
            && last.t.elapsed().as_secs_f32() < 1.2
            && last.sel_after == sel
        {
            self.open = self.undo.pop();
            return;
        }
        self.open = Some(Transaction { edits: vec![], sel_before: sel, sel_after: sel, kind, t: Instant::now() });
    }

    pub fn replace(&mut self, a: Pos, b: Pos, text: &str) -> Pos {
        let (a, b) = if b < a { (b, a) } else { (a, b) };
        let removed = self.get(a, b);
        if removed == text {
            return end_of(a, text);
        }
        let auto = self.open.is_none();
        if auto {
            self.begin((a, a), "");
        }
        self.open.as_mut().unwrap().edits.push(Edit { start: a, removed, inserted: text.to_string() });
        let end = self.raw_replace(a, b, text);
        if auto {
            self.commit((end, end));
        }
        end
    }

    pub fn insert(&mut self, at: Pos, text: &str) -> Pos {
        self.replace(at, at, text)
    }

    pub fn commit(&mut self, sel: Sel) {
        if let Some(mut tr) = self.open.take() {
            if tr.edits.is_empty() {
                return;
            }
            tr.sel_after = sel;
            tr.t = Instant::now();
            self.undo.push(tr);
            if self.undo.len() > 5000 {
                self.undo.drain(..1000);
            }
            self.redo.clear();
        }
    }

    pub fn seal(&mut self) {
        if let Some(t) = self.undo.last_mut() {
            t.kind = "sealed";
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn undo(&mut self) -> Option<Sel> {
        let tr = self.undo.pop()?;
        for e in tr.edits.iter().rev() {
            let end = end_of(e.start, &e.inserted);
            self.raw_replace(e.start, end, &e.removed);
        }
        let sel = tr.sel_before;
        self.redo.push(tr);
        Some(sel)
    }

    pub fn redo(&mut self) -> Option<Sel> {
        let mut tr = self.redo.pop()?;
        for e in &tr.edits {
            let end = end_of(e.start, &e.removed);
            self.raw_replace(e.start, end, &e.inserted);
        }
        tr.kind = "sealed";
        let sel = tr.sel_after;
        self.undo.push(tr);
        Some(sel)
    }

    /// Replace everything as one undoable edit.
    pub fn set_text(&mut self, text: &str) {
        let end = self.end();
        self.replace(Pos::new(0, 0), end, &text.replace("\r\n", "\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edit_undo_redo() {
        let mut b = Buffer::new("hello\nworld");
        let p = b.insert(Pos::new(0, 5), " there\nnew");
        assert_eq!(p, Pos::new(1, 3));
        assert_eq!(b.text(), "hello there\nnew\nworld");
        b.replace(Pos::new(0, 0), Pos::new(2, 0), "");
        assert_eq!(b.text(), "world");
        b.undo();
        assert_eq!(b.text(), "hello there\nnew\nworld");
        b.undo();
        assert_eq!(b.text(), "hello\nworld");
        b.redo();
        b.redo();
        assert_eq!(b.text(), "world");
    }
    #[test]
    fn coalescing() {
        let mut b = Buffer::new("");
        let mut p = Pos::new(0, 0);
        for ch in ["a", "b", "c"] {
            b.begin((p, p), "type");
            p = b.insert(p, ch);
            b.commit((p, p));
        }
        assert_eq!(b.text(), "abc");
        b.undo();
        assert_eq!(b.text(), "");
    }
    #[test]
    fn get_ranges() {
        let b = Buffer::new("ab\ncd\nef");
        assert_eq!(b.get(Pos::new(0, 1), Pos::new(2, 1)), "b\ncd\ne");
    }
}
