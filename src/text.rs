//! Display width, grapheme and word helpers. Columns in the editor are byte
//! offsets into a line; these helpers keep them on grapheme boundaries.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const TAB_WIDTH: usize = 4;

#[inline]
pub fn str_width(s: &str) -> usize {
    if s.is_ascii() {
        return s.bytes().filter(|b| (0x20..0x7f).contains(b)).count();
    }
    s.graphemes(true).map(grapheme_width).sum()
}

#[inline]
pub fn grapheme_width(g: &str) -> usize {
    if g == "\t" {
        return 1;
    }
    let w = UnicodeWidthStr::width(g);
    // emoji presentation sequences render double width in modern terminals
    if g.contains('\u{FE0F}') && w == 1 {
        return 2;
    }
    w.min(2)
}

/// Byte offset of the grapheme boundary after `col`.
pub fn next_boundary(s: &str, col: usize) -> usize {
    if col >= s.len() {
        return s.len();
    }
    let b = s.as_bytes()[col];
    if b < 0x80 {
        // fast path: ASCII followed by ASCII (or end) is its own grapheme, except CRLF
        let nb = s.as_bytes().get(col + 1).copied();
        if nb.map_or(true, |c| c < 0x80) {
            return col + 1;
        }
    }
    match s[col..].grapheme_indices(true).nth(1) {
        Some((i, _)) => col + i,
        None => s.len(),
    }
}

/// Byte offset of the grapheme boundary before `col`.
pub fn prev_boundary(s: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let col = col.min(s.len());
    let b = s.as_bytes()[col - 1];
    if b < 0x80 && (col < 2 || s.as_bytes()[col - 2] < 0x80) {
        return col - 1;
    }
    s[..col].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
}

pub fn snap(s: &str, col: usize) -> usize {
    let mut c = col.min(s.len());
    while c > 0 && !s.is_char_boundary(c) {
        c -= 1;
    }
    c
}

#[inline]
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if is_word_char(c) {
        1
    } else {
        2
    }
}

/// Byte range of the word (or run of same-class chars) at `col`.
pub fn word_bounds(s: &str, col: usize) -> (usize, usize) {
    if s.is_empty() {
        return (0, 0);
    }
    let mut col = snap(s, col.min(s.len()));
    let at = s[col..].chars().next();
    let before = s[..col].chars().next_back();
    if at.is_none() || (before.map_or(false, is_word_char) && !at.map_or(false, is_word_char)) {
        col = prev_boundary(s, col);
    }
    let c = s[col..].chars().next().unwrap();
    let k = class(c);
    let mut a = col;
    for (i, ch) in s[..col].char_indices().rev() {
        if class(ch) != k {
            break;
        }
        a = i;
    }
    let mut b = col;
    for (i, ch) in s[col..].char_indices() {
        if class(ch) != k {
            break;
        }
        b = col + i + ch.len_utf8();
    }
    (a, b)
}

pub fn next_word(s: &str, col: usize) -> usize {
    let mut it = s[col.min(s.len())..].char_indices().peekable();
    let base = col.min(s.len());
    // skip punctuation run
    let mut pos = base;
    let mut seen_punct = false;
    while let Some(&(i, c)) = it.peek() {
        if class(c) == 2 {
            seen_punct = true;
            pos = base + i + c.len_utf8();
            it.next();
        } else {
            break;
        }
    }
    if seen_punct {
        return pos;
    }
    while let Some(&(i, c)) = it.peek() {
        if c.is_whitespace() {
            it.next();
            pos = base + i + c.len_utf8();
        } else {
            break;
        }
    }
    while let Some(&(i, c)) = it.peek() {
        if is_word_char(c) {
            it.next();
            pos = base + i + c.len_utf8();
        } else {
            break;
        }
    }
    if pos == base && base < s.len() {
        return next_boundary(s, base);
    }
    pos
}

pub fn prev_word(s: &str, col: usize) -> usize {
    let col = col.min(s.len());
    let mut chars = s[..col].char_indices().rev().peekable();
    let mut pos = col;
    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() {
            pos = i;
            chars.next();
        } else {
            break;
        }
    }
    if let Some(&(_, c)) = chars.peek() {
        let k = class(c);
        while let Some(&(i, c)) = chars.peek() {
            if class(c) == k {
                pos = i;
                chars.next();
            } else {
                break;
            }
        }
    }
    pos
}

/// Truncate to `width` cells, appending `…` when cut.
pub fn truncate(s: &str, width: usize) -> String {
    if str_width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for g in s.graphemes(true) {
        let gw = grapheme_width(g);
        if w + gw > width - 1 {
            break;
        }
        out.push_str(g);
        w += gw;
    }
    out.push('…');
    out
}

pub fn pad_to(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let w = str_width(&t);
    let mut out = t;
    out.extend(std::iter::repeat(' ').take(width.saturating_sub(w)));
    out
}

pub fn leading_ws(s: &str) -> usize {
    s.len() - s.trim_start_matches([' ', '\t']).len()
}

pub fn indent_width(s: &str) -> usize {
    let mut w = 0;
    for c in s.chars() {
        match c {
            ' ' => w += 1,
            '\t' => w += TAB_WIDTH - w % TAB_WIDTH,
            _ => break,
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn widths() {
        assert_eq!(str_width("abc"), 3);
        assert_eq!(str_width("日本"), 4);
        assert_eq!(str_width("e\u{301}"), 1);
        assert_eq!(str_width("👍"), 2);
    }
    #[test]
    fn boundaries() {
        let s = "ae\u{301}b日";
        assert_eq!(next_boundary(s, 0), 1);
        assert_eq!(next_boundary(s, 1), 4);
        assert_eq!(prev_boundary(s, 4), 1);
        assert_eq!(next_boundary(s, 5), 8);
        assert_eq!(prev_boundary(s, 8), 5);
    }
    #[test]
    fn words() {
        let s = "hello, world  foo";
        assert_eq!(next_word(s, 0), 5);
        assert_eq!(next_word(s, 5), 6);
        assert_eq!(next_word(s, 6), 12);
        assert_eq!(prev_word(s, 17), 14);
        assert_eq!(prev_word(s, 14), 7);
        assert_eq!(word_bounds(s, 2), (0, 5));
        assert_eq!(word_bounds(s, 5), (0, 5));
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
