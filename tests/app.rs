//! End-to-end tests: the real App rendering into a virtual terminal.

use glyph::app::Config;
use glyph::input::{CTRL, SHIFT};
use glyph::testing::{Harness, kitty_caps};
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
    // the hardware cursor sits on a multicell block (kitty draws it full height),
    // including past the end of the text
    let (ex, ey) = h.vt.find("Title!").unwrap();
    assert!(h.vt.cursor_visible);
    assert_eq!((h.vt.cx, h.vt.cy), (ex + 12, ey));
    assert_eq!(h.vt.block_at(h.vt.cx, h.vt.cy).map(|b| b.2), Some(2));
}

#[test]
fn heading_images_without_text_sizing() {
    let mut caps = kitty_caps();
    caps.text_sizing = false;
    let mut h = Harness::with_caps(
        80,
        24,
        &[("a.md", "# Image Heading\nbody")],
        caps,
        Config { autosave: false, inline_title: false, typography: "terminal".into(), ..Config::default() },
    );
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
    assert!(s.contains("▒▒ next"), "checkbox drawn as an inline image: {s}");
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
    assert!(s.contains("Other Note  notes"), "name with dim folder: {s}");
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
    let (cx, cy) = h.vt.find("▒▒ todo").unwrap();
    let before = h.vt.placeholder_cells().iter().find(|p| p.4 == cy).map(|p| p.0);
    h.click(cx, cy, 0);
    assert_eq!(h.text(), "alpha beta gamma\n- [x] todo item\nlast");
    let after = h.vt.placeholder_cells().iter().find(|p| p.4 == cy).map(|p| p.0);
    assert!(before.is_some() && after.is_some() && before != after, "checked box is a different icon");
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
    let mut h = Harness::with_caps(
        80,
        20,
        &[("a.md", &body)],
        caps,
        Config { autosave: false, inline_title: false, typography: "terminal".into(), ..Config::default() },
    );
    png(&h.dir.join("big.png"), 200, 200);
    h.app.layout.clear();
    h.keys("down down");
    assert_eq!(h.vt.placements.len(), 1);
    let p = h.vt.placements[0].clone();
    assert_eq!(p.src, (0, 0, 200, 200));
    // scroll by 3 rows: the placement is re-issued cropped
    for _ in 0..3 {
        h.wheel(10, 10, true);
    }
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
    let mut h = Harness::with_caps(
        80,
        24,
        &[("a.md", "# Title\n![x](x.png)")],
        caps,
        Config { autosave: false, inline_title: false, typography: "terminal".into(), ..Config::default() },
    );
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

#[test]
fn paste_url_over_selection_makes_link() {
    let mut h = Harness::new(80, 24, &[("a.md", "see docs here")]);
    h.keys("ctrl+right right alt+w");
    h.paste("https://example.com/docs");
    assert_eq!(h.text(), "see [docs](https://example.com/docs) here");
}

#[test]
fn dropped_image_path_becomes_embed() {
    let mut h = Harness::new(80, 24, &[("notes/a.md", "")]);
    std::fs::create_dir_all(h.dir.join("assets")).unwrap();
    let img = h.dir.join("assets/My Pic.png");
    png(&img, 20, 20);
    h.paste(&format!("file://{}", img.display().to_string().replace(' ', "%20")));
    assert_eq!(h.text(), "![My Pic](../assets/My%20Pic.png)");
    // and it renders
    h.keys("enter");
    assert!(!h.vt.placeholder_cells().is_empty(), "{}", h.vt.text());
}

#[test]
fn note_embeds_render_inline() {
    let mut h = Harness::new(
        80,
        30,
        &[
            ("a.md", "top\n![[Other#Part two]]\nbottom"),
            ("Other.md", "# Other\nintro\n## Part two\n- [ ] embedded task\nmore **text**\n## Part three\nnot shown"),
        ],
    );
    let s = h.frame();
    assert!(s.contains("↳ Other › Part two"), "{s}");
    assert!(!s.contains("![["), "embed source hidden");
    assert!(s.contains("☐ embedded task"));
    assert!(s.contains("more text"));
    assert!(!s.contains("not shown"));
}

#[test]
fn back_from_untitled_tab_does_not_panic() {
    // regression (found by tests/chaos.rs): Back when the current tab has no file
    let mut h = Harness::new(80, 24, &[("a.md", "[[b]]"), ("b.md", "B")]);
    h.keys("right right alt+enter");
    assert_eq!(h.text(), "B");
    // an untitled buffer (no file) as the current document
    h.app.open_text("scratch");
    h.keys("alt+left alt+left alt+left alt+right alt+right");
    assert!(!h.app.tabs.is_empty(), "still alive");
}

#[test]
fn heading_size_ramp_uses_fractional_osc66() {
    let mut h = Harness::new(100, 30, &[("a.md", "x\n# One\n## Two words here\n### Three\n#### Four")]);
    h.app.screen.invalidate();
    let out = h.app.render();
    let _ = h.frame();
    assert!(out.contains("\x1b]66;s=2;One\x07"), "H1 at 2x: {out:?}");
    assert!(out.contains(":w=4:n=4:d=5:v=1;Two w"), "H2 at 1.6x in 5-column chunks: {out:?}");
    assert!(out.contains(":w=2:n=2:d=3:v=1;Thr"), "H3 at 1.33x in 3-column chunks");
    let (x, y) = h.vt.find("Two words").unwrap();
    let c = h.vt.cell(x, y);
    assert_eq!((c.scale, c.units, c.n, c.d), (2, 4, 4, 5));
    // H4 stays at normal size
    let (x, y) = h.vt.find("Four").unwrap();
    assert_eq!(h.vt.cell(x, y).scale, 1);
    // editing an H2 splits the chunk at the cursor so it lands on a block start
    h.keys("down down ctrl+right right");
    let cur = h.app.tabs[0].ed.cursor;
    assert_eq!(cur.col, 7, "cursor before 'w' of 'words'");
    let (bx, by, s) = h.vt.block_at(h.vt.cx, h.vt.cy).expect("cursor on a block");
    assert_eq!((bx, by, s), (h.vt.cx, h.vt.cy, 2));
    assert!(h.vt.cell(bx, by).text.starts_with('w'), "{:?}", h.vt.cell(bx, by).text);
}

#[test]
fn inline_title_and_pristine_open() {
    let cfg = Config { autosave: false, inline_title: true, typography: "terminal".into(), ..Config::default() };
    let mut h = Harness::with_caps(80, 24, &[("My Note.md", "# Heading\nbody **bold**")], kitty_caps(), cfg);
    let s = h.frame();
    // the note name is shown large above the text, and the freshly opened
    // note renders clean (no "# " revealed although the cursor is on line 0)
    let y = (0..24).find(|&y| h.vt.row(y).contains("My Note")).unwrap_or_else(|| panic!("{s}"));
    let x = (0..80).find(|&x| h.vt.cell(x, y).text.starts_with('M')).unwrap();
    assert_eq!(h.vt.cell(x, y).scale, 2);
    assert!(!s.contains("# Heading"), "{s}");
    assert!(s.contains("Heading"));
    // first interaction reveals syntax under the cursor as usual
    h.keys("end");
    assert!(h.vt.text().contains("# Heading"));
    // clicking the title puts the cursor on the first line; titles are not editable
    h.click(x, y, 0);
    assert_eq!(h.app.tabs[0].ed.cursor.line, 0);
    assert_eq!(h.text(), "# Heading\nbody **bold**");
}

#[test]
fn status_bar_counts_like_obsidian() {
    let mut h = Harness::new(100, 24, &[("a.md", "one two three"), ("b.md", "see [[a]]"), ("c.md", "[[a]] again")]);
    let s = h.frame();
    let last = s.lines().last().unwrap().to_string();
    assert!(last.contains("2 backlinks"), "{last}");
    assert!(last.contains("3 words"));
    assert!(last.contains("13 characters"));
}

#[test]
fn palette_has_placeholder_and_footer() {
    let mut h = Harness::new(100, 30, &[("a.md", "x")]);
    h.keys("ctrl+p");
    let s = h.frame();
    assert!(s.contains("Select a command..."), "{s}");
    assert!(s.contains("to navigate"));
    assert!(s.contains("Ctrl+S"), "keycap hints");
}

#[test]
fn single_document_no_tab_bar() {
    let mut h = Harness::new(80, 24, &[("a.md", "first note"), ("b.md", "second note")]);
    let s = h.frame();
    assert!(s.lines().nth(1).unwrap().contains("first note"), "no tab bar; one row of top padding: {s}");
    h.keys("ctrl+o");
    h.type_text("b.md");
    h.keys("enter");
    assert_eq!(h.app.tabs.len(), 1, "opening replaces the document");
    assert_eq!(h.text(), "second note");
    h.keys("alt+left");
    assert_eq!(h.text(), "first note");
    // ctrl+w deletes the previous word (no tab to close)
    h.keys("ctrl+end ctrl+w");
    assert_eq!(h.text(), "first ");
}

#[test]
fn save_untitled_asks_for_a_name() {
    let mut h = Harness::new(80, 24, &[("a.md", "x")]);
    h.app.open_text("");
    h.type_text("fresh text");
    h.keys("ctrl+s");
    assert!(h.vt.text().contains("Save as note"));
    h.type_text("Fresh");
    h.keys("enter");
    assert_eq!(h.file("Fresh.md"), "fresh text");
}

fn typeset_harness(w: usize, h: usize, files: &[(&str, &str)]) -> Harness {
    let cfg = Config { autosave: false, inline_title: false, typography: "proportional".into(), ..Config::default() };
    Harness::with_caps(w, h, files, kitty_caps(), cfg)
}

#[test]
fn typeset_prose_is_drawn_as_images_and_editable() {
    let mut h = typeset_harness(80, 20, &[("a.md", "Hello **world**, this is prose.\nsecond line")]);
    let s = h.frame();
    assert!(!s.contains("Hello"), "prose is not cell text: {s}");
    assert!(h.vt.placeholder_cells().len() > 30);
    // the caret is drawn in the image; the hardware cursor stays hidden
    h.keys("end");
    assert!(!h.vt.cursor_visible);
    h.type_text("!");
    assert_eq!(h.text(), "Hello **world**, this is prose.!\nsecond line");
    // clicking at the far left of the first image row puts the cursor at col 0
    let (x0, y0) = {
        let p = h.vt.placeholder_cells();
        let min_y = p.iter().map(|c| c.4).min().unwrap();
        (p.iter().filter(|c| c.4 == min_y).map(|c| c.3).min().unwrap(), min_y)
    };
    // pixel-precise click (SGR-Pixels) on the left edge of "H" → col 0; on
    // the right part of "H" → col 1
    h.click_px(x0 as u32 * 10 + 2, y0 as u32 * 20 + 10);
    assert_eq!(h.app.tabs[0].ed.cursor, glyph::buffer::Pos::new(0, 0));
    h.keys("down"); // (so the next click isn't a double-click)
    h.click_px(x0 as u32 * 10 + 30, y0 as u32 * 20 + 10);
    h.click_px(x0 as u32 * 10 + 8, y0 as u32 * 20 + 10);
    assert_eq!(h.app.tabs[0].ed.cursor, glyph::buffer::Pos::new(0, 1));
    // a click further right lands somewhere inside the line, on a char boundary
    h.click(x0 + 12, y0, 0);
    let c = h.app.tabs[0].ed.cursor;
    assert_eq!(c.line, 0);
    assert!(c.col > 3 && c.col < 32, "{c:?}");
}

#[test]
fn typeset_vertical_motion_walks_wrapped_lines() {
    let para = "word ".repeat(60);
    let mut h = typeset_harness(60, 30, &[("a.md", &format!("{}\nnext", para.trim_end()))]);
    h.keys("ctrl+home");
    h.keys("down");
    let c = h.app.tabs[0].ed.cursor;
    assert_eq!(c.line, 0, "down moves to the next visual line of the same paragraph");
    assert!(c.col > 20, "{c:?}");
    for _ in 0..10 {
        h.keys("down");
    }
    assert_eq!(h.app.tabs[0].ed.cursor.line, 1);
    h.keys("up");
    assert_eq!(h.app.tabs[0].ed.cursor.line, 0);
}

#[test]
fn typeset_falls_back_for_uncovered_scripts() {
    let mut h = typeset_harness(80, 20, &[("a.md", "日本語のテキスト\nLatin prose here")]);
    let s = h.frame();
    assert!(s.contains("日本語のテキスト"), "CJK stays terminal text: {s}");
    assert!(!s.contains("Latin prose"), "Latin prose is typeset");
}

#[test]
fn typeset_selection_and_search_render_without_cell_text() {
    let mut h = typeset_harness(80, 20, &[("a.md", "find the needle here\nanother needle")]);
    h.keys("ctrl+f");
    h.type_text("needle");
    h.frame();
    // (the count is drawn as proportional text; check the state instead)
    assert_eq!(h.app.find_hits().len(), 2);
    assert!(h.vt.placeholder_cells().len() > 20);
    h.keys("escape ctrl+a");
    assert_eq!(h.app.tabs[0].ed.selected_text().as_deref(), Some("find the needle here\nanother needle"));
}

// ---------------------------------------------------------------- grid tables

const TABLE: &str = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |";

#[test]
fn tables_stay_a_grid_while_editing() {
    let mut h = Harness::new(80, 24, &[("a.md", TABLE)]);
    h.keys("down down");
    let s = h.frame();
    assert!(s.contains("│ a │ b │"), "{s}");
    assert!(!s.contains("| a | b |"), "{s}");
    // the caret is in the first cell, so typing goes into it
    h.type_text("x");
    assert_eq!(h.text(), "intro\n\n| xa | b |\n|---|---|\n| 1 | 2 |");
    let s = h.frame();
    assert!(s.contains("│ xa │ b │"), "{s}");
    // the hardware cursor sits right after the typed character
    let (x, y) = h.vt.find("xa").unwrap();
    assert_eq!((h.vt.cx, h.vt.cy), (x + 1, y));
}

#[test]
fn table_arrows_move_between_cells_and_skip_the_alignment_row() {
    let mut h = Harness::new(80, 24, &[("a.md", TABLE)]);
    h.keys("down down end");
    // Right from the end of a cell enters the next one
    h.keys("left right right");
    h.type_text("R");
    assert_eq!(h.text(), "intro\n\n| a | Rb |\n|---|---|\n| 1 | 2 |");
    // past the last cell: next row's first cell, over the alignment row
    h.keys("end right");
    h.type_text("D");
    assert_eq!(h.text(), "intro\n\n| a | Rb |\n|---|---|\n| D1 | 2 |");
    // Left from a row's first cell goes to the previous row's last cell
    h.keys("home left");
    h.type_text("L");
    assert_eq!(h.text(), "intro\n\n| a | RbL |\n|---|---|\n| D1 | 2 |");
    // Down from the header lands in the same column of the body
    h.keys("down");
    h.type_text("V");
    assert!(h.text().ends_with("| D1 | V2 |") || h.text().ends_with("| D1 | 2V |"), "{}", h.text());
}

#[test]
fn table_enter_moves_down_and_adds_rows() {
    let mut h = Harness::new(80, 24, &[("a.md", TABLE)]);
    // End stays in the cell; Enter goes to the same column one row down
    h.keys("down down right right end enter");
    h.type_text("y");
    assert_eq!(h.text(), "intro\n\n| a | b |\n|---|---|\n| 1 | 2y |");
    // Enter on the last row appends a row and stays in the column
    h.keys("enter");
    h.type_text("z");
    assert_eq!(h.text(), "intro\n\n| a | b |\n|---|---|\n| 1 | 2y |\n| | z|");
    let s = h.frame();
    assert!(s.contains("│ z  │"), "{s}");
}

#[test]
fn table_backspace_and_delete_keep_cell_boundaries() {
    let mut h = Harness::new(80, 24, &[("a.md", TABLE)]);
    h.keys("down down end home");
    // at the start of a cell: nothing to delete
    h.keys("backspace");
    assert_eq!(h.text(), TABLE);
    h.keys("right right end delete");
    assert_eq!(h.text(), TABLE);
    // a literal pipe is escaped
    h.type_text("|");
    assert_eq!(h.text(), "intro\n\n| a | b\\| |\n|---|---|\n| 1 | 2 |");
    // shown as a plain pipe once the caret leaves it (revealed while touching)
    assert!(h.frame().contains("│ a │ b\\| │"));
    h.keys("down");
    let s = h.frame();
    assert!(s.contains("│ a │ b| │"), "{s}");
    // Backspace at the start of an empty row removes the row
    h.keys("end enter");
    assert_eq!(h.text().lines().count(), 6);
    h.keys("backspace");
    assert_eq!(h.text(), "intro\n\n| a | b\\| |\n|---|---|\n| 1 | 2 |");
    h.type_text("!");
    assert!(h.text().ends_with("| 1 | 2! |"), "{}", h.text());
}

#[test]
fn table_click_places_caret_in_the_cell() {
    let mut h = Harness::new(80, 24, &[("a.md", "intro\n\n| aaa | bbb |\n|---|---|\n| 111 | 222 |")]);
    h.frame();
    let (x, y) = h.vt.find("222").unwrap();
    h.click(x + 1, y, 0);
    h.type_text("_");
    assert!(h.text().ends_with("| 111 | 2_22 |"), "{}", h.text());
    // clicking the padding right of a cell lands at its end
    let (x, y) = h.vt.find("111").unwrap();
    h.click(x + 4, y, 0);
    h.type_text("+");
    assert!(h.text().ends_with("| 111+ | 2_22 |"), "{}", h.text());
}

#[test]
fn table_wrapped_cells_are_editable_on_every_row() {
    let long = "alpha beta gamma delta epsilon zeta eta theta";
    let mut h = Harness::new(40, 24, &[("a.md", &format!("intro\n\n| k | v |\n|---|---|\n| x | {long} |\nafter"))]);
    let s = h.frame();
    let (x, y) = h.vt.find("theta").unwrap_or_else(|| panic!("{s}"));
    let (_, y0) = h.vt.find("alpha").unwrap();
    assert!(y > y0, "cell should wrap: {s}");
    // click the last word on its wrapped row and type there
    h.click(x + 5, y, 0);
    h.type_text("!");
    assert!(h.text().contains("theta! |"), "{}", h.text());
    let (tx, ty) = h.vt.find("theta!").unwrap();
    assert_eq!((h.vt.cx, h.vt.cy), (tx + 6, ty));
    // Up from the short cell's row skips the other cell's wrapped rows
    h.keys("ctrl+home down down down");
    h.type_text("Q");
    assert!(h.text().contains("| Qx |"), "{}", h.text());
    // Down from the short cell skips the long cell's wrapped rows
    h.keys("down");
    h.type_text("A");
    assert!(h.text().lines().last().unwrap().contains('A'), "{}", h.text());
    assert!(h.text().contains("| Qx | alpha"), "{}", h.text());
}

#[test]
fn source_mode_edits_tables_as_text() {
    let mut h = Harness::new(80, 24, &[("a.md", TABLE)]);
    h.keys("alt+e down down home");
    let s = h.frame();
    assert!(s.contains("| a | b |"), "{s}");
    h.type_text("|");
    assert_eq!(h.text(), "intro\n\n|| a | b |\n|---|---|\n| 1 | 2 |");
}

#[test]
fn page_keys_move_half_a_screen_and_wheel_one_row() {
    let text: String = (0..200).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n");
    let mut h = Harness::new(80, 24, &[("a.md", &text)]);
    h.frame();
    let y_of = |h: &Harness, s: &str| (0..24).find(|&y| h.vt.row(y).split_whitespace().any(|w| w == s));
    h.keys("pagedown pagedown");
    let y0 = y_of(&h, "L40").unwrap();
    h.wheel(10, 10, true);
    assert_eq!(y_of(&h, "L40").unwrap(), y0 - 1, "one row per wheel event");
    let y1 = y_of(&h, "L40").unwrap();
    h.keys("pagedown");
    let y2 = y_of(&h, "L40").unwrap();
    let moved = y1 - y2;
    assert!((9..=12).contains(&moved), "PageDown should scroll about half of a 24-row screen, moved {moved}");
    h.keys("pageup");
    let back = y_of(&h, "L40").unwrap() - y2;
    assert!((9..=12).contains(&back), "PageUp should scroll about half a screen, moved {back}");
}

#[test]
fn table_caret_never_rests_on_pipes() {
    // opened with the caret at (0, 0), on the leading pipe
    let mut h = Harness::new(80, 24, &[("a.md", "| a | b |\n|---|---|\n| 1 | 2 |")]);
    h.type_text("x");
    assert_eq!(h.text(), "| xa | b |\n|---|---|\n| 1 | 2 |");
    // Down past the end of the document stays in the last cell
    h.keys("down down down down");
    h.type_text("y");
    assert_eq!(h.text(), "| xa | b |\n|---|---|\n| 1 | 2y |");
    h.keys("ctrl+home");
    h.type_text("z");
    assert_eq!(h.text(), "| zxa | b |\n|---|---|\n| 1 | 2y |");
    h.keys("ctrl+end");
    h.type_text("w");
    assert_eq!(h.text(), "| zxa | b |\n|---|---|\n| 1 | 2yw |");
}

#[test]
fn table_without_outer_pipes_skips_the_alignment_row() {
    let mut h = Harness::new(80, 24, &[("a.md", "a | b\n--|--\n1 | 2")]);
    h.keys("right right right right");
    h.type_text("x");
    assert_eq!(h.text(), "a | b\n--|--\nx1 | 2");
    h.keys("left left");
    h.type_text("y");
    assert_eq!(h.text(), "a | by\n--|--\nx1 | 2");
}

#[test]
fn table_pipe_after_a_backslash_completes_the_escape() {
    let mut h = Harness::new(80, 24, &[("a.md", "| a | b |\n|---|---|\n| 1 | 2 |")]);
    h.keys("right");
    h.type_text("\\|");
    assert_eq!(h.text(), "| a\\| | b |\n|---|---|\n| 1 | 2 |");
    h.type_text("|");
    assert_eq!(h.text(), "| a\\|\\| | b |\n|---|---|\n| 1 | 2 |");
}

#[test]
fn table_cells_beyond_the_header_are_not_reachable() {
    let mut h = Harness::new(80, 24, &[("a.md", "| a | b |\n|---|---|\n| 1 | 2 | 3 |")]);
    h.keys("down right right right right right");
    h.type_text("x");
    assert_eq!(h.text(), "| a | b |\n|---|---|\n| 1 | 2x | 3 |");
}

#[test]
fn table_enter_on_the_alignment_row_does_not_split_the_table() {
    let mut h = Harness::new(80, 24, &[("a.md", "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |")]);
    h.frame();
    let (x, y) = h.vt.find("├").unwrap();
    h.click(x + 1, y, 0);
    h.keys("enter");
    h.type_text("x");
    assert_eq!(h.text(), "intro\n\n| a | b |\n|---|---|\n| 1x | 2 |");
}
