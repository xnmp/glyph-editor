//! Commands, their palette names, and default key bindings (micro-style
//! basics + Obsidian shortcuts).

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cmd {
    Save,
    SaveAll,
    Quit,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    SelectAll,
    SelectLine,
    SelectWord,
    Find,
    Replace,
    FindNext,
    FindPrev,
    QuickSwitch,
    Palette,
    NewNote,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    ToggleReading,
    ToggleSource,
    ToggleLeft,
    ToggleRight,
    FocusFiles,
    FocusOutline,
    Bold,
    Italic,
    Strike,
    Highlight,
    InlineCode,
    Link,
    WikiLink,
    ToggleTask,
    Paragraph,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    CycleHeading,
    Quote,
    BulletList,
    NumberedList,
    CodeBlock,
    InsertTable,
    InsertHr,
    MoveLineUp,
    MoveLineDown,
    Duplicate,
    DeleteLine,
    JoinLines,
    Indent,
    Outdent,
    FollowLink,
    FollowLinkNewTab,
    Back,
    Forward,
    GoToLine,
    SearchVault,
    Help,
    FormatTable,
    ToggleLineNumbers,
    CycleTheme,
    ReloadFile,
    RenameNote,
    ToggleAutosave,
    ToggleReadable,
    Suspend,
    CenterCursor,
    ScrollUp,
    ScrollDown,
    OpenDaily,
}

use Cmd::*;

/// (command, palette title, default keys)
pub const COMMANDS: &[(Cmd, &str, &[&str])] = &[
    (Save, "Save file", &["ctrl+s"]),
    (SaveAll, "Save all files", &[]),
    (Quit, "Quit", &["ctrl+q"]),
    (Undo, "Undo", &["ctrl+z"]),
    (Redo, "Redo", &["ctrl+y", "ctrl+shift+z"]),
    (Copy, "Copy (line if no selection)", &["ctrl+c"]),
    (Cut, "Cut (line if no selection)", &["ctrl+x"]),
    (Paste, "Paste", &["ctrl+v"]),
    (SelectAll, "Select all", &["ctrl+a"]),
    (SelectLine, "Select line", &["ctrl+shift+l"]),
    (SelectWord, "Select word", &["alt+w"]),
    (Find, "Find in file", &["ctrl+f"]),
    (Replace, "Find and replace", &["ctrl+h", "ctrl+r"]),
    (FindNext, "Find next", &["f3"]),
    (FindPrev, "Find previous", &["shift+f3"]),
    (QuickSwitch, "Quick switcher: open note", &["ctrl+o"]),
    (Palette, "Command palette", &["ctrl+p"]),
    (NewNote, "New note", &["ctrl+n"]),
    (NewTab, "New tab", &["ctrl+t"]),
    (CloseTab, "Close tab", &["ctrl+w"]),
    (NextTab, "Next tab", &["ctrl+tab", "ctrl+pagedown", "alt+."]),
    (PrevTab, "Previous tab", &["ctrl+shift+tab", "ctrl+pageup", "alt+,"]),
    (ToggleReading, "Toggle reading view", &["ctrl+e"]),
    (ToggleSource, "Toggle source mode", &["alt+e"]),
    (ToggleLeft, "Toggle file explorer", &["ctrl+\\", "alt+b"]),
    (ToggleRight, "Toggle outline / backlinks panel", &["ctrl+shift+\\", "ctrl+|", "alt+\\"]),
    (FocusFiles, "Focus file explorer", &["alt+1"]),
    (FocusOutline, "Focus outline", &["alt+2"]),
    (Bold, "Toggle bold", &["ctrl+b"]),
    (Italic, "Toggle italic", &["ctrl+i"]),
    (Strike, "Toggle strikethrough", &["ctrl+shift+x", "alt+s"]),
    (Highlight, "Toggle highlight", &["alt+h"]),
    (InlineCode, "Toggle inline code", &["ctrl+`", "alt+`"]),
    (Link, "Insert markdown link", &["ctrl+k"]),
    (WikiLink, "Insert wikilink", &["alt+k"]),
    (ToggleTask, "Toggle checklist status", &["ctrl+l", "ctrl+enter"]),
    (Paragraph, "Set paragraph (remove heading)", &["ctrl+0"]),
    (H1, "Heading 1", &["ctrl+1"]),
    (H2, "Heading 2", &["ctrl+2"]),
    (H3, "Heading 3", &["ctrl+3"]),
    (H4, "Heading 4", &["ctrl+4"]),
    (H5, "Heading 5", &["ctrl+5"]),
    (H6, "Heading 6", &["ctrl+6"]),
    (CycleHeading, "Cycle heading level", &["alt+#"]),
    (Quote, "Toggle blockquote", &["alt+q"]),
    (BulletList, "Toggle bullet list", &["alt+-"]),
    (NumberedList, "Toggle numbered list", &["alt+n"]),
    (CodeBlock, "Insert code block", &["alt+c"]),
    (InsertTable, "Insert table", &["alt+t"]),
    (InsertHr, "Insert horizontal rule", &[]),
    (MoveLineUp, "Move line up", &["alt+up"]),
    (MoveLineDown, "Move line down", &["alt+down"]),
    (Duplicate, "Duplicate line / selection", &["ctrl+d"]),
    (DeleteLine, "Delete line", &["ctrl+shift+k", "ctrl+shift+d"]),
    (JoinLines, "Join lines", &["ctrl+j"]),
    (Indent, "Indent", &["ctrl+]"]),
    (Outdent, "Outdent", &["ctrl+["]),
    (FollowLink, "Follow link under cursor", &["alt+enter", "f12"]),
    (FollowLinkNewTab, "Follow link in new tab", &["alt+shift+enter"]),
    (Back, "Navigate back", &["alt+left"]),
    (Forward, "Navigate forward", &["alt+right"]),
    (GoToLine, "Go to line", &["ctrl+g"]),
    (SearchVault, "Search all notes", &["ctrl+shift+f"]),
    (Help, "Help: key bindings", &["f1"]),
    (FormatTable, "Format table", &[]),
    (ToggleLineNumbers, "Toggle line numbers", &[]),
    (CycleTheme, "Cycle theme", &[]),
    (ReloadFile, "Reload file from disk", &[]),
    (RenameNote, "Rename note (updates links)", &["f2"]),
    (ToggleAutosave, "Toggle autosave", &[]),
    (ToggleReadable, "Toggle readable line length", &[]),
    (Suspend, "Suspend to shell", &[]),
    (CenterCursor, "Center cursor in view", &[]),
    (ScrollUp, "Scroll up", &["ctrl+up"]),
    (ScrollDown, "Scroll down", &["ctrl+down"]),
    (OpenDaily, "Open today's daily note", &["alt+d"]),
];

pub fn title(c: Cmd) -> &'static str {
    COMMANDS.iter().find(|x| x.0 == c).map_or("?", |x| x.1)
}

pub fn keys_of(c: Cmd) -> &'static [&'static str] {
    COMMANDS.iter().find(|x| x.0 == c).map_or(&[], |x| x.2)
}

pub fn lookup(key: &str) -> Option<Cmd> {
    COMMANDS.iter().find(|x| x.2.contains(&key)).map(|x| x.0)
}

/// Pretty key label: "ctrl+shift+f" → "Ctrl+Shift+F"
pub fn pretty(k: &str) -> String {
    k.split('+')
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) if p.len() > 1 => f.to_uppercase().collect::<String>() + c.as_str(),
                Some(f) => f.to_uppercase().collect(),
                None => "+".into(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_duplicate_bindings() {
        let mut seen = std::collections::HashMap::new();
        for (c, _, keys) in COMMANDS {
            for k in *keys {
                if let Some(prev) = seen.insert(*k, *c) {
                    panic!("{k} bound to both {prev:?} and {c:?}");
                }
            }
        }
        assert_eq!(lookup("ctrl+s"), Some(Cmd::Save));
        assert_eq!(pretty("ctrl+shift+f"), "Ctrl+Shift+F");
    }
}
