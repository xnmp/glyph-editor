//! The real terminal: raw mode, modes, size, capability probing, signals.

use std::io::Write;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use crate::graphics as gfx;
use crate::input::{Event, Parser, Reply};
use crate::theme::Palette;

#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub graphics: bool,
    pub placeholders: bool,
    pub text_sizing: bool,
    pub kitty_keyboard: bool,
    /// SGR-Pixels mouse reporting (mode 1016): exact pointer positions
    pub pixel_mouse: bool,
    pub cell_w: u32,
    pub cell_h: u32,
    pub name: String,
    pub palette: Palette,
}

static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static RESIZED: AtomicI32 = AtomicI32::new(0);
static TERM_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(1, Ordering::SeqCst);
    let fd = WAKE_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            libc::write(fd, b"w".as_ptr() as *const _, 1);
        }
    }
}

pub struct Terminal {
    pub fd: RawFd,
    saved: Option<libc::termios>,
    wake_r: RawFd,
    wake_w: RawFd,
    pub parser: Parser,
    pub active: bool,
}

const ENTER: &str = "\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[>1u\x1b[H\x1b[2J";

fn leave_seq() -> String {
    let mut s = String::new();
    gfx::delete_all(&mut s);
    s.push_str("\x1b[<u\x1b[?1016l\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[0 q\x1b[?25h\x1b[?7h\x1b[?1049l");
    s
}

impl Terminal {
    pub fn open() -> std::io::Result<Terminal> {
        let path = std::ffi::CString::new("/dev/tty").unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        let fd = if fd < 0 { 0 } else { fd };
        if unsafe { libc::isatty(fd) } == 0 {
            return Err(std::io::Error::other("not a terminal"));
        }
        let mut p = [0i32; 2];
        unsafe {
            libc::pipe(p.as_mut_ptr());
            for f in p {
                let fl = libc::fcntl(f, libc::F_GETFL);
                libc::fcntl(f, libc::F_SETFL, fl | libc::O_NONBLOCK);
                libc::fcntl(f, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        WAKE_FD.store(p[1], Ordering::SeqCst);
        unsafe {
            libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        }
        Ok(Terminal { fd, saved: None, wake_r: p[0], wake_w: p[1], parser: Parser::new(), active: false })
    }

    pub fn waker(&self) -> std::sync::Arc<dyn Fn() + Send + Sync> {
        let fd = self.wake_w;
        std::sync::Arc::new(move || unsafe {
            libc::write(fd, b"x".as_ptr() as *const _, 1);
        })
    }

    pub fn write(&self, s: &str) {
        let mut b = s.as_bytes();
        while !b.is_empty() {
            let n = unsafe { libc::write(self.fd, b.as_ptr() as *const _, b.len()) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted || e.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                return;
            }
            b = &b[n as usize..];
        }
    }

    pub fn size(&self) -> (usize, usize, u32, u32) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize, ws.ws_xpixel as u32, ws.ws_ypixel as u32);
        }
        (80, 24, 0, 0)
    }

    pub fn enter(&mut self) {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(self.fd, &mut t) == 0 {
                self.saved = Some(t);
                let mut raw = t;
                libc::cfmakeraw(&mut raw);
                raw.c_cc[libc::VMIN] = 1;
                raw.c_cc[libc::VTIME] = 0;
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, &raw);
            }
        }
        TERM_FD.store(self.fd, Ordering::SeqCst);
        self.write(ENTER);
        self.active = true;
    }

    pub fn leave(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.write(&leave_seq());
        if let Some(t) = self.saved {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, &t);
            }
        }
    }

    /// Report mouse positions in pixels (when supported) for precise
    /// placement inside proportional text.
    pub fn enable_pixel_mouse(&mut self) {
        self.write("\x1b[?1016h");
    }

    pub fn suspend(&mut self) {
        self.leave();
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTSTP);
        }
        self.enter();
    }

    pub fn take_resized(&self) -> bool {
        RESIZED.swap(0, Ordering::SeqCst) == 1
    }

    /// Wait up to `timeout` for input; returns decoded events.
    pub fn read(&mut self, timeout: Option<Duration>) -> Vec<Event> {
        let mut fds = [libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: self.wake_r, events: libc::POLLIN, revents: 0 }];
        let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        let mut ev = vec![];
        if r <= 0 {
            if self.parser.pending() && !self.parser.in_paste() {
                ev.extend(self.parser.flush());
            }
            return ev;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 256];
            unsafe {
                libc::read(self.wake_r, buf.as_mut_ptr() as *mut _, buf.len());
            }
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let mut buf = vec![0u8; 1 << 16];
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                ev.extend(self.parser.feed(&buf[..n as usize]));
                if self.parser.pending() && !self.parser.in_paste() {
                    // give the rest of an escape sequence a moment to arrive
                    let mut f = [libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 }];
                    if unsafe { libc::poll(f.as_mut_ptr(), 1, 25) } > 0 {
                        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                        if n > 0 {
                            ev.extend(self.parser.feed(&buf[..n as usize]));
                        }
                    }
                    if self.parser.pending() && !self.parser.in_paste() {
                        ev.extend(self.parser.flush());
                    }
                }
            } else if n == 0 {
                ev.push(Event::Key(crate::input::Key::new("hangup", 0)));
            }
        }
        ev
    }

    /// Probe terminal capabilities. Returns non-reply events that arrived meanwhile.
    pub fn probe(&mut self, timeout: Duration) -> (Caps, Vec<Event>) {
        let mut caps = Caps::default();
        let (cols, rows, xp, yp) = self.size();
        if xp > 0 && yp > 0 {
            caps.cell_w = xp / cols as u32;
            caps.cell_h = yp / rows as u32;
        }
        let mut q = String::from("\x1b[H");
        gfx::query(&mut q);
        q.push_str("\x1b[?u\x1b[>0q\x1b[16t");
        // text sizing (per the spec): CPR, w=2 space, CPR, s=2 space, CPR
        q.push_str("\r\x1b[6n\x1b]66;w=2; \x07\x1b[6n\x1b]66;s=2; \x07\x1b[6n");
        // terminal colors
        q.push_str("\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
        for i in 0..8 {
            q.push_str(&format!("\x1b]4;{i};?\x1b\\"));
        }
        q.push_str("\x1b[?1016$p"); // DECRQM: SGR-Pixels mouse?
        q.push_str("\x1b[c");
        self.parser.expect_cpr = true;
        self.write(&q);
        let deadline = Instant::now() + timeout;
        let mut cprs = vec![];
        let mut other = vec![];
        let mut got_da1 = false;
        while !got_da1 {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            for e in self.read(Some(deadline - now)) {
                match e {
                    Event::Reply(r) => match r {
                        Reply::Graphics(s) => {
                            if s.contains("OK") {
                                caps.graphics = true;
                            }
                        }
                        Reply::KbdFlags => caps.kitty_keyboard = true,
                        Reply::XtVersion(s) => caps.name = s,
                        Reply::CellSize(w, h) => {
                            if w > 0 && h > 0 {
                                caps.cell_w = w as u32;
                                caps.cell_h = h as u32;
                            }
                        }
                        Reply::Cpr(r, c) => cprs.push((r, c)),
                        Reply::Osc(s) => parse_color_reply(&s, &mut caps.palette),
                        Reply::Da1 => got_da1 = true,
                        Reply::Mode(1016, v) => caps.pixel_mouse = v == 1 || v == 2,
                        Reply::Mode(..) => {}
                        Reply::Focus(_) => {}
                    },
                    e => other.push(e),
                }
            }
        }
        self.parser.expect_cpr = false;
        // width support moves the cursor by 2, scale support by another 2
        if cprs.len() >= 3 && cprs[1].1 == cprs[0].1 + 2 && cprs[2].1 == cprs[1].1 + 2 {
            caps.text_sizing = true;
        }
        let name = caps.name.to_lowercase();
        let term = std::env::var("TERM").unwrap_or_default();
        let kitty = name.contains("kitty") || term == "xterm-kitty" || std::env::var_os("KITTY_WINDOW_ID").is_some();
        let ghostty =
            name.contains("ghostty") || term == "xterm-ghostty" || std::env::var("TERM_PROGRAM").map(|t| t.eq_ignore_ascii_case("ghostty")).unwrap_or(false);
        if caps.graphics && (kitty || ghostty) {
            caps.placeholders = true;
        }
        if caps.cell_w == 0 || caps.cell_h == 0 {
            caps.cell_w = 10;
            caps.cell_h = 20;
        }
        self.write("\r\x1b[2K\x1b[2;1H\x1b[2K\x1b[H\x1b[2J");
        (caps, other)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Parse "10;rgb:ffff/ffff/ffff" / "4;3;rgb:..." replies.
pub fn parse_color_reply(s: &str, p: &mut Palette) {
    let Some((head, spec)) = s.rsplit_once(";rgb:") else { return };
    let comps: Vec<u8> = spec
        .trim_end_matches(['\x07', '\x1b', '\\'])
        .split('/')
        .filter_map(|c| {
            let v = u32::from_str_radix(c, 16).ok()?;
            let max = (1u32 << (4 * c.len() as u32)) - 1;
            Some(((v * 255 + max / 2) / max.max(1)) as u8)
        })
        .collect();
    if comps.len() != 3 {
        return;
    }
    let rgb = (comps[0], comps[1], comps[2]);
    match head {
        "10" => p.fg = Some(rgb),
        "11" => p.bg = Some(rgb),
        h if h.starts_with("4;") => {
            if let Ok(i) = h[2..].parse::<usize>()
                && i < 16
            {
                p.ansi[i] = Some(rgb);
            }
        }
        _ => {}
    }
}

/// Restore the terminal on panic so the user isn't left in raw mode.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let fd = TERM_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            let s = leave_seq();
            unsafe {
                libc::write(fd, s.as_ptr() as *const _, s.len());
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut t) == 0 {
                    t.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN;
                    t.c_iflag |= libc::ICRNL;
                    t.c_oflag |= libc::OPOST;
                    libc::tcsetattr(fd, libc::TCSAFLUSH, &t);
                }
            }
        }
        let _ = std::io::stderr().flush();
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn color_replies() {
        let mut p = Palette::default();
        parse_color_reply("11;rgb:1e1e/1e1e/1e1e", &mut p);
        parse_color_reply("10;rgb:ff/80/00", &mut p);
        parse_color_reply("4;5;rgb:8a8a/5c5c/f5f5", &mut p);
        assert_eq!(p.bg, Some((30, 30, 30)));
        assert_eq!(p.fg, Some((255, 128, 0)));
        assert_eq!(p.ansi[5], Some((138, 92, 245)));
    }
}
