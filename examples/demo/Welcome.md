---
title: Welcome to glyph
tags: [markdown, terminal, kitty]
created: 2026-11-09
---
# Welcome to glyph

glyph is a **WYSIWYG** Markdown editor that lives *in your terminal*. Syntax hides
itself until your cursor touches it — just like Obsidian's live preview — and it
uses the ==kitty graphics protocol== for images and big headings.

## Text formatting

You can write **bold**, *italic*, ***both***, ~~strikethrough~~, ==highlights==,
`inline code`, and [links](https://sw.kovidgoyal.net/kitty/). Inline math works
too: $e^{i\pi} + 1 = 0$ and $\sum_{i=1}^{n} x_i^2$. Footnotes look like this[^1].

Link to other notes with [[Projects/Roadmap|the roadmap]] or [[Daily Log]], and tag
things with #ideas or #todo/later.

> [!tip] Callouts
> Callouts are rendered with an icon, a color and a tinted background.
> They can contain **formatting** and lists:
> - like this one

> [!warning]- Folded callout
> This body is hidden until you move the cursor into it.

> A plain blockquote, for when you just want to quote someone.

### Lists and tasks

- Bullet lists render with real bullets
    - and nested indent guides
        - at every level
1. Numbered lists
2. keep their numbers
- [ ] An open task — click the box or press Ctrl+L
- [x] A finished task
- [/] In progress

### Tables

| Feature        | Status | Notes                  |
| -------------- | :----: | ---------------------- |
| Live preview   |   ✔    | syntax hides itself    |
| Images         |   ✔    | kitty graphics         |
| Big headings   |   ✔    | OSC 66 text sizing     |
| Tables         |   ✔    | Tab moves between cells |

### Code

```rust
fn main() {
    // syntax highlighted with your terminal's palette
    let greeting = "hello, glyph";
    println!("{greeting} {}", 42);
}
```

### Images

![A gradient|420](assets/aurora.png)

### Embeds

![[Daily Log]]

### Math

$$
\int_0^\infty e^{-x^2} dx = \frac{\sqrt{\pi}}{2}
$$

---

Press **Ctrl+P** for the command palette, **Ctrl+O** to open a note, **F1** for help.

[^1]: Footnotes are shown as superscripts.
