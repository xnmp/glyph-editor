#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::manual_clamp,
    clippy::if_same_then_else,
    clippy::collapsible_if,
    clippy::explicit_counter_loop,
    clippy::manual_strip,
    clippy::match_single_binding,
    clippy::needless_late_init
)]
//! glyph — a WYSIWYG Markdown editor for the terminal.

pub mod app;
pub mod buffer;
pub mod commands;
pub mod diacritics;
pub mod editor;
pub mod fuzzy;
pub mod graphics;
pub mod highlight;
pub mod images;
pub mod input;
pub mod layout;
pub mod markdown;
pub mod mathtext;
pub mod screen;
pub mod style;
pub mod term;
pub mod testing;
pub mod text;
pub mod theme;
pub mod ui;
pub mod vault;
pub mod view;
pub mod vt;
