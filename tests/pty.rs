//! Runs the real binary inside a pseudo-terminal, answering its capability
//! probe like kitty does, and drives it with raw key bytes.

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glyph::vt::Vt;

struct Pty {
    master: std::fs::File,
    out: Arc<Mutex<Vec<u8>>>,
    child: std::process::Child,
}

fn set_size(fd: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: cols * 10, ws_ypixel: rows * 20 };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

fn spawn(args: &[&str], dir: &std::path::Path, cols: u16, rows: u16) -> Pty {
    spawn_env(args, dir, cols, rows, &[])
}

fn spawn_env(args: &[&str], dir: &std::path::Path, cols: u16, rows: u16, env: &[(&str, &str)]) -> Pty {
    let (mut m, mut s) = (0, 0);
    unsafe {
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()), 0);
    }
    set_size(m, cols, rows);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_glyph"));
    cmd.args(args).current_dir(dir).env("TERM", "xterm-kitty").env_remove("GLYPH_NO_PROBE").env("HOME", dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    unsafe {
        cmd.stdin(Stdio::from_raw_fd(libc::dup(s))).stdout(Stdio::from_raw_fd(libc::dup(s))).stderr(Stdio::from_raw_fd(libc::dup(s)));
        cmd.pre_exec(move || {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().expect("spawn glyph");
    unsafe {
        libc::close(s);
    }
    let master = unsafe { std::fs::File::from_raw_fd(m) };
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut reader = master.try_clone().unwrap();
    let o2 = out.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => o2.lock().unwrap().extend_from_slice(&buf[..n]),
            }
        }
    });
    Pty { master, out, child }
}

impl Pty {
    fn wait_for(&self, needle: &[u8], timeout: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if self.out.lock().unwrap().windows(needle.len()).any(|w| w == needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
    fn send(&mut self, b: &[u8]) {
        self.master.write_all(b).unwrap();
        self.master.flush().unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }
    fn screen(&self, cols: usize, rows: usize) -> Vt {
        let mut vt = Vt::new(cols, rows);
        let out = self.out.lock().unwrap().clone();
        vt.feed(&String::from_utf8_lossy(&out));
        vt
    }
    fn settle(&self) {
        // wait until output stops changing
        let mut last = 0;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(30));
            let n = self.out.lock().unwrap().len();
            if n == last {
                return;
            }
            last = n;
        }
    }
    /// Answer the startup probe like kitty 0.43.
    fn answer_probe(&mut self) {
        assert!(self.wait_for(b"\x1b[c", Duration::from_secs(5)), "probe never sent");
        let reply = concat!(
            "\x1b_Gi=31;OK\x1b\\",
            "\x1b[?1u",
            "\x1bP>|kitty(0.43.1)\x1b\\",
            "\x1b[6;20;10t",
            "\x1b[1;1R",
            "\x1b[1;3R",
            "\x1b[1;5R",
            "\x1b]10;rgb:dada/dada/dada\x1b\\",
            "\x1b]11;rgb:1e1e/1e1e/1e1e\x1b\\",
            "\x1b]4;5;rgb:8a8a/5c5c/f5f5\x1b\\",
            "\x1b[?1016;2$y",
            "\x1b[?62;c",
        );
        self.send(reply.as_bytes());
    }
}

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("glyph-pty-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join(".obsidian")).unwrap();
    d
}

#[test]
fn real_binary_end_to_end() {
    let dir = tmpdir("e2e");
    std::fs::write(dir.join("note.md"), "# Heading\n\nsome **bold** text\n").unwrap();
    let mut p = spawn(&["--no-autosave", "--typography", "terminal", "note.md"], &dir, 80, 24);
    p.answer_probe();
    p.settle();
    let vt = p.screen(80, 24);
    let (x, y) = vt.find("Heading").unwrap_or_else(|| panic!("no heading:\n{}", vt.text()));
    assert_eq!(vt.cell(x, y).scale, 2, "OSC 66 heading after detecting text sizing");
    assert!(vt.text().contains("some bold text"), "{}", vt.text());
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    assert!(out.contains("\x1b[?1049h"), "alternate screen");
    assert!(out.contains("\x1b[>1u"), "kitty keyboard protocol requested");
    assert!(out.contains("\x1b[?1016h"), "pixel mouse enabled after DECRQM says it is supported");

    // type (legacy + kitty-encoded keys), save with ctrl+s, quit with ctrl+q
    p.send(b"\x1b[B\x1b[B");
    p.send(b"\x1b[F"); // end
    p.send(" and more".as_bytes());
    p.send(b"\x1b[115;5u"); // ctrl+s (kitty encoding)
    p.settle();
    assert_eq!(std::fs::read_to_string(dir.join("note.md")).unwrap(), "# Heading\n\nsome **bold** text and more\n");
    let vt = p.screen(80, 24);
    assert!(vt.text().contains("Saved"), "{}", vt.text());

    // resize
    set_size(p.master.try_clone().map(|f| std::os::fd::AsRawFd::as_raw_fd(&f)).unwrap(), 60, 20);
    unsafe {
        libc::kill(p.child.id() as i32, libc::SIGWINCH);
    }
    p.settle();
    p.send(b"\x11"); // ctrl+q (legacy)
    let t = Instant::now();
    let status = loop {
        if let Some(s) = p.child.try_wait().unwrap() {
            break s;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "glyph did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    p.settle();
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    assert!(out.ends_with("\x1b[?1049l") || out.contains("\x1b[?1049l"), "terminal restored");
    assert!(out.contains("\x1b[<u"), "keyboard mode popped");
}

#[test]
fn starts_without_probe_answers() {
    // a terminal that answers nothing: glyph must still come up (after the timeout)
    let dir = tmpdir("dumb");
    std::fs::write(dir.join("a.md"), "hello").unwrap();
    let mut p = spawn(&["a.md"], &dir, 80, 24);
    assert!(p.wait_for(b"hello", Duration::from_secs(5)), "never rendered");
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    // only the probe's two detection sequences, no rendered multicell text
    assert_eq!(out.matches("\x1b]66;").count(), 2, "no OSC 66 rendering without detection");
    p.send(b"\x11");
    let t = Instant::now();
    while p.child.try_wait().unwrap().is_none() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn help_and_version() {
    let o = Command::new(env!("CARGO_BIN_EXE_glyph")).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("glyph "));
    let o = Command::new(env!("CARGO_BIN_EXE_glyph")).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).contains("USAGE"));
    let o = Command::new(env!("CARGO_BIN_EXE_glyph")).arg("--bogus").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn real_binary_typesets_prose_with_kitty_graphics() {
    let dir = tmpdir("typeset");
    std::fs::write(dir.join("note.md"), "# Heading\n\nsome **bold** text\n").unwrap();
    let mut p = spawn(&["--no-autosave", "note.md"], &dir, 80, 24);
    p.answer_probe();
    p.settle();
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    assert!(out.contains("\x1b_Ga=T,U=1,f=100"), "prose transmitted as images");
    let vt = p.screen(80, 24);
    assert!(vt.placeholder_cells().len() > 20, "shown through Unicode placeholders");
    p.send(b"\x11");
    let t = Instant::now();
    while p.child.try_wait().unwrap().is_none() {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A directory with a stand-in `kitty` that prints its arguments, and a PATH using it.
fn fake_kitty(name: &str) -> (std::path::PathBuf, String) {
    let dir = tmpdir(name);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("kitty");
    std::fs::write(&fake, "#!/bin/sh\nprintf 'KITTY-ARG %s\\n' \"$@\"\nsleep 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (dir, path)
}

#[test]
fn kitty_flag_stays_in_a_graphics_terminal() {
    // Ghostty/kitty/WezTerm already speak kitty graphics: no new window
    let (dir, path) = fake_kitty("kittystay");
    std::fs::write(dir.join("note.md"), "hello there").unwrap();
    let mut p = spawn_env(&["--kitty", "note.md"], &dir, 80, 24, &[("PATH", &path), ("TERM", "xterm-ghostty")]);
    p.answer_probe();
    p.settle();
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    assert!(!out.contains("KITTY-ARG"), "must not launch kitty: {out}");
    assert!(p.child.try_wait().unwrap().is_none(), "glyph keeps running in place");
    assert!(out.contains("\x1b_G"), "draws with kitty graphics here");
    p.send(b"\x11");
}

#[test]
fn kitty_flag_launches_kitty_from_a_plain_terminal() {
    // a terminal that answers the probe without graphics support
    let (dir, path) = fake_kitty("kittylaunch");
    let mut p = spawn_env(&["--kitty", "note.md"], &dir, 80, 24, &[("PATH", &path), ("TERM", "xterm-256color")]);
    assert!(p.wait_for(b"\x1b[c", Duration::from_secs(5)), "probe never sent");
    p.send(b"\x1b[?62;c");
    assert!(p.wait_for(b"KITTY-ARG note.md", Duration::from_secs(5)), "kitty not launched");
    let out = String::from_utf8_lossy(&p.out.lock().unwrap()).to_string();
    assert!(out.contains("modify_font=cell_height 125%"), "{out}");
    assert!(out.contains("window_padding_width=20"));
    assert!(!out.contains("KITTY-ARG --kitty"), "re-runs glyph with the user's args minus --kitty");
    assert!(out.contains("\x1b[?1049l"), "restores the terminal before handing over");
}
