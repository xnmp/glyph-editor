//! Terminal input decoding: keys (legacy + kitty keyboard protocol), SGR
//! mouse, bracketed paste, focus, and capability-query replies.

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;
pub const SUPER: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// "a", "A", "enter", "left", "f1", "space", ...
    pub key: String,
    pub mods: u8,
    /// text to insert when this is a plain printable key
    pub text: String,
}

impl Key {
    pub fn new(key: &str, mods: u8) -> Key {
        Key { key: key.to_string(), mods, text: String::new() }
    }
    pub fn text(t: &str) -> Key {
        let c = t.chars().next().unwrap_or(' ');
        let mods = if c.is_uppercase() { SHIFT } else { 0 };
        Key { key: t.to_string(), mods, text: t.to_string() }
    }
    /// Canonical binding name, e.g. "ctrl+shift+left", "ctrl+s", "f1".
    pub fn name(&self) -> String {
        let mut s = String::new();
        if self.mods & CTRL != 0 {
            s.push_str("ctrl+");
        }
        if self.mods & ALT != 0 {
            s.push_str("alt+");
        }
        if self.mods & SUPER != 0 {
            s.push_str("super+");
        }
        let printable = self.key.chars().count() == 1;
        if self.mods & SHIFT != 0 && !(printable && self.mods & (CTRL | ALT | SUPER) == 0) {
            s.push_str("shift+");
        }
        if printable && self.mods & (CTRL | ALT | SUPER) != 0 {
            s.push_str(&self.key.to_lowercase());
        } else {
            s.push_str(&self.key);
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Drag,
    Move,
    Wheel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    /// 0 left, 1 middle, 2 right; wheel: 0 up, 1 down, 2 left, 3 right
    pub button: u8,
    pub x: u16,
    pub y: u16,
    pub mods: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Cpr(u16, u16),
    Da1,
    KbdFlags,
    Graphics(String),
    XtVersion(String),
    CellSize(u16, u16),
    Osc(String),
    Focus(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    Reply(Reply),
    Resize,
}

fn kitty_functional(code: u32) -> Option<&'static str> {
    Some(match code {
        9 => "tab",
        13 => "enter",
        27 => "escape",
        127 => "backspace",
        32 => "space",
        57399 => "0", 57400 => "1", 57401 => "2", 57402 => "3", 57403 => "4", 57404 => "5",
        57405 => "6", 57406 => "7", 57407 => "8", 57408 => "9", 57409 => ".", 57410 => "/",
        57411 => "*", 57412 => "-", 57413 => "+", 57414 => "enter", 57415 => "=",
        57417 => "left", 57418 => "right", 57419 => "up", 57420 => "down", 57421 => "pageup",
        57422 => "pagedown", 57423 => "home", 57424 => "end", 57425 => "insert", 57426 => "delete",
        _ => return None,
    })
}

fn tilde(code: u32) -> Option<&'static str> {
    Some(match code {
        1 | 7 => "home",
        2 => "insert",
        3 => "delete",
        4 | 8 => "end",
        5 => "pageup",
        6 => "pagedown",
        11 => "f1", 12 => "f2", 13 => "f3", 14 => "f4", 15 => "f5", 17 => "f6", 18 => "f7",
        19 => "f8", 20 => "f9", 21 => "f10", 23 => "f11", 24 => "f12",
        _ => return None,
    })
}

fn letter(c: u8) -> Option<&'static str> {
    Some(match c {
        b'A' => "up",
        b'B' => "down",
        b'C' => "right",
        b'D' => "left",
        b'H' => "home",
        b'F' => "end",
        b'P' => "f1",
        b'Q' => "f2",
        b'R' => "f3",
        b'S' => "f4",
        _ => return None,
    })
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    paste: Option<Vec<u8>>,
    pub expect_cpr: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    pub fn pending(&self) -> bool {
        !self.buf.is_empty()
    }
    pub fn in_paste(&self) -> bool {
        self.paste.is_some()
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        self.parse(false)
    }

    /// No further input arrived quickly: resolve a lone ESC / partial sequence.
    pub fn flush(&mut self) -> Vec<Event> {
        self.parse(true)
    }

    fn parse(&mut self, fin: bool) -> Vec<Event> {
        let mut ev = vec![];
        let s = std::mem::take(&mut self.buf);
        let n = s.len();
        let mut i = 0;
        while i < n {
            if let Some(p) = self.paste.as_mut() {
                const END: &[u8] = b"\x1b[201~";
                match find(&s[i..], END) {
                    Some(k) => {
                        p.extend_from_slice(&s[i..i + k]);
                        let text = String::from_utf8_lossy(p).replace("\r\n", "\n").replace('\r', "\n");
                        ev.push(Event::Paste(text));
                        self.paste = None;
                        i += k + END.len();
                    }
                    None => {
                        // keep a possible partial terminator for next time
                        let mut keep = 0;
                        for k in 1..END.len() {
                            if s[i..].ends_with(&END[..k]) {
                                keep = k;
                            }
                        }
                        p.extend_from_slice(&s[i..n - keep]);
                        i = n - keep;
                        break;
                    }
                }
                continue;
            }
            let c = s[i];
            if c == 0x1b {
                if i + 1 >= n {
                    if fin {
                        ev.push(Event::Key(Key::new("escape", 0)));
                        i += 1;
                        continue;
                    }
                    break;
                }
                let nx = s[i + 1];
                match nx {
                    b'[' => match self.csi(&s, i + 2, fin) {
                        None => break,
                        Some((j, e)) => {
                            if let Some(e) = e {
                                ev.push(e);
                            }
                            i = j;
                            continue;
                        }
                    },
                    b'O' => {
                        if i + 2 >= n {
                            if fin {
                                ev.push(Event::Key(Key::new("o", ALT)));
                                i += 2;
                                continue;
                            }
                            break;
                        }
                        match letter(s[i + 2]) {
                            Some(name) => ev.push(Event::Key(Key::new(name, 0))),
                            None => ev.push(Event::Key(Key::new(&(s[i + 2] as char).to_string(), ALT))),
                        }
                        i += 3;
                        continue;
                    }
                    b'_' | b'P' | b']' => {
                        // string sequence terminated by ST or BEL
                        let mut j = i + 2;
                        let mut end = None;
                        while j < n {
                            if s[j] == 0x07 {
                                end = Some((j, 1));
                                break;
                            }
                            if s[j] == 0x1b && j + 1 < n && s[j + 1] == b'\\' {
                                end = Some((j, 2));
                                break;
                            }
                            j += 1;
                        }
                        let Some((e, stl)) = end else {
                            if fin && n - i > 1 << 16 {
                                i = n;
                            }
                            break;
                        };
                        let body = String::from_utf8_lossy(&s[i + 2..e]).to_string();
                        i = e + stl;
                        if nx == b'_' && body.starts_with('G') {
                            ev.push(Event::Reply(Reply::Graphics(body[1..].to_string())));
                        } else if nx == b'P' && body.starts_with(">|") {
                            ev.push(Event::Reply(Reply::XtVersion(body[2..].to_string())));
                        } else if nx == b']' {
                            ev.push(Event::Reply(Reply::Osc(body)));
                        }
                        continue;
                    }
                    0x1b => {
                        ev.push(Event::Key(Key::new("escape", 0)));
                        i += 1;
                        continue;
                    }
                    _ => {
                        let (k, len) = match decode_char(&s[i + 1..]) {
                            Some(x) => x,
                            None if !fin => break,
                            None => ('?', 1),
                        };
                        let mut key = single(k);
                        key.mods |= ALT;
                        key.text.clear();
                        if key.key.chars().count() == 1 {
                            key.key = key.key.to_lowercase();
                            if k.is_uppercase() {
                                key.mods |= SHIFT;
                            }
                        }
                        ev.push(Event::Key(key));
                        i += 1 + len;
                        continue;
                    }
                }
            }
            match decode_char(&s[i..]) {
                Some((ch, len)) => {
                    ev.push(Event::Key(single(ch)));
                    i += len;
                }
                None => {
                    if fin || n - i >= 4 {
                        i += 1; // invalid byte
                    } else {
                        break;
                    }
                }
            }
        }
        self.buf = s[i..].to_vec();
        ev
    }

    fn csi(&mut self, s: &[u8], j: usize, fin: bool) -> Option<(usize, Option<Event>)> {
        let n = s.len();
        let mut k = j;
        while k < n && (0x30..=0x3f).contains(&s[k]) {
            k += 1;
        }
        while k < n && (0x20..=0x2f).contains(&s[k]) {
            k += 1;
        }
        if k >= n {
            if fin {
                return Some((n, Some(Event::Key(Key::new("[", ALT)))));
            }
            return None;
        }
        let f = s[k];
        let params = std::str::from_utf8(&s[j..k]).unwrap_or("");
        let end = k + 1;
        if let Some(p) = params.strip_prefix('<') {
            if f == b'M' || f == b'm' {
                let v: Vec<u32> = p.split(';').filter_map(|x| x.parse().ok()).collect();
                if v.len() != 3 {
                    return Some((end, None));
                }
                let (b, x, y) = (v[0], v[1].saturating_sub(1) as u16, v[2].saturating_sub(1) as u16);
                let mods = (if b & 4 != 0 { SHIFT } else { 0 }) | (if b & 8 != 0 { ALT } else { 0 }) | (if b & 16 != 0 { CTRL } else { 0 });
                let btn = (b & 3) as u8;
                let kind = if b & 64 != 0 {
                    MouseKind::Wheel
                } else if b & 32 != 0 {
                    if btn == 3 { MouseKind::Move } else { MouseKind::Drag }
                } else if f == b'M' {
                    MouseKind::Press
                } else {
                    MouseKind::Release
                };
                return Some((end, Some(Event::Mouse(Mouse { kind, button: btn, x, y, mods }))));
            }
        }
        if let Some(_p) = params.strip_prefix('?') {
            return Some((
                end,
                match f {
                    b'u' => Some(Event::Reply(Reply::KbdFlags)),
                    b'c' => Some(Event::Reply(Reply::Da1)),
                    _ => None,
                },
            ));
        }
        if params.starts_with('>') || params.starts_with('=') {
            return Some((end, None));
        }
        if params.is_empty() && f == b'I' {
            return Some((end, Some(Event::Reply(Reply::Focus(true)))));
        }
        if params.is_empty() && f == b'O' {
            return Some((end, Some(Event::Reply(Reply::Focus(false)))));
        }
        let parts: Vec<&str> = params.split(';').collect();
        let num = |idx: usize| -> u32 { parts.get(idx).and_then(|p| p.split(':').next()).and_then(|x| x.parse().ok()).unwrap_or(0) };
        let mods_of = |idx: usize| -> u8 { (num(idx).saturating_sub(1) as u8) & (SHIFT | ALT | CTRL | SUPER) };
        let event_type = |idx: usize| -> u32 { parts.get(idx).and_then(|p| p.split(':').nth(1)).and_then(|x| x.parse().ok()).unwrap_or(1) };
        match f {
            b't' => {
                if num(0) == 6 && parts.len() == 3 {
                    return Some((end, Some(Event::Reply(Reply::CellSize(num(2) as u16, num(1) as u16)))));
                }
                Some((end, None))
            }
            b'R' if self.expect_cpr && parts.len() == 2 => Some((end, Some(Event::Reply(Reply::Cpr(num(0) as u16, num(1) as u16))))),
            b'u' => {
                let code = num(0);
                if event_type(1) == 3 || (57358..=57360).contains(&code) || (57441..=57452).contains(&code) {
                    return Some((end, None));
                }
                let mods = mods_of(1);
                if let Some(name) = kitty_functional(code) {
                    let printable = name.len() == 1 && code >= 57399;
                    let mut k = Key::new(name, mods);
                    if name == "space" && mods & (CTRL | ALT | SUPER) == 0 {
                        k.text = " ".into();
                    } else if printable && mods & (CTRL | ALT | SUPER) == 0 {
                        k.text = name.into();
                    }
                    return Some((end, Some(Event::Key(k))));
                }
                if code < 32 || (57344..=63743).contains(&code) {
                    return Some((end, None));
                }
                let Some(ch) = char::from_u32(code) else { return Some((end, None)) };
                let mut k = Key::new(&ch.to_string(), mods);
                if mods & (CTRL | ALT | SUPER) == 0 {
                    let t: String = if mods & SHIFT != 0 { ch.to_uppercase().collect() } else { ch.to_string() };
                    k.key = t.clone();
                    k.text = t;
                }
                Some((end, Some(Event::Key(k))))
            }
            b'~' => {
                let code = num(0);
                if code == 200 {
                    self.paste = Some(vec![]);
                    return Some((end, None));
                }
                Some((end, tilde(code).map(|name| Event::Key(Key::new(name, mods_of(1))))))
            }
            b'Z' => Some((end, Some(Event::Key(Key::new("tab", SHIFT))))),
            _ => Some((end, letter(f).map(|name| Event::Key(Key::new(name, mods_of(1)))))),
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn decode_char(s: &[u8]) -> Option<(char, usize)> {
    let len = match s.first()? {
        b if *b < 0x80 => 1,
        b if *b >> 5 == 0b110 => 2,
        b if *b >> 4 == 0b1110 => 3,
        b if *b >> 3 == 0b11110 => 4,
        _ => return Some(('\u{fffd}', 1)),
    };
    if s.len() < len {
        return None;
    }
    match std::str::from_utf8(&s[..len]) {
        Ok(t) => Some((t.chars().next().unwrap(), len)),
        Err(_) => Some(('\u{fffd}', 1)),
    }
}

fn single(c: char) -> Key {
    match c {
        '\r' | '\n' => Key::new("enter", 0),
        '\t' => Key::new("tab", 0),
        '\x7f' => Key::new("backspace", 0),
        '\x08' => Key::new("backspace", CTRL),
        '\0' => Key::new("space", CTRL),
        '\x1c' => Key::new("\\", CTRL),
        '\x1d' => Key::new("]", CTRL),
        '\x1e' => Key::new("^", CTRL),
        '\x1f' => Key::new("/", CTRL),
        c if (c as u32) >= 1 && (c as u32) <= 26 => Key::new(&((c as u8 + 96) as char).to_string(), CTRL),
        ' ' => Key { key: "space".into(), mods: 0, text: " ".into() },
        c => Key::text(&c.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(p: &mut Parser, b: &[u8]) -> Vec<String> {
        p.feed(b)
            .into_iter()
            .map(|e| match e {
                Event::Key(k) => k.name(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn legacy_and_kitty_keys() {
        let mut p = Parser::new();
        assert_eq!(keys(&mut p, b"a"), ["a"]);
        assert_eq!(keys(&mut p, b"\x1b[97;5u"), ["ctrl+a"]);
        assert_eq!(keys(&mut p, b"\x1b[105;5u"), ["ctrl+i"]);
        assert_eq!(keys(&mut p, b"\x1b[1;5C"), ["ctrl+right"]);
        assert_eq!(keys(&mut p, b"\x1b[1;2D"), ["shift+left"]);
        assert_eq!(keys(&mut p, b"\x1b[3~"), ["delete"]);
        assert_eq!(keys(&mut p, b"\x1b[27u"), ["escape"]);
        assert_eq!(keys(&mut p, b"\x1b[13;2u"), ["shift+enter"]);
        assert_eq!(keys(&mut p, b"\x1bb"), ["alt+b"]);
        assert_eq!(keys(&mut p, b"\x13"), ["ctrl+s"]);
        assert_eq!(keys(&mut p, b"\x1b[115;6u"), ["ctrl+shift+s"]);
        assert_eq!(keys(&mut p, b"\x1b[Z"), ["shift+tab"]);
        assert_eq!(keys(&mut p, b"\x1b[97;1:3u"), Vec::<String>::new()); // release ignored
        assert_eq!(keys(&mut p, b"A"), ["A"]);
    }

    #[test]
    fn split_utf8_and_escape() {
        let mut p = Parser::new();
        assert!(keys(&mut p, b"\xc3").is_empty());
        assert_eq!(keys(&mut p, b"\xa9"), ["é"]);
        assert!(keys(&mut p, b"\x1b").is_empty());
        let e = p.flush();
        assert_eq!(e, vec![Event::Key(Key::new("escape", 0))]);
        assert!(keys(&mut p, b"\x1b[1;5").is_empty());
        assert_eq!(keys(&mut p, b"A"), ["ctrl+up"]);
    }

    #[test]
    fn mouse_paste_replies() {
        let mut p = Parser::new();
        assert_eq!(p.feed(b"\x1b[<0;10;5M"), vec![Event::Mouse(Mouse { kind: MouseKind::Press, button: 0, x: 9, y: 4, mods: 0 })]);
        assert_eq!(p.feed(b"\x1b[<65;1;1M")[0], Event::Mouse(Mouse { kind: MouseKind::Wheel, button: 1, x: 0, y: 0, mods: 0 }));
        assert_eq!(p.feed(b"\x1b[<32;3;3M")[0], Event::Mouse(Mouse { kind: MouseKind::Drag, button: 0, x: 2, y: 2, mods: 0 }));
        assert_eq!(p.feed(b"\x1b[200~hello\r\nwor"), vec![]);
        assert_eq!(p.feed(b"ld\x1b[20"), vec![]);
        assert_eq!(p.feed(b"1~x"), vec![Event::Paste("hello\nworld".into()), Event::Key(Key::text("x"))]);
        assert_eq!(p.feed(b"\x1b_Gi=31;OK\x1b\\"), vec![Event::Reply(Reply::Graphics("i=31;OK".into()))]);
        assert_eq!(p.feed(b"\x1b[?1u\x1b[?62;4c"), vec![Event::Reply(Reply::KbdFlags), Event::Reply(Reply::Da1)]);
        assert_eq!(p.feed(b"\x1b[6;20;10t"), vec![Event::Reply(Reply::CellSize(10, 20))]);
        assert_eq!(p.feed(b"\x1bP>|kitty(0.43)\x1b\\"), vec![Event::Reply(Reply::XtVersion("kitty(0.43)".into()))]);
        p.expect_cpr = true;
        assert_eq!(p.feed(b"\x1b[5;7R"), vec![Event::Reply(Reply::Cpr(5, 7))]);
        assert_eq!(p.feed(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07"), vec![Event::Reply(Reply::Osc("11;rgb:1e1e/1e1e/1e1e".into()))]);
    }
}
