//! Kitty graphics protocol and text-sizing protocol (OSC 66) encoders.
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::diacritics::DIACRITICS;

pub const PLACEHOLDER: char = '\u{10EEEE}';
const CHUNK: usize = 4096;

fn apc(out: &mut String, ctrl: &str, payload: &str) {
    out.push_str("\x1b_G");
    out.push_str(ctrl);
    if !payload.is_empty() {
        out.push(';');
        out.push_str(payload);
    }
    out.push_str("\x1b\\");
}

/// Transmit PNG data. `virtual_place` also creates a Unicode-placeholder
/// placement of `cols`×`rows` cells.
pub fn transmit_png(out: &mut String, id: u32, png: &[u8], cols: u32, rows: u32, virtual_place: bool) {
    let data = STANDARD.encode(png);
    let n = data.len().div_ceil(CHUNK).max(1);
    for k in 0..n {
        let chunk = &data[(k * CHUNK).min(data.len())..((k + 1) * CHUNK).min(data.len())];
        let more = if k + 1 < n { 1 } else { 0 };
        let ctrl = if k == 0 {
            if virtual_place { format!("a=T,U=1,f=100,t=d,i={id},c={cols},r={rows},q=2,m={more}") } else { format!("a=t,f=100,t=d,i={id},q=2,m={more}") }
        } else {
            format!("m={more},q=2")
        };
        apc(out, &ctrl, chunk);
    }
}

/// Show a transmitted image at the cursor (cursor does not move), cropped to
/// the given source rectangle in pixels.
pub fn place(out: &mut String, id: u32, pid: u32, src: (u32, u32, u32, u32)) {
    apc(out, &format!("a=p,i={id},p={pid},C=1,q=2,x={},y={},w={},h={}", src.0, src.1, src.2, src.3), "");
}

pub fn delete_placement(out: &mut String, id: u32, pid: u32) {
    apc(out, &format!("a=d,d=i,i={id},p={pid},q=2"), "");
}

pub fn delete_image(out: &mut String, id: u32) {
    apc(out, &format!("a=d,d=I,i={id},q=2"), "");
}

pub fn delete_all(out: &mut String) {
    apc(out, "a=d,d=A,q=2", "");
}

/// Probe for graphics support: query with a 1×1 RGB pixel (nothing stored).
pub fn query(out: &mut String) {
    apc(out, "i=31,s=1,v=1,a=q,t=d,f=24", &STANDARD.encode([0u8, 0, 0]));
}

/// Text of a placeholder cell for image row/column.
pub fn placeholder_cell(row: usize, col: usize) -> String {
    let mut s = String::with_capacity(12);
    s.push(PLACEHOLDER);
    s.push(DIACRITICS[row.min(DIACRITICS.len() - 1)]);
    s.push(DIACRITICS[col.min(DIACRITICS.len() - 1)]);
    s
}

pub fn diacritic_index(c: char) -> Option<usize> {
    DIACRITICS.iter().position(|&d| d == c)
}

/// The 24-bit foreground color encoding an image id for placeholders.
pub fn id_color(id: u32) -> (u8, u8, u8) {
    ((id >> 16) as u8, (id >> 8) as u8, id as u8)
}

/// OSC 66 multicell text at `scale`.
pub fn text_sized(out: &mut String, text: &str, scale: u8) {
    out.push_str("\x1b]66;s=");
    out.push((b'0' + scale) as char);
    out.push(';');
    out.push_str(text);
    out.push('\x07');
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunking() {
        let png = vec![7u8; 10000];
        let mut s = String::new();
        transmit_png(&mut s, 42, &png, 3, 2, true);
        let parts: Vec<&str> = s.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        assert_eq!(parts.len(), 4); // 13336 b64 chars / 4096
        assert!(parts[0].starts_with("\x1b_Ga=T,U=1,f=100,t=d,i=42,c=3,r=2,q=2,m=1;"));
        assert!(parts[3].starts_with("\x1b_Gm=0,q=2;"));
        let joined: String = parts.iter().map(|p| p.split_once(';').unwrap().1).collect();
        assert_eq!(STANDARD.decode(joined).unwrap(), png);
    }
    #[test]
    fn placeholders() {
        let c = placeholder_cell(2, 5);
        let v: Vec<char> = c.chars().collect();
        assert_eq!(v[0], PLACEHOLDER);
        assert_eq!(diacritic_index(v[1]), Some(2));
        assert_eq!(diacritic_index(v[2]), Some(5));
        assert_eq!(DIACRITICS[0], '\u{305}');
        assert_eq!(id_color(0x010203), (1, 2, 3));
    }
}
