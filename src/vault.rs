//! The vault: the folder of notes around the opened file. Note index, link
//! resolution (Obsidian rules), backlinks, tags and full-text search.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::markdown::{self as md, SpanKind};

const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".obsidian", ".trash", "__pycache__", ".venv", "venv"];
const MAX_FILES: usize = 50_000;

#[derive(Default)]
struct FileIndex {
    mtime: Option<SystemTime>,
    /// (line, link target (lowercase, no ext), line text)
    links: Vec<(usize, String, String)>,
    tags: Vec<String>,
}

pub struct Vault {
    pub root: PathBuf,
    /// all files, relative to root, sorted
    pub files: Vec<PathBuf>,
    /// markdown notes (relative)
    pub notes: Vec<PathBuf>,
    stems: HashMap<String, Vec<PathBuf>>,
    rel_noext: HashSet<String>,
    pub by_name: HashMap<String, PathBuf>,
    index: HashMap<PathBuf, FileIndex>,
    pub truncated: bool,
}

pub fn is_note(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("md" | "markdown" | "mdown" | "mkd"))
}

fn key(s: &str) -> String {
    s.trim().trim_end_matches(".md").to_lowercase()
}

impl Vault {
    pub fn open(root: &Path) -> Vault {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut v = Vault { root, files: vec![], notes: vec![], stems: HashMap::new(), rel_noext: HashSet::new(), by_name: HashMap::new(), index: HashMap::new(), truncated: false };
        v.rescan();
        v
    }

    pub fn rescan(&mut self) {
        let mut files = vec![];
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                let p = e.path();
                if ft.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(p);
                    }
                } else if ft.is_file() || ft.is_symlink() {
                    if let Ok(rel) = p.strip_prefix(&self.root) {
                        files.push(rel.to_path_buf());
                    }
                    if files.len() >= MAX_FILES {
                        self.truncated = true;
                        break;
                    }
                }
            }
            if files.len() >= MAX_FILES {
                break;
            }
        }
        files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        self.notes = files.iter().filter(|p| is_note(p)).cloned().collect();
        self.stems.clear();
        self.rel_noext.clear();
        self.by_name.clear();
        for f in &files {
            self.by_name.entry(f.file_name().unwrap_or_default().to_string_lossy().to_lowercase()).or_insert_with(|| self.root.join(f));
            let rel = f.to_string_lossy().replace('\\', "/");
            if is_note(f) {
                let stem = f.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
                self.stems.entry(stem).or_default().push(f.clone());
                self.rel_noext.insert(key(&rel));
            } else {
                self.rel_noext.insert(rel.to_lowercase());
            }
        }
        self.files = files;
    }

    /// Does a wikilink target resolve to something in the vault?
    pub fn exists(&self, target: &str) -> bool {
        if target.is_empty() {
            return true;
        }
        let k = key(target);
        self.stems.contains_key(&k) || self.rel_noext.contains(&k) || self.by_name.contains_key(&target.to_lowercase())
    }

    /// Resolve a link target to an absolute path (closest match to `from`).
    pub fn resolve(&self, target: &str, from: Option<&Path>) -> Option<PathBuf> {
        if target.is_empty() {
            return from.map(Path::to_path_buf);
        }
        let t = target.trim();
        // relative/absolute path links
        if let Some(from) = from.and_then(|f| f.parent()) {
            for cand in [from.join(t), from.join(format!("{t}.md"))] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
        for cand in [self.root.join(t), self.root.join(format!("{t}.md"))] {
            if cand.is_file() {
                return Some(cand);
            }
        }
        let k = key(t);
        let name = k.rsplit('/').next().unwrap_or(&k).to_string();
        if let Some(c) = self.stems.get(&name) {
            // prefer a path whose suffix matches the full target, then the closest to `from`
            let best = c.iter().filter(|p| key(&p.to_string_lossy().replace('\\', "/")).ends_with(&k)).min_by_key(|p| {
                let from_dir = from.and_then(|f| f.parent()).and_then(|d| d.strip_prefix(&self.root).ok()).map(Path::to_path_buf).unwrap_or_default();
                if p.starts_with(&from_dir) { 0 } else { p.components().count() }
            });
            if let Some(b) = best.or(c.first()) {
                return Some(self.root.join(b));
            }
        }
        self.by_name.get(&t.to_lowercase()).cloned()
    }

    /// Where a new note for `target` should be created.
    pub fn new_note_path(&self, target: &str, from: Option<&Path>) -> PathBuf {
        let t = target.trim();
        let t = if t.ends_with(".md") { t.to_string() } else { format!("{t}.md") };
        if t.contains('/') {
            self.root.join(t)
        } else {
            from.and_then(|f| f.parent()).map(|d| d.join(&t)).unwrap_or_else(|| self.root.join(&t))
        }
    }

    fn refresh_index(&mut self) {
        let notes = self.notes.clone();
        let live: HashSet<&PathBuf> = notes.iter().collect();
        self.index.retain(|k, _| live.contains(k));
        for rel in &notes {
            let abs = self.root.join(rel);
            let mtime = std::fs::metadata(&abs).and_then(|m| m.modified()).ok();
            let ent = self.index.entry(rel.clone()).or_default();
            if ent.mtime == mtime && mtime.is_some() {
                continue;
            }
            ent.mtime = mtime;
            ent.links.clear();
            ent.tags.clear();
            let Ok(text) = std::fs::read_to_string(&abs) else { continue };
            for (i, line) in text.lines().enumerate() {
                if !(line.contains("[[") || line.contains("](") || line.contains('#')) {
                    continue;
                }
                for s in md::parse_inline(line, 0, line.len()) {
                    match s.kind {
                        SpanKind::WikiLink | SpanKind::Embed => ent.links.push((i, key(&s.target), line.to_string())),
                        SpanKind::Link if !s.target.contains("://") => {
                            let t = crate::editor::percent_decode(s.target.split('#').next().unwrap_or(""));
                            if !t.is_empty() {
                                ent.links.push((i, key(&t), line.to_string()));
                            }
                        }
                        SpanKind::Tag => ent.tags.push(s.target.clone()),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Notes linking to `path`: (relative path, line, line text).
    pub fn backlinks(&mut self, path: &Path) -> Vec<(PathBuf, usize, String)> {
        self.refresh_index();
        let rel = path.strip_prefix(&self.root).unwrap_or(path).to_path_buf();
        let stem = key(&path.file_stem().unwrap_or_default().to_string_lossy());
        let relk = key(&rel.to_string_lossy().replace('\\', "/"));
        let mut out = vec![];
        for (p, ent) in &self.index {
            if *p == rel {
                continue;
            }
            for (ln, tgt, text) in &ent.links {
                let t = tgt.rsplit('/').next().unwrap_or(tgt);
                if *tgt == relk || (t == stem && (!tgt.contains('/') || relk.ends_with(tgt.as_str()))) {
                    out.push((p.clone(), *ln, text.trim().to_string()));
                }
            }
        }
        out.sort();
        out
    }

    pub fn tags(&mut self) -> Vec<(String, usize)> {
        self.refresh_index();
        let mut m: HashMap<String, usize> = HashMap::new();
        for ent in self.index.values() {
            for t in &ent.tags {
                *m.entry(t.clone()).or_default() += 1;
            }
        }
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// Case-insensitive full-text search: (relative path, line, byte col, line text).
    pub fn search(&self, q: &str, limit: usize) -> Vec<(PathBuf, usize, usize, String)> {
        let ql = q.to_lowercase();
        let mut out = vec![];
        if ql.is_empty() {
            return out;
        }
        for rel in &self.notes {
            let Ok(text) = std::fs::read_to_string(self.root.join(rel)) else { continue };
            for (i, line) in text.lines().enumerate() {
                let ll = line.to_lowercase();
                if let Some(c) = ll.find(&ql) {
                    let c = if ll.len() == line.len() { c } else { 0 };
                    out.push((rel.clone(), i, c, line.to_string()));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    pub fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().to_string()
    }
}

/// Pick the vault root for a path: the nearest ancestor containing `.obsidian`
/// or `.git`, else the file's directory.
pub fn find_root(start: &Path) -> PathBuf {
    let start = if start.is_dir() { start.to_path_buf() } else { start.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")) };
    let start = if start.as_os_str().is_empty() { PathBuf::from(".") } else { start };
    let abs = std::fs::canonicalize(&start).unwrap_or(start.clone());
    let mut cur = Some(abs.as_path());
    while let Some(d) = cur {
        if d.join(".obsidian").is_dir() {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    let mut cur = Some(abs.as_path());
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    abs
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_backlinks_tags() {
        let root = std::env::temp_dir().join(format!("glyph-vault-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(root.join("Home.md"), "Go to [[Target]] and [x](sub/Deep.md) #idea").unwrap();
        std::fs::write(root.join("sub/Target.md"), "# Target\nback to [[home|Home]] #idea #todo").unwrap();
        std::fs::write(root.join("sub/Deep.md"), "deep").unwrap();
        std::fs::write(root.join("sub/pic.png"), "x").unwrap();
        let mut v = Vault::open(&root);
        assert_eq!(v.notes.len(), 3);
        assert!(v.exists("Target"));
        assert!(v.exists("sub/Deep"));
        assert!(v.exists("pic.png"));
        assert!(!v.exists("Nope"));
        assert_eq!(v.resolve("Target", None), Some(root.join("sub/Target.md")));
        assert_eq!(v.resolve("sub/Deep.md", Some(&root.join("Home.md"))), Some(root.join("sub/Deep.md")));
        let bl = v.backlinks(&root.join("sub/Target.md"));
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].0, PathBuf::from("Home.md"));
        let bl = v.backlinks(&root.join("Home.md"));
        assert_eq!(bl.len(), 1);
        let bl = v.backlinks(&root.join("sub/Deep.md"));
        assert_eq!(bl.len(), 1);
        assert_eq!(v.tags()[0], ("idea".to_string(), 2));
        assert_eq!(v.search("BACK TO", 10).len(), 1);
        assert_eq!(find_root(&root.join("sub/Deep.md")), std::fs::canonicalize(&root).unwrap());
        assert_eq!(v.new_note_path("New", Some(&root.join("sub/Deep.md"))), root.join("sub/New.md"));
    }
}
