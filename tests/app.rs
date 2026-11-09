//! End-to-end tests: the real App rendering into a virtual terminal.

use glyph::app::Config;
use glyph::testing::{Harness, kitty_caps};
use glyph::input::{CTRL, SHIFT};
use std::time::Instant;

fn png(path: &std::path::Path, w: u32, h: u32) {
    image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 30, 255])).save(path).unwrap();
}

#[test]
fn type_and_save() {
    let mut h = Harness::new(80, 24, &[("note.md", "")]);
    h.type_text("# Hello\nworld");
    assert_eq!(h.text(), "# Hello\nworld");
    assert!(h.vt.text().contains("● unsaved"));
    h.keys("ctrl+s");
    assert_eq!(h.file("note.md"), "# Hello\nworld");
    assert!(h.vt.text().contains("✓ saved"));
    assert!(h.vt.title.contains("note"));
}

#[test]
fn live_preview_conceals_and_reveals() {
    let mut h = Harness::new(80, 24, &[("a.md", "plain **bold** text\nsecond line")]);
    // cursor starts on line 0 at col 0: "**" is not touched → hidden
    let s = h.frame();
    assert!(s.contains("plain bold text"), "{s}");
    // move into the bold span: markers appear
    h.keys("ctrl+right ctrl+right");
    assert!(h.vt.text().contains("plain **bold** text"), "{}", h.vt.text());
    // move to the next line: hidden again
    h.keys("down");
    assert!(h.vt.text().contains("plain bold text"));
    let (x, y) = h.vt.find("bold").unwrap();
    assert!(h.vt.cell(x, y).style.has(glyph::style::BOLD));
}

#[test]
fn headings_use_text_sizing() {
    let mut h = Harness::new(80, 24, &[("a.md", "text\n# Big Title\nafter")]);
    let s = h.frame();
    let (x, y) = h.vt.find("Big").unwrap();
    assert_eq!(h.vt.cell(x, y).scale, 2, "{s}");
    // the row below the scaled heading is covered
    assert_eq!(h.vt.cell(x, y + 1).flag, 2);
    // editing inside the heading keeps it scaled, shows "# " and uses a soft cursor
    h.keys("down end");
    let (x2, y2) = h.vt.find("#").unwrap();
    assert_eq!(h.vt.cell(x2, y2).scale, 2);
    h.type_text("!");
    assert_eq!(h.text(), "text\n# Big Title!\nafter");
}

#[test]
fn heading_images_without_text_sizing() {
    let mut caps = kitty_caps();
    caps.text_sizing = false;
    let mut h = Harness::with_caps(80, 24, &[("a.md", "# Image Heading\nbody")], caps, Config { autosave: false, ..Config::default() });
    h.keys("down");
    if h.app.images.has_font() {
        let ph = h.vt.placeholder_cells();
        assert!(!ph.is_empty(), "heading should render as an image");
        assert!(!h.vt.text().contains("Image Heading"));
        // moving onto the heading shows editable text (same height)
        h.keys("up");
        assert!(h.vt.text().contains("# Image Heading"));
        assert!(h.vt.find("body").unwrap().1 == 3);
    }
}

#[test]
fn smart_lists_and_tasks() {
    let mut h = Harness::new(80, 24, &[("a.md", "")]);
    // Enter on an empty item ends the list (Obsidian behaviour)
    h.type_text("- one\ntwo\n\nafter");
    assert_eq!(h.text(), "- one\n- two\nafter");
    h.keys("ctrl+a backspace");
    h.type_text("1. a\nb");
    assert_eq!(h.text(), "1. a\n2. b");
    h.keys("ctrl+a backspace");
    h.type_text("task");
    h.keys("ctrl+l");
    assert_eq!(h.text(), "- [ ] task");
    h.keys("enter");
    h.type_text("next");
    assert_eq!(h.text(), "- [ ] task\n- [ ] next");
    let s = h.keys("up");
    assert!(s.contains("☐ next"), "{s}");
    // tab indents a list item
    h.keys("down tab");
    assert_eq!(h.text(), "- [ ] task\n    - [ ] next");
    h.keys("shift+tab");
    assert_eq!(h.text(), "- [ ] task\n- [ ] next");
}

#[test]
fn undo_redo_keys() {
    let mut h = Harness::new(80, 24, &[("a.md", "")]);
    h.type_text("hello world");
    h.keys("ctrl+z");
    assert_eq!(h.text(), "");
    h.keys("ctrl+y");
    assert_eq!(h.text(), "hello world");
    h.keys("ctrl+b");
    assert_eq!(h.text(), "hello **world**");
    h.keys("ctrl+z");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn find_and_replace() {
    let mut h = Harness::new(80, 24, &[("a.md", "apple banana apple\ncherry apple")]);
    h.keys("ctrl+f");
    h.type_text("apple");
    let s = h.frame();
    assert!(s.contains("1 of 3"), "{s}");
    h.keys("enter");
    assert!(h.vt.text().contains("2 of 3"));
    h.keys("escape ctrl+h");
    h.keys("tab");
    h.type_text("pear");
    h.keys("alt+enter");
    assert_eq!(h.text(), "pear banana pear\ncherry pear");
    assert!(h.vt.text().contains("Replaced 3 occurrences"));
}

#[test]
fn quick_switcher_and_create() {
    let mut h = Harness::new(80, 24, &[("a.md", "first"), ("notes/Other Note.md", "second")]);
    h.keys("ctrl+o");
    h.type_text("othnot");
    let s = h.frame();
    assert!(s.contains("notes/Other Note"), "{s}");
    h.keys("enter");
    assert_eq!(h.text(), "second");
    h.keys("ctrl+o");
    h.type_text("Brand New");
    assert!(h.vt.text().contains("Create note: Brand New"));
    h.keys("enter");
    assert!(h.dir.join("notes/Brand New.md").exists());
}

#[test]
fn wikilink_autocomplete_and_follow() {
    let mut h = Harness::new(80, 24, &[("a.md", ""), ("Daily Log.md", "# Daily\n## Plans\nhere")]);
    h.type_text("see [[dai");
    let s = h.frame();
    assert!(s.contains("Daily Log"), "{s}");
    h.keys("enter");
    assert_eq!(h.text(), "see [[Daily Log]]");
    // follow the link, then go back
    h.keys("left left left alt+enter");
    assert_eq!(h.text(), "# Daily\n## Plans\nhere");
    h.keys("alt+left");
    assert_eq!(h.text(), "see [[Daily Log]]");
    // heading anchors
    h.keys("end");
    h.type_text(" [[Daily Log#Plans]]");
    h.keys("left left left alt+enter");
    assert_eq!(h.app.tabs[h.app.cur].ed.cursor.line, 1);
    // unresolved links create the note
    h.keys("alt+left end");
    h.type_text(" [[Fresh]]");
    h.keys("left left left alt+enter");
    assert!(h.dir.join("Fresh.md").exists());
}

#[test]
fn mouse_click_drag_and_checkbox() {
    let mut h = Harness::new(80, 24, &[("a.md", "alpha beta gamma\n- [ ] todo item\nlast")]);
    h.keys("down down");
    let (x, y) = h.vt.find("beta").unwrap();
    h.click(x + 1, y, 0);
    assert_eq!(h.app.tabs[0].ed.cursor, glyph::buffer::Pos::new(0, 7));
    // double click selects the word
    h.click(x + 1, y, 0);
    assert_eq!(h.app.tabs[0].ed.selected_text().as_deref(), Some("beta"));
    // drag selects (click elsewhere first so this isn't a triple click)
    let (lx, ly) = h.vt.find("last").unwrap();
    h.click(lx, ly, 0);
    let (gx, gy) = h.vt.find("gamma").unwrap();
    h.drag((x, y), (gx + 5, gy));
    assert_eq!(h.app.tabs[0].ed.selected_text().as_deref(), Some("beta gamma"));
    // click the checkbox toggles the task without moving the cursor away
    let (cx, cy) = h.vt.find("☐").unwrap();
    h.click(cx, cy, 0);
    assert_eq!(h.text(), "alpha beta gamma\n- [x] todo item\nlast");
    assert!(h.vt.text().contains("☑ todo item"));
}

#[test]
fn ctrl_click_follows_links() {
    let mut h = Harness::new(80, 24, &[("a.md", "go [[b]] now"), ("b.md", "target note")]);
    h.keys("end");
    let (x, y) = h.vt.find("b now").unwrap();
    h.click(x, y, CTRL);
    assert_eq!(h.text(), "target note");
}

#[test]
fn images_via_unicode_placeholders() {
    let mut h = Harness::new(80, 30, &[("a.md", "text\n\n![pic](img/red.png)\n\nafter")]);
    std::fs::create_dir_all(h.dir.join("img")).unwrap();
    png(&h.dir.join("img/red.png"), 100, 60);
    h.app.layout.clear();
    let _ = h.frame();
    let ph = h.vt.placeholder_cells();
    assert!(!ph.is_empty(), "{}", h.vt.text());
    let id = ph[0].0;
    let img = h.vt.images.get(&id).expect("image transmitted");
    assert_eq!((img.cols, img.rows), (10, 3)); // 100x60 px at 10x20 cells
    assert_eq!(ph.len(), 30);
    // the source line is hidden while the cursor is elsewhere
    assert!(!h.vt.text().contains("![pic]"));
    // moving onto the line reveals the source above the image
    h.keys("down down");
    assert!(h.vt.text().contains("![pic](img/red.png)"));
    assert!(!h.vt.placeholder_cells().is_empty());
}

#[test]
fn images_via_classic_placements_crop_when_scrolled() {
    let mut caps = kitty_caps();
    caps.placeholders = false;
    let mut body = String::from("![big](big.png)\n");
    for i in 0..40 {
        body.push_str(&format!("line {i}\n"));
    }
    let mut h = Harness::with_caps(80, 20, &[("a.md", &body)], caps, Config { autosave: false, ..Config::default() });
    png(&h.dir.join("big.png"), 200, 200);
    h.app.layout.clear();
    h.keys("down down");
    assert_eq!(h.vt.placements.len(), 1);
    let p = h.vt.placements[0].clone();
    assert_eq!(p.src, (0, 0, 200, 200));
    // scroll by 3 rows: the placement is re-issued cropped
    h.wheel(10, 10, true);
    assert_eq!(h.vt.placements.len(), 1);
    let p = &h.vt.placements[0];
    assert_eq!(p.y, 1);
    assert_eq!(p.src.1, 3 * 20);
    assert_eq!(p.src.3, 7 * 20);
}

#[test]
fn missing_image_shows_message() {
    let mut h = Harness::new(80, 24, &[("a.md", "x\n![[nope.png]]")]);
    let s = h.frame();
    assert!(s.contains("nope.png — not found"), "{s}");
}

#[test]
fn tables_render_and_tab_formats() {
    let mut h = Harness::new(80, 24, &[("a.md", "intro\n\n|a|b|\n|-|-|\n|1|2|")]);
    let s = h.frame();
    assert!(s.contains("┌───┬───┐"), "{s}");
    assert!(s.contains("│ a │ b │"));
    h.keys("down down down tab");
    assert_eq!(h.text(), "intro\n\n| a   | b   |\n| --- | --- |\n| 1   | 2   |");
}

#[test]
fn reading_and_source_modes() {
    let mut h = Harness::new(80, 24, &[("a.md", "# Title\n**bold** and %%secret%%")]);
    h.keys("ctrl+e");
    let s = h.frame();
    assert!(s.contains("bold and"), "{s}");
    assert!(!s.contains("secret"));
    assert!(s.contains("Reading"));
    assert!(!h.vt.cursor_visible);
    h.type_text("x");
    assert_eq!(h.text(), "# Title\n**bold** and %%secret%%");
    h.keys("ctrl+e alt+e");
    let s = h.frame();
    assert!(s.contains("# Title"));
    assert!(s.contains("**bold** and %%secret%%"));
}

#[test]
fn sidebars_and_file_tree() {
    let mut h = Harness::new(100, 24, &[("a.md", "A"), ("dir/b.md", "B text")]);
    h.keys("ctrl+\\");
    let s = h.frame();
    assert!(s.contains("▸ dir"), "{s}");
    let (x, y) = h.vt.find("▸ dir").unwrap();
    h.click(x, y, 0);
    let (x, y) = h.vt.find("  b").unwrap_or_else(|| panic!("{}", h.vt.text()));
    h.click(x + 2, y, 0);
    assert_eq!(h.text(), "B text");
    // outline panel
    h.keys("ctrl+o");
    h.type_text("a.md");
    h.keys("enter");
    h.type_text("\n# Head One\n## Head Two");
    h.keys("ctrl+shift+\\");
    let s = h.frame();
    assert!(s.contains("Outline"));
    assert!(s.contains("Head Two"));
}

#[test]
fn command_palette_runs_commands() {
    let mut h = Harness::new(80, 24, &[("a.md", "word")]);
    h.keys("ctrl+p");
    h.type_text("toggle bold");
    let s = h.frame();
    assert!(s.contains("Toggle bold"), "{s}");
    h.keys("enter");
    assert_eq!(h.text(), "**word**");
    // empty palette lists commands in table order
    h.keys("ctrl+p");
    assert!(h.vt.text().contains("Save file"));
    h.keys("escape");
    assert!(!h.vt.text().contains("Save file"));
}

#[test]
fn code_blocks_are_highlighted() {
    let mut h = Harness::new(80, 24, &[("a.md", "x\n\n```rust\nfn main() { let v = 42; }\n```")]);
    let s = h.frame();
    assert!(s.contains("rust"));
    assert!(!s.contains("```"));
    let (x, y) = h.vt.find("fn main").unwrap();
    let fn_style = h.vt.cell(x, y).style;
    let (nx, ny) = h.vt.find("42").unwrap();
    assert_ne!(fn_style.fg, h.vt.cell(nx, ny).style.fg, "keywords and numbers differ");
    assert!(h.vt.cell(x, y).style.bg.is_set(), "code panel background");
}

#[test]
fn wide_characters_and_cursor() {
    let mut h = Harness::new(60, 10, &[("a.md", "日本語テキスト")]);
    h.keys("end");
    h.type_text("!");
    assert_eq!(h.text(), "日本語テキスト!");
    let row = h.vt.row(1);
    assert!(row.contains("日本語テキスト!"), "{row}");
    // cursor sits after the 7 wide chars + "!"
    let cx0 = h.app.tabs[0].view.cx0;
    assert_eq!(h.vt.cx, cx0 + 15);
    h.keys("left left");
    assert_eq!(h.vt.cx, cx0 + 12);
}

#[test]
fn soft_wrap_and_vertical_motion() {
    let long = "word ".repeat(40);
    let mut h = Harness::new(40, 20, &[("a.md", &format!("{}\nnext", long.trim_end()))]);
    let s = h.frame();
    let rows_with_word = s.lines().filter(|l| l.contains("word")).count();
    assert!(rows_with_word >= 6, "{s}");
    h.keys("down");
    let c = h.app.tabs[0].ed.cursor;
    assert_eq!(c.line, 0, "down moves within the wrapped line");
    assert!(c.col > 0);
}

#[test]
fn rename_updates_links() {
    let mut h = Harness::new(80, 24, &[("Old.md", "content"), ("ref.md", "see [[Old]] and [[Old|alias]]")]);
    h.keys("f2 ctrl+u");
    h.type_text("New Name");
    h.keys("enter");
    assert!(h.dir.join("New Name.md").exists());
    assert!(!h.dir.join("Old.md").exists());
    assert_eq!(h.file("ref.md"), "see [[New Name]] and [[New Name|alias]]");
}

#[test]
fn copy_uses_osc52() {
    let mut h = Harness::new(80, 24, &[("a.md", "copy me\nsecond")]);
    h.keys("ctrl+c");
    assert_eq!(h.vt.clipboard.as_deref(), Some("copy me\n"));
    h.keys("down end");
    h.app.system_paste = Box::new(|| None);
    h.keys("ctrl+v");
    assert_eq!(h.text(), "copy me\nsecondcopy me\n");
}

#[test]
fn paste_event_inserts_text() {
    let mut h = Harness::new(80, 24, &[("a.md", "")]);
    h.paste("line one\nline two");
    assert_eq!(h.text(), "line one\nline two");
    h.keys("ctrl+z");
    assert_eq!(h.text(), "");
}

#[test]
fn external_change_reloads_clean_buffer() {
    let mut h = Harness::new(80, 24, &[("a.md", "before")]);
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(h.dir.join("a.md"), "after edit").unwrap();
    h.app.handle(glyph::input::Event::Reply(glyph::input::Reply::Focus(true)));
    h.frame();
    assert_eq!(h.text(), "after edit");
}

#[test]
fn autosave_after_idle() {
    let mut h = Harness::with_caps(80, 24, &[("a.md", "")], kitty_caps(), Config::default());
    h.type_text("auto");
    assert_eq!(h.file("a.md"), "");
    std::thread::sleep(std::time::Duration::from_millis(1300));
    h.app.tick();
    assert_eq!(h.file("a.md"), "auto");
}

#[test]
fn folded_callout_expands_on_entry() {
    let mut h = Harness::new(80, 24, &[("a.md", "top\n> [!note]- Hidden stuff\n> secret body\nend")]);
    let s = h.frame();
    assert!(s.contains("Hidden stuff"));
    assert!(!s.contains("secret body"), "{s}");
    h.keys("down down");
    assert!(h.vt.text().contains("secret body"));
}

#[test]
fn no_graphics_terminal_emits_no_apc() {
    let mut caps = kitty_caps();
    caps.graphics = false;
    caps.placeholders = false;
    caps.text_sizing = false;
    let mut h = Harness::with_caps(80, 24, &[("a.md", "# Title\n![x](x.png)")], caps, Config { autosave: false, ..Config::default() });
    png(&h.dir.join("x.png"), 10, 10);
    let out = h.app.render();
    assert!(!out.contains("\x1b_G"));
    assert!(!out.contains("\x1b]66"));
    let s = h.frame();
    assert!(s.contains("Title"));
    assert!(s.contains("━━━"), "plain H1 gets a rule: {s}");
}

#[test]
fn resize_rerenders() {
    let mut h = Harness::new(80, 24, &[("a.md", "some text here")]);
    h.app.w = 50;
    h.app.h = 12;
    h.vt = glyph::vt::Vt::new(50, 12);
    let s = h.frame();
    assert!(s.contains("some text here"));
    assert_eq!(s.lines().count(), 12);
}

#[test]
fn idle_frames_are_tiny() {
    let mut h = Harness::new(100, 40, &[("a.md", &"# T\nsome **text** here\n".repeat(30))]);
    h.keys("down end");
    h.frame();
    h.frame();
    assert!(h.out_bytes < 80, "idle frame should only reposition the cursor, got {} bytes", h.out_bytes);
    h.type_text("x");
    assert!(h.out_bytes < 2000, "typing a char should redraw ~one row, got {} bytes", h.out_bytes);
}

#[test]
fn large_document_is_fast() {
    let mut doc = String::new();
    for i in 0..20_000 {
        doc.push_str(&format!("## Section {i}\nSome **bold** text with a [[link]] and `code` #tag {i}\n- item {i}\n\n"));
    }
    let mut h = Harness::new(120, 50, &[("big.md", &doc)]);
    let t = Instant::now();
    h.frame();
    let first = t.elapsed();
    let t = Instant::now();
    for _ in 0..20 {
        h.type_text("x");
    }
    let per_key = t.elapsed() / 20;
    let t = Instant::now();
    h.keys("ctrl+end");
    let jump = t.elapsed();
    let t = Instant::now();
    for _ in 0..10 {
        h.keys("pageup");
    }
    let page = t.elapsed() / 10;
    eprintln!("first frame {first:?}, keystroke {per_key:?}, jump to end {jump:?}, page {page:?}");
    // generous bounds for debug builds on shared CI machines
    assert!(per_key.as_millis() < 250, "keystroke took {per_key:?}");
    assert!(jump.as_millis() < 2000, "jump took {jump:?}");
}

#[test]
fn shift_selection_and_typing_replaces() {
    let mut h = Harness::new(80, 24, &[("a.md", "hello world")]);
    h.keys("shift+end");
    h.type_text("bye");
    assert_eq!(h.text(), "bye");
    let _ = SHIFT;
}

#[test]
fn quit_with_unsaved_changes_asks() {
    let mut h = Harness::new(80, 24, &[("a.md", "")]);
    h.type_text("dirty");
    h.keys("ctrl+q");
    assert!(h.vt.text().contains("Save before quitting"));
    h.keys("y");
    assert!(h.app.quit);
    assert_eq!(h.file("a.md"), "dirty");
}

#[test]
fn vault_search() {
    let mut h = Harness::new(90, 24, &[("a.md", "alpha"), ("b.md", "line\nfind the needle here")]);
    h.keys("ctrl+shift+f");
    h.type_text("needle");
    let s = h.frame();
    assert!(s.contains("find the needle here"), "{s}");
    h.keys("enter");
    assert_eq!(h.app.tabs[h.app.cur].ed.selected_text().as_deref(), Some("needle"));
}

#[test]
fn backlinks_panel() {
    let mut h = Harness::new(110, 24, &[("target.md", "# Target"), ("src.md", "links to [[target]] here")]);
    h.keys("ctrl+shift+\\");
    let (x, y) = h.vt.find("Links").unwrap();
    h.click(x, y, 0);
    let s = h.frame();
    assert!(s.contains("src · links to target here"), "{s}");
}
