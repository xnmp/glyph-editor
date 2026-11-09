//! Markdown structure kept in *source byte offsets*: a line-oriented block
//! parser and an inline parser. The editor renders the source itself, hiding
//! syntax, so every construct reports where its markers are.
//!
//! Dialect: CommonMark/GFM core + Obsidian (wikilinks, embeds, callouts, tags,
//! ==highlights==, %%comments%%, $math$, frontmatter, task states).

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ListMarker {
    pub start: usize,
    pub marker_end: usize,
    /// content start (after spaces / task box)
    pub end: usize,
    pub ordered: bool,
    pub number: u64,
    pub delim: u8,
    pub bullet: u8,
    pub task: Option<char>,
    pub task_start: usize,
    pub task_end: usize,
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Callout {
    pub kind: String,
    pub title: String,
    pub fold: u8, // 0, b'+', b'-'
    pub marker_start: usize,
    pub marker_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum LineKind {
    #[default]
    Text,
    Blank,
    Heading,
    SetextRule,
    Hr,
    Fence,
    Code,
    MathFence,
    Math,
    Front,
    FrontFence,
    TableHead,
    TableDelim,
    TableRow,
    CalloutHead,
    Comment,
    FootDef,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineInfo {
    pub kind: LineKind,
    pub indent: usize,
    pub quote_depth: usize,
    pub quote_end: usize,
    pub content: usize,
    pub level: u8,
    pub setext: bool,
    pub lst: Option<ListMarker>,
    pub block: Option<usize>,
    pub callout: Option<Callout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Code,
    Math,
    Front,
    Table,
    Quote,
    Callout,
    Heading,
    Comment,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Block {
    pub kind: BlockKind,
    pub start: usize,
    pub end: usize,
    pub lang: String,
    pub closed: bool,
    pub aligns: Vec<u8>, // b'l' b'c' b'r'
    pub ncols: usize,
    pub callout_kind: String,
    pub fold: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub infos: Vec<LineInfo>,
    pub blocks: Vec<Block>,
    /// line starts a construct with an empty parser state (safe restart point)
    pub restart: Vec<bool>,
}

impl Document {
    pub fn block_of(&self, line: usize) -> Option<&Block> {
        self.infos.get(line).and_then(|i| i.block).map(|b| &self.blocks[b])
    }
}

fn indent_width(s: &str) -> usize {
    crate::text::indent_width(s)
}

/// Leading blockquote markers: (depth, end byte).
pub fn parse_quote(line: &str) -> (usize, usize) {
    let b = line.as_bytes();
    let (mut depth, mut i) = (0, 0);
    loop {
        let mut j = i;
        let mut sp = 0;
        while j < b.len() && b[j] == b' ' && sp < 3 {
            j += 1;
            sp += 1;
        }
        if j < b.len() && b[j] == b'>' {
            depth += 1;
            j += 1;
            if j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            i = j;
        } else {
            return (depth, i);
        }
    }
}

pub fn parse_list_marker(line: &str, at: usize) -> Option<ListMarker> {
    let b = line.as_bytes();
    let mut i = at;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let start = i;
    if i >= b.len() {
        return None;
    }
    let (ordered, number, delim, bullet);
    if matches!(b[i], b'-' | b'*' | b'+') {
        bullet = b[i];
        ordered = false;
        number = 0;
        delim = 0;
        i += 1;
    } else if b[i].is_ascii_digit() {
        let ds = i;
        while i < b.len() && b[i].is_ascii_digit() && i - ds < 9 {
            i += 1;
        }
        if i >= b.len() || !(b[i] == b'.' || b[i] == b')') {
            return None;
        }
        number = line[ds..i].parse().unwrap_or(0);
        delim = b[i];
        bullet = 0;
        ordered = true;
        i += 1;
    } else {
        return None;
    }
    let marker_end = i;
    let mut has_space = false;
    if i < b.len() {
        if b[i] != b' ' && b[i] != b'\t' {
            return None;
        }
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
            has_space = true;
        }
    }
    let mut lm = ListMarker { start, marker_end, end: i, ordered, number, delim, bullet, task: None, task_start: 0, task_end: 0, depth: 0 };
    if has_space && i + 2 < b.len() && b[i] == b'[' && b.get(i + 2) == Some(&b']') {
        let c = b[i + 1];
        if b" xX/-!?<>*".contains(&c) && (i + 3 >= b.len() || b[i + 3] == b' ' || b[i + 3] == b'\t') {
            let mut e = i + 3;
            while e < b.len() && (b[e] == b' ' || b[e] == b'\t') {
                e += 1;
            }
            lm.task = Some(c as char);
            lm.task_start = i;
            lm.task_end = e;
            lm.end = e;
        }
    }
    Some(lm)
}

fn is_hr(s: &str) -> bool {
    let t = s.trim_start_matches(' ');
    if s.len() - t.len() > 3 {
        return false;
    }
    let mut ch = 0u8;
    let mut n = 0;
    for &c in t.as_bytes() {
        match c {
            b' ' | b'\t' => {}
            b'-' | b'*' | b'_' => {
                if ch == 0 {
                    ch = c;
                } else if ch != c {
                    return false;
                }
                n += 1;
            }
            _ => return false,
        }
    }
    n >= 3
}

fn atx(s: &str) -> Option<(u8, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b' ' && i < 3 {
        i += 1;
    }
    let hs = i;
    while i < b.len() && b[i] == b'#' {
        i += 1;
    }
    let level = i - hs;
    if level == 0 || level > 6 {
        return None;
    }
    if i < b.len() && b[i] != b' ' && b[i] != b'\t' {
        return None;
    }
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    Some((level as u8, i))
}

fn setext(s: &str) -> Option<u8> {
    let t = s.trim_end();
    let u = t.trim_start_matches(' ');
    if t.len() - u.len() > 3 || u.is_empty() {
        return None;
    }
    if u.bytes().all(|c| c == b'=') {
        Some(1)
    } else if u.bytes().all(|c| c == b'-') {
        Some(2)
    } else {
        None
    }
}

fn fence_open(line: &str) -> Option<(u8, usize, String)> {
    let t = line.trim_start();
    let b = t.as_bytes();
    if b.len() < 3 || !(b[0] == b'`' || b[0] == b'~') {
        return None;
    }
    let c = b[0];
    let n = b.iter().take_while(|&&x| x == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim();
    if c == b'`' && info.contains('`') {
        return None;
    }
    let lang = info.split(|ch: char| ch.is_whitespace() || ch == '{').next().unwrap_or("").to_lowercase();
    Some((c, n, lang))
}

/// Cell content byte ranges (trimmed) of a pipe-table row.
pub fn split_table_row(line: &str) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i < n && b[i] == b'|' {
        i += 1;
    }
    let mut cells = vec![];
    let mut start = i;
    let mut in_code = 0usize;
    while i <= n {
        let ch = if i < n { b[i] } else { b'|' };
        if ch == b'\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if ch == b'`' && i < n {
            let j = i + b[i..].iter().take_while(|&&c| c == b'`').count();
            let run = j - i;
            if in_code == 0 {
                in_code = run;
            } else if in_code == run {
                in_code = 0;
            }
            i = j;
            continue;
        }
        if ch == b'|' && (in_code == 0 || i == n) {
            let (mut a, mut e) = (start, i);
            while a < e && (b[a] == b' ' || b[a] == b'\t') {
                a += 1;
            }
            while e > a && (b[e - 1] == b' ' || b[e - 1] == b'\t') {
                e -= 1;
            }
            cells.push((a, e));
            start = i + 1;
        }
        i += 1;
    }
    let tr = line.trim_end();
    if cells.len() > 1
        && tr.ends_with('|')
        && !tr.ends_with("\\|")
        && let Some(&(a, e)) = cells.last()
        && a == e
        && a >= tr.len()
    {
        cells.pop();
    }
    cells
}

fn is_delim_row(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || !t.contains('-') {
        return false;
    }
    let cells = split_table_row(s);
    !cells.is_empty()
        && cells.iter().all(|&(a, e)| {
            let c = s[a..e].trim();
            let c = c.strip_prefix(':').unwrap_or(c);
            let c = c.strip_suffix(':').unwrap_or(c);
            !c.is_empty() && c.bytes().all(|x| x == b'-')
        })
}

pub fn table_aligns(delim: &str) -> Vec<u8> {
    split_table_row(delim)
        .iter()
        .map(|&(a, e)| {
            let c = delim[a..e].trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => b'c',
                (false, true) => b'r',
                _ => b'l',
            }
        })
        .collect()
}

fn is_table_start(lines: &[String], i: usize) -> bool {
    if i + 1 >= lines.len() {
        return false;
    }
    let (h, d) = (&lines[i], &lines[i + 1]);
    if !h.contains('|') || !is_delim_row(d) {
        return false;
    }
    split_table_row(h).len() == split_table_row(d).len()
}

fn blank_info(kind: LineKind) -> LineInfo {
    LineInfo { kind, ..Default::default() }
}

pub fn parse_document(lines: &[String]) -> Document {
    let (infos, blocks, restart, _) = parse_from(lines, 0, None);
    Document { infos, blocks, restart }
}

/// Re-parse after an edit, in place. Lines `[start, old_end)` of the old
/// document were replaced by `[start, new_end)`; everything else is unchanged.
/// Work is proportional to the edited construct, not the document.
pub fn parse_incremental(doc: &mut Document, lines: &[String], start: usize, old_end: usize, new_end: usize) {
    let old_n = doc.infos.len();
    let delta = new_end as isize - old_end as isize;
    if old_n == 0 || lines.len() as isize != old_n as isize + delta {
        *doc = parse_document(lines);
        return;
    }
    // an opening "---" without a frontmatter block could gain its closing line
    if lines[0].trim_end() == "---" && !doc.blocks.first().is_some_and(|b| b.kind == BlockKind::Front) {
        *doc = parse_document(lines);
        return;
    }
    // restart at or before the line preceding the edit (tables look one line ahead)
    let mut r = start.saturating_sub(1).min(old_n - 1);
    while r > 0 && !doc.restart[r] {
        r -= 1;
    }
    let (mid_infos, mid_blocks, mid_restart, sync) = parse_from(lines, r, Some((doc, delta, new_end)));
    let sync_old = match sync {
        Some(s) => (s as isize - delta) as usize,
        None => old_n,
    };
    let k0 = doc.blocks.iter().position(|b| b.end > r).unwrap_or(doc.blocks.len());
    let k1 = doc.blocks[k0..].iter().position(|b| b.start >= sync_old).map_or(doc.blocks.len(), |p| p + k0);
    let nmid = mid_blocks.len();
    let shift = (k0 + nmid) as isize - k1 as isize;
    // tail fix-ups (skipped entirely in the common same-shape case)
    if shift != 0 {
        for i in &mut doc.infos[sync_old..] {
            if let Some(b) = i.block.as_mut() {
                *b = (*b as isize + shift) as usize;
            }
        }
    }
    if delta != 0 {
        for b in &mut doc.blocks[k1..] {
            b.start = (b.start as isize + delta) as usize;
            b.end = (b.end as isize + delta) as usize;
        }
    }
    let mid_infos: Vec<LineInfo> = mid_infos
        .into_iter()
        .map(|mut i| {
            i.block = i.block.map(|b| b + k0);
            i
        })
        .collect();
    // same-length replacements are assigned in place (no tail memmove)
    if mid_infos.len() == sync_old - r {
        for (dst, src) in doc.infos[r..sync_old].iter_mut().zip(mid_infos) {
            *dst = src;
        }
        doc.restart[r..sync_old].copy_from_slice(&mid_restart);
    } else {
        doc.infos.splice(r..sync_old, mid_infos);
        doc.restart.splice(r..sync_old, mid_restart);
    }
    if nmid == k1 - k0 {
        for (dst, src) in doc.blocks[k0..k1].iter_mut().zip(mid_blocks) {
            *dst = src;
        }
    } else {
        doc.blocks.splice(k0..k1, mid_blocks);
    }
}

/// A vector that grows (with defaults) when written past its end.
struct Grow<T: Default>(Vec<T>);

impl<T: Default> std::ops::Index<usize> for Grow<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}

impl<T: Default> std::ops::IndexMut<usize> for Grow<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        if i >= self.0.len() {
            self.0.resize_with(i + 1, T::default);
        }
        &mut self.0[i]
    }
}

type Parsed = (Vec<LineInfo>, Vec<Block>, Vec<bool>, Option<usize>);

/// Parse from line `from` with a fresh state. With `sync`, stop at the first
/// restart point at/after `min_line` that matches a restart point of the old
/// document; returns that line. Infos/blocks are relative to `from`
/// (block line numbers are absolute).
fn parse_from(lines: &[String], from: usize, sync: Option<(&Document, isize, usize)>) -> Parsed {
    let n_all = lines.len();
    let base = from;
    let n = n_all;
    // grown on demand: an incremental parse usually stops after a few lines
    let mut infos: Grow<LineInfo> = Grow(Vec::with_capacity(64.min(n - base)));
    let mut restart: Grow<bool> = Grow(Vec::with_capacity(64.min(n - base)));
    let mut blocks: Vec<Block> = vec![];
    let mut sync_at = None;
    let add = |infos: &mut Grow<LineInfo>, blocks: &mut Vec<Block>, kind: BlockKind, s: usize, e: usize| -> usize {
        blocks.push(Block { kind, start: s, end: e, lang: String::new(), closed: true, aligns: vec![], ncols: 0, callout_kind: String::new(), fold: 0 });
        let id = blocks.len() - 1;
        for k in s..e {
            infos[k - base].block = Some(id);
        }
        id
    };
    let mut i = from;
    if from == 0
        && n > 1
        && lines[0].trim_end() == "---"
        && let Some(j) = (1..n).find(|&j| matches!(lines[j].trim_end(), "---" | "..."))
    {
        add(&mut infos, &mut blocks, BlockKind::Front, 0, j + 1);
        infos[(0) - base].kind = LineKind::FrontFence;
        infos[(j) - base].kind = LineKind::FrontFence;
        for k in 1..j {
            infos[(k) - base].kind = LineKind::Front;
        }
        i = j + 1;
    }
    let mut list_stack: Vec<usize> = vec![];
    let mut para_open = false;
    while i < n {
        if list_stack.is_empty() && !para_open {
            restart[i - base] = true;
            if let Some((old, delta, min_line)) = sync
                && i >= min_line
                && i > from
            {
                let oi = i as isize - delta;
                if oi >= 0 && (oi as usize) < old.restart.len() && old.restart[oi as usize] {
                    sync_at = Some(i);
                    break;
                }
            }
        }
        let line = lines[i].as_str();
        let stripped = line.trim();
        infos[(i) - base].indent = indent_width(line);

        // fenced code
        if let Some((c, len, lang)) = fence_open(line) {
            let mut j = i + 1;
            let mut closed = false;
            while j < n {
                let s = lines[j].trim();
                if s.len() >= len && s.bytes().all(|x| x == c) {
                    closed = true;
                    break;
                }
                j += 1;
            }
            let end = if closed { j + 1 } else { n };
            let id = add(&mut infos, &mut blocks, BlockKind::Code, i, end);
            blocks[id].lang = lang;
            blocks[id].closed = closed;
            infos[(i) - base].kind = LineKind::Fence;
            for k in i + 1..end {
                infos[(k) - base].kind = LineKind::Code;
                infos[(k) - base].indent = indent_width(&lines[k]);
            }
            if closed {
                infos[(j) - base].kind = LineKind::Fence;
            }
            i = end;
            para_open = false;
            continue;
        }
        // display math
        if stripped.starts_with("$$") {
            if stripped == "$$" {
                let mut j = i + 1;
                while j < n && !lines[j].trim().ends_with("$$") {
                    j += 1;
                }
                let closed = j < n;
                let end = if closed { j + 1 } else { n };
                let id = add(&mut infos, &mut blocks, BlockKind::Math, i, end);
                blocks[id].closed = closed;
                for k in i..end {
                    infos[(k) - base].kind = LineKind::Math;
                }
                infos[(i) - base].kind = LineKind::MathFence;
                if closed && lines[j].trim() == "$$" {
                    infos[(j) - base].kind = LineKind::MathFence;
                }
                i = end;
                para_open = false;
                continue;
            }
            if stripped.len() > 4 && stripped.ends_with("$$") {
                add(&mut infos, &mut blocks, BlockKind::Math, i, i + 1);
                infos[(i) - base].kind = LineKind::Math;
                i += 1;
                para_open = false;
                continue;
            }
        }
        // multi-line comments
        if (stripped.starts_with("%%") && stripped.matches("%%").count() == 1) || (stripped.starts_with("<!--") && !stripped.contains("-->")) {
            let close = if stripped.starts_with("%%") { "%%" } else { "-->" };
            let mut j = i + 1;
            while j < n && !lines[j].contains(close) {
                j += 1;
            }
            let end = (j + 1).min(n);
            add(&mut infos, &mut blocks, BlockKind::Comment, i, end);
            for k in i..end {
                infos[(k) - base].kind = LineKind::Comment;
            }
            i = end;
            para_open = false;
            continue;
        }
        // tables
        if !line.trim_start().starts_with('>') && is_table_start(lines, i) {
            let mut j = i + 2;
            while j < n && !lines[j].trim().is_empty() && lines[j].contains('|') {
                j += 1;
            }
            let id = add(&mut infos, &mut blocks, BlockKind::Table, i, j);
            blocks[id].aligns = table_aligns(&lines[i + 1]);
            blocks[id].ncols = split_table_row(line).len();
            infos[(i) - base].kind = LineKind::TableHead;
            infos[(i + 1) - base].kind = LineKind::TableDelim;
            for k in i + 2..j {
                infos[(k) - base].kind = LineKind::TableRow;
            }
            i = j;
            para_open = false;
            list_stack.clear();
            continue;
        }
        if stripped.is_empty() {
            infos[(i) - base].kind = LineKind::Blank;
            para_open = false;
            i += 1;
            continue;
        }
        // blockquote / callout
        let (depth, qend) = parse_quote(line);
        if depth > 0 {
            let mut j = i;
            while j < n && parse_quote(&lines[j]).0 > 0 {
                j += 1;
            }
            let callout = parse_callout(&line[qend..]);
            let id = add(&mut infos, &mut blocks, if callout.is_some() { BlockKind::Callout } else { BlockKind::Quote }, i, j);
            let mut qstack = vec![];
            for k in i..j {
                let (d, qe) = parse_quote(&lines[k]);
                infos[(k) - base].quote_depth = d;
                infos[(k) - base].quote_end = qe;
                classify_inner(&lines[k], qe, &mut infos[(k) - base], &mut qstack);
            }
            if let Some((kind, fold, title, mend)) = callout {
                blocks[id].callout_kind = kind.clone();
                blocks[id].fold = fold;
                let hi = &mut infos[(i) - base];
                hi.kind = LineKind::CalloutHead;
                hi.lst = None;
                hi.level = 0;
                hi.content = qend + mend;
                hi.callout = Some(Callout { kind, title, fold, marker_start: qend, marker_end: qend + mend });
            }
            i = j;
            para_open = false;
            continue;
        }
        // setext underline
        if let Some(level) = setext(line)
            && para_open
            && i > 0
            && infos[(i - 1) - base].kind == LineKind::Text
            && infos[(i - 1) - base].lst.is_none()
            && list_stack.is_empty()
        {
            let prev = &mut infos[(i - 1) - base];
            prev.kind = LineKind::Heading;
            prev.level = level;
            prev.setext = true;
            prev.content = crate::text::leading_ws(&lines[i - 1]);
            infos[(i) - base].kind = LineKind::SetextRule;
            infos[(i) - base].level = level;
            add(&mut infos, &mut blocks, BlockKind::Heading, i - 1, i + 1);
            para_open = false;
            i += 1;
            continue;
        }
        classify_inner(line, 0, &mut infos[(i) - base], &mut list_stack);
        if infos[(i) - base].kind == LineKind::Heading {
            add(&mut infos, &mut blocks, BlockKind::Heading, i, i + 1);
        }
        para_open = infos[(i) - base].kind == LineKind::Text && infos[(i) - base].lst.is_none();
        i += 1;
    }
    let _ = blank_info;
    let len = match sync_at {
        Some(s) => s - base,
        None => n - base,
    };
    let (mut infos, mut restart) = (infos.0, restart.0);
    infos.resize_with(len, LineInfo::default);
    restart.resize(len, false);
    (infos, blocks, restart, sync_at)
}

/// `[!kind]+ Title` → (kind, fold, title, byte offset where title starts)
fn parse_callout(s: &str) -> Option<(String, u8, String, usize)> {
    let rest = s.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = &rest[..close];
    if kind.is_empty() || !kind.chars().next().unwrap().is_ascii_alphabetic() || !kind.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let mut k = 2 + close + 1;
    let b = s.as_bytes();
    let mut fold = 0;
    if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
        fold = b[k];
        k += 1;
    }
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    Some((kind.to_lowercase(), fold, s[k..].trim().to_string(), k))
}

fn classify_inner(line: &str, at: usize, info: &mut LineInfo, stack: &mut Vec<usize>) {
    let rest = &line[at..];
    if rest.trim().is_empty() {
        info.kind = if at == 0 { LineKind::Blank } else { LineKind::Text };
        info.content = line.len();
        return;
    }
    let indent = indent_width(rest);
    if at == 0 {
        info.indent = indent;
    }
    if let Some((level, c)) = atx(rest) {
        info.kind = LineKind::Heading;
        info.level = level;
        info.content = at + c;
        stack.clear();
        return;
    }
    if is_hr(rest) {
        info.kind = LineKind::Hr;
        info.content = line.len();
        stack.clear();
        return;
    }
    if at == 0
        && rest.starts_with("[^")
        && let Some(close) = rest.find("]:")
        && !rest[2..close].contains(' ')
        && close > 2
    {
        info.kind = LineKind::FootDef;
        let mut c = close + 2;
        if rest.as_bytes().get(c) == Some(&b' ') {
            c += 1;
        }
        info.content = c;
        return;
    }
    if let Some(mut lm) = parse_list_marker(line, at) {
        while let Some(&top) = stack.last() {
            if top > indent {
                stack.pop();
            } else {
                break;
            }
        }
        lm.depth = stack.len();
        stack.push(indent + 1);
        info.kind = LineKind::Text;
        info.content = lm.end;
        info.lst = Some(lm);
        return;
    }
    if !stack.is_empty() && indent == 0 {
        stack.clear();
    }
    info.kind = LineKind::Text;
    info.content = at + crate::text::leading_ws(rest);
}

// ---------------------------------------------------------------------------
// inline
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanKind {
    Strong,
    Em,
    Strike,
    Mark,
    Code,
    Math,
    Link,
    WikiLink,
    Image,
    Embed,
    Url,
    Tag,
    Escape,
    FootRef,
    Comment,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub kind: SpanKind,
    pub start: usize,
    pub end: usize,
    pub open_end: usize,
    pub close_start: usize,
    /// url / wikilink target / image src / tag / footnote id / math source
    pub target: String,
    /// wikilink alias, image alt
    pub label: String,
    /// wikilink heading or block anchor
    pub anchor: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub display: bool,
}

impl Span {
    fn new(kind: SpanKind, start: usize, end: usize, open_end: usize, close_start: usize) -> Span {
        Span {
            kind,
            start,
            end,
            open_end,
            close_start,
            target: String::new(),
            label: String::new(),
            anchor: String::new(),
            width: None,
            height: None,
            display: false,
        }
    }
    pub fn markers(&self) -> [(usize, usize); 2] {
        [(self.start, self.open_end), (self.close_start, self.end)]
    }
}

#[inline]
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_ascii())
}

struct Delim {
    ch: u8,
    pos: usize,
    count: usize,
    orig: usize,
    can_open: bool,
    can_close: bool,
    active: bool,
}

fn char_before(s: &str, i: usize, start: usize) -> char {
    if i <= start { ' ' } else { s[..i].chars().next_back().unwrap_or(' ') }
}
fn char_at(s: &str, i: usize, end: usize) -> char {
    if i >= end { ' ' } else { s[i..].chars().next().unwrap_or(' ') }
}

/// `[text](dest "title")` at s[i]=='[' → (text_a, text_b, dest_a, dest_b, end)
fn find_link_end(s: &str, i: usize, end: usize) -> Option<(usize, usize, usize, usize, usize)> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut j = i;
    loop {
        if j >= end {
            return None;
        }
        match b[j] {
            b'\\' => {
                j += 2;
                continue;
            }
            b'`' => {
                let k = j + b[j..end].iter().take_while(|&&c| c == b'`').count();
                let run = &s[j..k];
                j = match s[k..end].find(run) {
                    Some(p) => k + p + run.len(),
                    None => k,
                };
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j + 1 >= end || b[j + 1] != b'(' {
        return None;
    }
    let mut k = j + 2;
    while k < end && b[k] == b' ' {
        k += 1;
    }
    let (da, db);
    if k < end && b[k] == b'<' {
        let close = s[k + 1..end].find('>')? + k + 1;
        da = k + 1;
        db = close;
        k = close + 1;
    } else {
        da = k;
        let mut pd = 0;
        while k < end {
            match b[k] {
                b'\\' => {
                    k += 2;
                    continue;
                }
                b'(' => pd += 1,
                b')' => {
                    if pd == 0 {
                        break;
                    }
                    pd -= 1;
                }
                b' ' if k + 1 < end && matches!(b[k + 1], b'"' | b'\'') => break,
                _ => {}
            }
            k += 1;
        }
        db = k.min(end);
    }
    while k < end && b[k] == b' ' {
        k += 1;
    }
    if k < end && matches!(b[k], b'"' | b'\'') {
        let q = b[k] as char;
        let close = s[k + 1..end].find(q)? + k + 1;
        k = close + 1;
        while k < end && b[k] == b' ' {
            k += 1;
        }
    }
    if k >= end || b[k] != b')' {
        return None;
    }
    Some((i + 1, j, da, db, k + 1))
}

/// Obsidian size suffix `alt|300` / `alt|300x200`
fn size_suffix(t: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(bar) = t.rfind('|') {
        let size = t[bar + 1..].trim();
        let mut it = size.splitn(2, 'x');
        let w = it.next().and_then(|x| x.parse::<u32>().ok());
        let h = it.next().map(|x| x.parse::<u32>().ok());
        if let Some(w) = w {
            match h {
                None => return (&t[..bar], Some(w), None),
                Some(Some(h)) => return (&t[..bar], Some(w), Some(h)),
                Some(None) => {}
            }
        }
    }
    (t, None, None)
}

fn match_url(s: &str, i: usize, end: usize) -> Option<usize> {
    let rest = &s[i..end];
    let ok = rest.starts_with("https://") || rest.starts_with("http://") || rest.starts_with("ftp://") || rest.starts_with("www.");
    if !ok {
        return None;
    }
    let mut e = i;
    for (k, c) in rest.char_indices() {
        if c.is_whitespace() || c == '<' || c == '>' || c == '"' {
            break;
        }
        e = i + k + c.len_utf8();
    }
    let mut url = &s[i..e];
    loop {
        let last = url.chars().last()?;
        if ".,;:!?*_~'\"".contains(last) {
            url = &url[..url.len() - 1];
        } else if last == ')' && url.matches(')').count() > url.matches('(').count() {
            url = &url[..url.len() - 1];
        } else {
            break;
        }
    }
    if url.len() < 8 || (url.starts_with("www.") && !url[4..].contains('.')) {
        return None;
    }
    Some(i + url.len())
}

fn tag_end(s: &str, i: usize, end: usize) -> Option<usize> {
    // s[i] == '#'
    let mut e = i + 1;
    let mut has_non_digit = false;
    for c in s[i + 1..end].chars() {
        if c.is_whitespace() || "#!@$%^&*()=+.,;:'\"?<>{}[]|\\`~".contains(c) {
            break;
        }
        if !c.is_ascii_digit() && c != '-' {
            has_non_digit = true;
        }
        e += c.len_utf8();
    }
    if e > i + 1 && has_non_digit { Some(e) } else { None }
}

pub fn parse_inline(s: &str, start: usize, end: usize) -> Vec<Span> {
    let b = s.as_bytes();
    let mut spans: Vec<Span> = Vec::new();
    let mut delims: Vec<Delim> = Vec::new();
    let mut i = start;
    while i < end {
        let c = b[i];
        match c {
            b'\\' if i + 1 < end && b[i + 1].is_ascii_punctuation() => {
                spans.push(Span::new(SpanKind::Escape, i, i + 2, i + 1, i + 2));
                i += 2;
                continue;
            }
            b'`' => {
                let j = i + b[i..end].iter().take_while(|&&x| x == b'`').count();
                let run = j - i;
                let mut k = j;
                let mut found = None;
                while k < end {
                    if b[k] == b'`' {
                        let m = k + b[k..end].iter().take_while(|&&x| x == b'`').count();
                        if m - k == run {
                            found = Some(k);
                            break;
                        }
                        k = m;
                    } else {
                        k += 1;
                    }
                }
                match found {
                    Some(f) => {
                        spans.push(Span::new(SpanKind::Code, i, f + run, j, f));
                        i = f + run;
                    }
                    None => i = j,
                }
                continue;
            }
            b'$' => {
                if i + 1 < end && b[i + 1] == b'$' {
                    if let Some(p) = s[i + 2..end].find("$$")
                        && p > 0
                    {
                        let close = i + 2 + p;
                        let mut sp = Span::new(SpanKind::Math, i, close + 2, i + 2, close);
                        sp.display = true;
                        sp.target = s[i + 2..close].to_string();
                        spans.push(sp);
                        i = close + 2;
                        continue;
                    }
                    i += 2;
                    continue;
                }
                if i + 1 < end && !(b[i + 1] as char).is_whitespace() {
                    let mut k = i + 1;
                    let mut found = None;
                    while let Some(p) = s[k..end].find('$') {
                        let q = k + p;
                        if b[q - 1] == b'\\' {
                            k = q + 1;
                            continue;
                        }
                        if !(b[q - 1] as char).is_whitespace() && !(q + 1 < end && b[q + 1].is_ascii_digit()) {
                            found = Some(q);
                        }
                        break;
                    }
                    if let Some(f) = found
                        && f > i + 1
                    {
                        let mut sp = Span::new(SpanKind::Math, i, f + 1, i + 1, f);
                        sp.target = s[i + 1..f].to_string();
                        spans.push(sp);
                        i = f + 1;
                        continue;
                    }
                }
                i += 1;
                continue;
            }
            b'!' if s[i..end].starts_with("![[") => {
                if let Some(p) = s[i + 3..end].find("]]")
                    && p > 0
                {
                    let close = i + 3 + p;
                    let (t, w, h) = size_suffix(&s[i + 3..close]);
                    let mut sp = Span::new(SpanKind::Embed, i, close + 2, close + 2, close + 2);
                    let (tgt, anchor) = t.split_once('#').unwrap_or((t, ""));
                    sp.target = tgt.trim().to_string();
                    sp.anchor = anchor.to_string();
                    sp.width = w;
                    sp.height = h;
                    spans.push(sp);
                    i = close + 2;
                    continue;
                }
            }
            b'[' if s[i..end].starts_with("[[") => {
                if let Some(p) = s[i + 2..end].find("]]")
                    && p > 0
                {
                    let close = i + 2 + p;
                    let inner = &s[i + 2..close];
                    let (target, alias, open_end) = match inner.find('|') {
                        Some(bar) => (&inner[..bar], Some(&inner[bar + 1..]), i + 2 + bar + 1),
                        None => (inner, None, i + 2),
                    };
                    let (tgt, anchor) = target.split_once('#').unwrap_or((target, ""));
                    let mut sp = Span::new(SpanKind::WikiLink, i, close + 2, open_end, close);
                    sp.target = tgt.trim().to_string();
                    sp.anchor = anchor.to_string();
                    sp.label = alias.unwrap_or("").to_string();
                    spans.push(sp);
                    i = close + 2;
                    continue;
                }
            }
            b'!' if i + 1 < end && b[i + 1] == b'[' => {
                if let Some((ta, tb, da, db, close)) = find_link_end(s, i + 1, end) {
                    let (alt, w, h) = size_suffix(&s[ta..tb]);
                    let mut sp = Span::new(SpanKind::Image, i, close, close, close);
                    sp.label = alt.to_string();
                    sp.target = s[da..db].trim().to_string();
                    sp.width = w;
                    sp.height = h;
                    spans.push(sp);
                    i = close;
                    continue;
                }
            }
            b'[' => {
                if i + 1 < end
                    && b[i + 1] == b'^'
                    && let Some(p) = s[i + 2..end].find(']')
                {
                    let close = i + 2 + p;
                    if p > 0 && !s[i + 2..close].contains(' ') {
                        let mut sp = Span::new(SpanKind::FootRef, i, close + 1, i + 2, close);
                        sp.target = s[i + 2..close].to_string();
                        spans.push(sp);
                        i = close + 1;
                        continue;
                    }
                }
                if let Some((ta, tb, da, db, close)) = find_link_end(s, i, end) {
                    let mut sp = Span::new(SpanKind::Link, i, close, ta, tb);
                    sp.target = s[da..db].trim().to_string();
                    spans.push(sp);
                    spans.extend(parse_inline(s, ta, tb));
                    i = close;
                    continue;
                }
            }
            b'<' => {
                let rest = &s[i..end];
                if rest.starts_with("<!--") {
                    let stop = rest[4..].find("-->").map(|p| i + 4 + p + 3).unwrap_or(end);
                    spans.push(Span::new(SpanKind::Comment, i, stop, i, stop));
                    i = stop;
                    continue;
                }
                if (rest.starts_with("<http://") || rest.starts_with("<https://") || rest.starts_with("<mailto:"))
                    && let Some(p) = rest.find('>')
                    && !rest[..p].contains(' ')
                {
                    let mut sp = Span::new(SpanKind::Url, i, i + p + 1, i + 1, i + p);
                    sp.target = rest[1..p].to_string();
                    spans.push(sp);
                    i += p + 1;
                    continue;
                }
            }
            b'%' if s[i..end].starts_with("%%") => {
                let stop = s[i + 2..end].find("%%").map(|p| i + 2 + p + 2).unwrap_or(end);
                spans.push(Span::new(SpanKind::Comment, i, stop, i, stop));
                i = stop;
                continue;
            }
            b'h' | b'f' | b'w' => {
                let pc = char_before(s, i, start);
                if (!(pc.is_alphanumeric() || pc == '/' || pc == ':' || pc == '@') || i == start)
                    && let Some(e) = match_url(s, i, end)
                {
                    let mut sp = Span::new(SpanKind::Url, i, e, i, e);
                    let u = &s[i..e];
                    sp.target = if u.starts_with("www.") { format!("https://{u}") } else { u.to_string() };
                    spans.push(sp);
                    i = e;
                    continue;
                }
            }
            b'#' => {
                let pc = char_before(s, i, start);
                if (i == start || pc == ' ' || pc == '\t' || pc == '(' || pc == '[')
                    && let Some(e) = tag_end(s, i, end)
                {
                    let mut sp = Span::new(SpanKind::Tag, i, e, i, e);
                    sp.target = s[i + 1..e].to_string();
                    spans.push(sp);
                    i = e;
                    continue;
                }
            }
            b'*' | b'_' | b'~' | b'=' => {
                let j = i + b[i..end].iter().take_while(|&&x| x == c).count();
                let run = j - i;
                if (c == b'~' || c == b'=') && run != 2 {
                    i = j;
                    continue;
                }
                let before = char_before(s, i, start);
                let after = char_at(s, j, end);
                let lf = !after.is_whitespace() && (!is_punct(after) || before.is_whitespace() || is_punct(before));
                let rf = !before.is_whitespace() && (!is_punct(before) || after.is_whitespace() || is_punct(after));
                let (co, cc) = if c == b'_' { (lf && (!rf || is_punct(before)), rf && (!lf || is_punct(after))) } else { (lf, rf) };
                if co || cc {
                    delims.push(Delim { ch: c, pos: i, count: run, orig: run, can_open: co, can_close: cc, active: true });
                }
                i = j;
                continue;
            }
            _ => {}
        }
        // advance one char
        i += if c < 0x80 { 1 } else { s[i..].chars().next().map_or(1, |ch| ch.len_utf8()) };
    }
    process_emphasis(&mut delims, &mut spans);
    spans
}

fn process_emphasis(delims: &mut [Delim], spans: &mut Vec<Span>) {
    let mut ci = 0;
    while ci < delims.len() {
        if !delims[ci].can_close || !delims[ci].active || delims[ci].count == 0 {
            ci += 1;
            continue;
        }
        let mut matched = false;
        let mut oi = ci as isize - 1;
        while oi >= 0 {
            let o = oi as usize;
            let (op, cl) = (&delims[o], &delims[ci]);
            if op.active && op.ch == cl.ch && op.can_open && op.count > 0 {
                let (useq, kind);
                if op.ch == b'*' || op.ch == b'_' {
                    if (op.can_close || cl.can_open) && (op.orig + cl.orig) % 3 == 0 && !(op.orig % 3 == 0 && cl.orig % 3 == 0) {
                        oi -= 1;
                        continue;
                    }
                    useq = if op.count >= 2 && cl.count >= 2 { 2 } else { 1 };
                    kind = if useq == 2 { SpanKind::Strong } else { SpanKind::Em };
                } else {
                    if op.count < 2 || cl.count < 2 {
                        oi -= 1;
                        continue;
                    }
                    useq = 2;
                    kind = if op.ch == b'~' { SpanKind::Strike } else { SpanKind::Mark };
                }
                let o_end = op.pos + op.count;
                let cpos = cl.pos;
                spans.push(Span::new(kind, o_end - useq, cpos + useq, o_end, cpos));
                delims[o].count -= useq;
                delims[ci].pos += useq;
                delims[ci].count -= useq;
                for d in &mut delims[o + 1..ci] {
                    d.active = false;
                }
                matched = true;
                break;
            }
            oi -= 1;
        }
        if matched && delims[ci].count > 0 {
            continue;
        }
        ci += 1;
    }
}

/// Plain text of a markdown fragment with syntax removed (outline, titles).
pub fn inline_text(s: &str) -> String {
    let spans = parse_inline(s, 0, s.len());
    let mut hide = vec![false; s.len()];
    let mut repl: Vec<(usize, String)> = vec![];
    for sp in &spans {
        match sp.kind {
            SpanKind::Comment | SpanKind::Image | SpanKind::Embed => hide[sp.start..sp.end].iter_mut().for_each(|h| *h = true),
            SpanKind::WikiLink if sp.label.is_empty() => {
                hide[sp.start..sp.end].iter_mut().for_each(|h| *h = true);
                repl.push((sp.start, sp.target.clone()));
            }
            SpanKind::Tag | SpanKind::Url => {}
            _ => {
                for (a, b) in sp.markers() {
                    hide[a..b].iter_mut().for_each(|h| *h = true);
                }
            }
        }
    }
    let mut out = String::new();
    for (k, ch) in s.char_indices() {
        if let Some((_, r)) = repl.iter().find(|(p, _)| *p == k) {
            out.push_str(r);
        }
        if !hide[k] {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// (line, level, text) of each top-level heading.
pub fn outline(lines: &[String], doc: &Document) -> Vec<(usize, u8, String)> {
    let mut out = vec![];
    for (i, info) in doc.infos.iter().enumerate() {
        if info.kind == LineKind::Heading && info.quote_depth == 0 {
            let l = &lines[i];
            let t = if info.setext { l.as_str() } else { l[info.content..].trim_end().trim_end_matches('#') };
            out.push((i, info.level, inline_text(t)));
        }
    }
    out
}

pub fn slug(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_').collect::<String>().trim().to_lowercase().replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<(SpanKind, String, String)> {
        let mut v = parse_inline(s, 0, s.len());
        v.sort_by_key(|x| (x.start, std::cmp::Reverse(x.end)));
        v.iter().map(|x| (x.kind, s[x.start..x.end].to_string(), s[x.open_end..x.close_start.max(x.open_end)].to_string())).collect()
    }

    #[test]
    fn emphasis() {
        let k = kinds("**bold** and *it* ~~s~~ ==m==");
        assert_eq!(k[0], (SpanKind::Strong, "**bold**".into(), "bold".into()));
        assert_eq!(k[1], (SpanKind::Em, "*it*".into(), "it".into()));
        assert_eq!(k[2].0, SpanKind::Strike);
        assert_eq!(k[3].0, SpanKind::Mark);
        let k = kinds("***both***");
        assert_eq!(k.len(), 2);
        assert!(kinds("snake_case_word").is_empty());
        assert_eq!(kinds("_em_")[0].0, SpanKind::Em);
        assert!(kinds("2 * 3 * 4").is_empty());
        assert_eq!(kinds("*a **b** c*").len(), 2);
    }

    #[test]
    fn links_and_friends() {
        let s = "a [link **b**](http://x.com) [[Note#H|alias]] #tag `code` $x^2$ https://e.com/a_(b). ![i|300](p.png) ![[pic.png|200]] [^1]";
        let k = kinds(s);
        let names: Vec<_> = k.iter().map(|x| x.0).collect();
        assert_eq!(
            names,
            vec![
                SpanKind::Link,
                SpanKind::Strong,
                SpanKind::WikiLink,
                SpanKind::Tag,
                SpanKind::Code,
                SpanKind::Math,
                SpanKind::Url,
                SpanKind::Image,
                SpanKind::Embed,
                SpanKind::FootRef
            ]
        );
        let sp = parse_inline(s, 0, s.len());
        let w = sp.iter().find(|x| x.kind == SpanKind::WikiLink).unwrap();
        assert_eq!((w.target.as_str(), w.anchor.as_str(), w.label.as_str()), ("Note", "H", "alias"));
        let u = sp.iter().find(|x| x.kind == SpanKind::Url).unwrap();
        assert_eq!(u.target, "https://e.com/a_(b)");
        let im = sp.iter().find(|x| x.kind == SpanKind::Image).unwrap();
        assert_eq!((im.target.as_str(), im.width), ("p.png", Some(300)));
        assert!(kinds("price $5 and $10").is_empty());
        assert!(kinds("issue #12").is_empty());
        assert_eq!(kinds("x \\*not em\\*")[0].0, SpanKind::Escape);
    }

    #[test]
    fn unicode_safe() {
        let s = "héllo **wörld** 日本 *語* #タグ";
        let k = kinds(s);
        assert_eq!(k[0].1, "**wörld**");
        assert_eq!(k[1].1, "*語*");
        assert_eq!(k[2].0, SpanKind::Tag);
    }

    fn doc(s: &str) -> (Vec<String>, Document) {
        let l: Vec<String> = s.split('\n').map(String::from).collect();
        let d = parse_document(&l);
        (l, d)
    }

    #[test]
    fn blocks() {
        let (_l, d) = doc(
            "---\na: 1\n---\n# H\ntext\n- [ ] task\n  - sub\n> [!tip]- Title\n> body\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n```py\nx=1\n```\nSetext\n===\n$$\nx\n$$\n---",
        );
        let k: Vec<LineKind> = d.infos.iter().map(|i| i.kind).collect();
        use LineKind::*;
        assert_eq!(
            k,
            vec![
                FrontFence,
                Front,
                FrontFence,
                Heading,
                Text,
                Text,
                Text,
                CalloutHead,
                Text,
                Blank,
                TableHead,
                TableDelim,
                TableRow,
                Fence,
                Code,
                Fence,
                Heading,
                SetextRule,
                MathFence,
                Math,
                MathFence,
                Hr
            ]
        );
        assert_eq!(d.infos[5].lst.as_ref().unwrap().task, Some(' '));
        assert_eq!(d.infos[6].lst.as_ref().unwrap().depth, 1);
        let c = d.infos[7].callout.as_ref().unwrap();
        assert_eq!((c.kind.as_str(), c.title.as_str(), c.fold), ("tip", "Title", b'-'));
        let t = d.block_of(10).unwrap();
        assert_eq!(t.aligns, vec![b'l', b'c']);
        assert_eq!(d.block_of(13).unwrap().lang, "py");
    }

    #[test]
    fn table_cells() {
        let r = "| a `|` b | c \\| d |  |";
        let cells = split_table_row(r);
        assert_eq!(cells.len(), 3);
        assert_eq!(&r[cells[0].0..cells[0].1], "a `|` b");
        assert_eq!(&r[cells[1].0..cells[1].1], "c \\| d");
    }

    #[test]
    fn incremental_matches_full() {
        // pseudo-random edits on a mixed document must equal a full re-parse
        let base = "---\na: 1\n---\n# H\npara\ntext\n\n- a\n  - b\n\n```py\nx\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> [!note]\n> body\n\n$$\nx\n$$\nSetext\n---\n\nend";
        let snippets = ["", "```", "$$", "- item", "> quote", "| x | y |", "|---|---|", "---", "===", "# head", "text", "  - nested", "%%", "<!--", "-->"];
        let mut seed: u64 = std::env::var("GLYPH_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(12345);
        let mut rnd = |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % m
        };
        let rounds: usize = std::env::var("GLYPH_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
        for _round in 0..rounds {
            let mut buf = crate::buffer::Buffer::new(base);
            let mut doc = parse_document(&buf.lines);
            buf.take_dirty();
            for _ in 0..6 {
                let n = buf.lines.len();
                let l = rnd(n);
                let action = rnd(3);
                let snip = snippets[rnd(snippets.len())];
                match action {
                    0 => {
                        let p = crate::buffer::Pos::new(l, 0);
                        buf.insert(p, &format!("{snip}\n"));
                    }
                    1 if n > 2 => {
                        let e = (l + 1 + rnd(3)).min(n - 1);
                        buf.replace(crate::buffer::Pos::new(l, 0), crate::buffer::Pos::new(e, 0), "");
                    }
                    _ => {
                        let len = buf.lines[l].len();
                        buf.replace(crate::buffer::Pos::new(l, 0), crate::buffer::Pos::new(l, len), snip);
                    }
                }
                let Some((s, oe, ne)) = buf.take_dirty() else { continue };
                parse_incremental(&mut doc, &buf.lines, s, oe, ne);
                let full = parse_document(&buf.lines);
                assert_eq!(doc.infos, full.infos, "infos differ:\n{}", buf.text());
                assert_eq!(doc.blocks, full.blocks, "blocks differ:\n{}", buf.text());
                assert_eq!(doc.restart, full.restart);
            }
        }
    }

    #[test]
    fn plain_text() {
        assert_eq!(inline_text("**Hello** [[World]] `x`"), "Hello World x");
    }
}
