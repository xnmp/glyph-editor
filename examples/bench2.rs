use glyph::testing::Harness;
use std::time::Instant;
fn main() {
    let mut doc = String::new();
    for i in 0..20_000 {
        doc.push_str(&format!("## Section {i}\nSome **bold** text with a [[link]] and `code` #tag {i}\n- item {i}\n\n"));
    }
    let mut h = Harness::new(120, 50, &[("big.md", &doc)]);
    h.frame();
    let ed = &mut h.app.tabs[0].ed;
    ed.doc();
    let t = Instant::now();
    for _ in 0..20 { ed.insert("x"); }
    println!("ed.insert: {:?}", t.elapsed() / 20);
    let t = Instant::now();
    for _ in 0..20 { ed.insert("x"); ed.doc(); }
    println!("ed.insert+doc: {:?}", t.elapsed() / 20);
    let t = Instant::now();
    for _ in 0..20 { ed.type_char('y'); }
    println!("type_char: {:?}", t.elapsed() / 20);
}
