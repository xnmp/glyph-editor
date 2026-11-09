use glyph::testing::Harness;
use std::time::Instant;
fn main() {
    let mut doc = String::new();
    for i in 0..20_000 {
        doc.push_str(&format!("## Section {i}\nSome **bold** text with a [[link]] and `code` #tag {i}\n- item {i}\n\n"));
    }
    let mut h = Harness::new(120, 50, &[("big.md", &doc)]);
    h.frame();
    let lines: Vec<String> = h.app.tabs[0].ed.buf.lines.clone();
    let t = Instant::now();
    for _ in 0..10 { glyph::markdown::parse_document(&lines); }
    println!("parse_document: {:?}", t.elapsed() / 10);
    let t = Instant::now();
    for _ in 0..10 { h.app.tabs[0].ed.word_count(); }
    println!("word_count: {:?}", t.elapsed() / 10);
    let t = Instant::now();
    for _ in 0..10 { h.app.tabs[0].ed.buf.text(); }
    println!("text(): {:?}", t.elapsed() / 10);
    let t = Instant::now();
    for _ in 0..20 { h.app.handle(glyph::input::Event::Key(glyph::input::Key::text("x"))); }
    println!("handle key: {:?}", t.elapsed() / 20);
    let t = Instant::now();
    for _ in 0..20 { h.app.render(); }
    println!("render (no change): {:?}", t.elapsed() / 20);
}
