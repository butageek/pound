# Pound sample document

Welcome to **Pound** — press `Ctrl+U` or click **Source** (top right) to see
this file's markdown side by side with the rendering.

## Text

Regular text with *emphasis*, **strong**, ***both***, ~~strikethrough~~ and
`inline code`. [Links open in your browser](https://www.rust-lang.org).

## Lists

1. Ordered item one
2. Ordered item two
   - nested bullet
   - another *nested* bullet
3. Ordered item three

- [x] shipped: markdown rendering
- [x] shipped: side-by-side source
- [ ] todo: syntax highlighting

## Quotes

> Markdown is intended to be as easy-to-read and easy-to-write as is
> feasible.
>
> — John Gruber

## Code

```rust
fn main() {
    println!("Hello, pound!");
}
```

## Tables

| Feature   | State | Notes                     |
|-----------|:-----:|---------------------------|
| Render    |  ✅   | pulldown-cmark + WebView2  |
| Source    |  ✅   | side-by-side, exact 50/50  |
| Highlight |  ⏳   | roadmap                   |

---

That's a horizontal rule. Images work too when they exist next to the file:
![missing image placeholder](does/not/exist.png)
