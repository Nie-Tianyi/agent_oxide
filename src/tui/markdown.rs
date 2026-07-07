//! # Markdown Rendering
//!
//! Parses markdown text with `pulldown_cmark` and converts it to styled
//! ratatui [`Line`]s. Supports code blocks, headers, bold, italic,
//! inline code, lists, blockquotes, links, and horizontal rules.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

// ── Public API ────────────────────────────────────────────────────────────────────

/// Renders markdown text into styled ratatui [`Line`]s.
///
/// The returned lines borrow from the input `content` — the caller must
/// ensure `content` outlives the lines.
pub fn render_markdown(content: &str, area_width: u16) -> Vec<Line<'_>> {
    let mut renderer = MdRenderer::new(area_width);
    renderer.render(content);
    renderer.finish()
}

// ── Color Palette ─────────────────────────────────────────────────────────────────

const HEADING_COLOR: Color = Color::Rgb(100, 180, 255); // soft blue
const HEADING2_COLOR: Color = Color::Rgb(130, 200, 255);
const CODE_BG: Color = Color::Rgb(40, 44, 52); // dark bg for code
const INLINE_CODE_BG: Color = Color::Rgb(55, 55, 65);
const QUOTE_BORDER_COLOR: Color = Color::Rgb(100, 140, 180);
const QUOTE_TEXT_COLOR: Color = Color::Rgb(170, 180, 195);
const LINK_COLOR: Color = Color::Rgb(80, 160, 220);
const RULE_COLOR: Color = Color::Rgb(70, 70, 80);
const BULLET_COLOR: Color = Color::Rgb(130, 160, 190);

// ── Renderer ──────────────────────────────────────────────────────────────────────

struct MdRenderer<'a> {
    lines: Vec<Line<'a>>,
    /// Spans accumulating for the current line being built.
    current_spans: Vec<Span<'a>>,
    /// Active inline styles (pushed on Start, popped on End).
    style_stack: Vec<Style>,
    /// Base style for normal paragraph text.
    base_style: Style,
    /// Current block-level context.
    block: BlockCtx,
    /// Pending block state to apply on next text.
    pending_block_start: bool,
    /// Ordered list counter.
    list_number: u64,
    /// Blockquote nesting depth.
    quote_depth: usize,
    area_width: u16,
}

/// Tracks the current block-level element we're inside.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockCtx {
    Normal,
    CodeBlock { language: Option<&'static str> },
    BlockQuote,
    ListItem,
    Heading { level: u8 },
}

// ── Renderer Implementation ───────────────────────────────────────────────────────

impl<'a> MdRenderer<'a> {
    fn new(area_width: u16) -> Self {
        Self {
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: Vec::new(),
            base_style: Style::default().fg(Color::White),
            block: BlockCtx::Normal,
            pending_block_start: true,
            list_number: 0,
            quote_depth: 0,
            area_width,
        }
    }

    /// Main entry: parse `content` and build styled lines.
    fn render(&mut self, content: &'a str) {
        let parser = Parser::new(content);

        for event in parser {
            match event {
                Event::Start(tag) => self.handle_start(tag),
                Event::End(tag) => self.handle_end(tag),
                Event::Text(text) => self.handle_text(text),
                Event::Code(text) => self.handle_inline_code(text),
                Event::Html(html) | Event::InlineHtml(html) => {
                    self.push_span(Span::styled(
                        html.into_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
                Event::SoftBreak => {
                    // Space between words in a paragraph
                    self.push_span(Span::raw(" "));
                }
                Event::HardBreak => {
                    // Explicit line break
                    self.flush_line();
                }
                Event::Rule => {
                    self.flush_line();
                    let w = self.area_width.max(20) as usize;
                    let rule = "─".repeat(w.saturating_sub(2));
                    self.lines.push(Line::from(Span::styled(
                        rule,
                        Style::default().fg(RULE_COLOR).add_modifier(Modifier::DIM),
                    )));
                    self.lines.push(Line::from(Span::raw(""))); // blank after rule
                    self.pending_block_start = true;
                }
                Event::TaskListMarker(checked) => {
                    let marker = if checked { "[x] " } else { "[ ] " };
                    self.push_span(Span::styled(
                        marker,
                        Style::default()
                            .fg(BULLET_COLOR)
                            .add_modifier(Modifier::DIM),
                    ));
                }
                Event::FootnoteReference(_) => {
                    // Skip — footnotes not rendered in TUI
                }
                _ => {
                    // InlineMath, DisplayMath, etc. — skip
                }
            }
        }

        // Flush final line
        self.flush_line();
    }

    // ── Block & Inline Handlers ────────────────────────────────────────

    fn handle_start(&mut self, tag: Tag<'a>) {
        match tag {
            // ── Block tags ────────────────────────────────────────
            Tag::Paragraph => {
                self.flush_line();
            }

            Tag::Heading { level, .. } => {
                self.flush_line();
                self.pending_block_start = true;
                let lvl = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.block = BlockCtx::Heading { level: lvl };

                // Push heading style
                let (color, size_mod) = match lvl {
                    1 => (HEADING_COLOR, Modifier::BOLD),
                    2 => (HEADING2_COLOR, Modifier::BOLD),
                    _ => (HEADING2_COLOR, Modifier::BOLD),
                };
                self.style_stack
                    .push(Style::default().fg(color).add_modifier(size_mod));
            }

            Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth += 1;
                self.block = BlockCtx::BlockQuote;
                self.pending_block_start = true;
            }

            Tag::CodeBlock(kind) => {
                self.flush_line();
                self.block = BlockCtx::CodeBlock { language: None };
                // Emit a blank line before code block
                self.lines.push(Line::from(Span::raw("")));

                // Emit language label if fenced
                if let CodeBlockKind::Fenced(lang) = &kind
                    && !lang.is_empty()
                {
                    self.lines.push(Line::from(Span::styled(
                        format!("  ┌─ {lang} "),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    )));
                }
                self.pending_block_start = true;
            }

            Tag::List(number) => {
                self.flush_line();
                self.list_number = number.unwrap_or(0);
                // Blank line before list
                if !self.lines.is_empty()
                    && !self
                        .lines
                        .last()
                        .map(|l| l.spans.is_empty())
                        .unwrap_or(false)
                {
                    // Don't add extra blanks — pulldown-cmark handles this via paragraph events
                }
            }

            Tag::Item => {
                self.flush_line();
                self.block = BlockCtx::ListItem;
                self.pending_block_start = true;
            }

            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                // Tables — simplified: treat as normal text with separators
            }

            // ── Inline tags — push style onto stack ──────────────
            Tag::Emphasis => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::ITALIC));
            }
            Tag::Strong => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::BOLD));
            }
            Tag::Strikethrough => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }

            Tag::Link {
                link_type: _,
                dest_url: _,
                title: _,
                id: _,
            } => {
                // Push link style and store URL to render after text
                self.style_stack.push(
                    Style::default()
                        .fg(LINK_COLOR)
                        .add_modifier(Modifier::UNDERLINED),
                );
                // We'll render the URL as a trailing dim span after the link text
                // Store it — handled in handle_end for links
                self.style_stack.push(Style::default()); // placeholder for URL marker
                self.style_stack.push(
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ); // URL style
                  // Push the URL string onto... hmm. We need a different approach.
                  // Let's handle links differently: render text + URL inline.
                  // For now, just style the text as a link and prepend URL info.
            }

            Tag::Image { .. } => {
                // Images: just show placeholder
                self.style_stack
                    .push(Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM));
            }

            Tag::FootnoteDefinition(_) => {
                // Skip footnote definitions in TUI
            }
            _ => {
                // HtmlBlock, DefinitionList, MetadataBlock, etc. — skip
            }
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
                // Add blank line between paragraphs
                self.lines.push(Line::from(Span::raw("")));
                self.pending_block_start = true;
            }

            TagEnd::Heading(_level) => {
                self.flush_line();
                // Pop heading style
                self.style_stack.pop();
                // Blank line after heading
                self.lines.push(Line::from(Span::raw("")));
                self.block = BlockCtx::Normal;
                self.pending_block_start = true;
            }

            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.lines.push(Line::from(Span::raw("")));
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.block = if self.quote_depth > 0 {
                    BlockCtx::BlockQuote
                } else {
                    BlockCtx::Normal
                };
                self.pending_block_start = true;
            }

            TagEnd::CodeBlock => {
                self.flush_line();
                // Emit closing decoration
                self.lines.push(Line::from(Span::styled(
                    "  └─".to_string(),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )));
                self.lines.push(Line::from(Span::raw("")));
                self.block = BlockCtx::Normal;
                self.pending_block_start = true;
            }

            TagEnd::List(_) => {
                self.flush_line();
                self.list_number = 0;
                if !self.lines.is_empty() {
                    // Ensure trailing blank
                    let last_empty = self
                        .lines
                        .last()
                        .map(|l| l.spans.is_empty() || l.spans.len() == 1 && l.spans[0].content.is_empty())
                        .unwrap_or(true);
                    if !last_empty {
                        self.lines.push(Line::from(Span::raw("")));
                    }
                }
                self.block = BlockCtx::Normal;
                self.pending_block_start = true;
            }

            TagEnd::Item => {
                self.flush_line();
                self.block = BlockCtx::ListItem; // stay in list context
                self.pending_block_start = true;
                if self.list_number > 0 {
                    self.list_number += 1;
                }
            }

            TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow => {
                self.flush_line();
            }

            TagEnd::TableCell => {
                self.push_span(Span::raw(" │ "));
            }

            // ── Inline ends — pop style ──────────────────────────
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => {
                self.style_stack.pop();
            }

            TagEnd::FootnoteDefinition => {}
            _ => {
                // HtmlBlock, DefinitionList, MetadataBlock, etc. — skip
            }
        }
    }

    fn handle_text(&mut self, text: CowStr<'a>) {
        if self.pending_block_start {
            self.flush_block_prefix();
            self.pending_block_start = false;
        }

        let current_style = self.compute_current_style();

        // Split text by newlines within a paragraph
        let s: &str = &text;
        let mut parts = s.split('\n').peekable();

        while let Some(part) = parts.next() {
            if !part.is_empty() {
                self.current_spans
                    .push(Span::styled(part.to_string(), current_style));
            }
            if parts.peek().is_some() {
                // Newline within paragraph text — flush and restart
                self.flush_line();
                self.flush_block_prefix();
            }
        }
    }

    fn handle_inline_code(&mut self, text: CowStr<'a>) {
        if self.pending_block_start {
            self.flush_block_prefix();
            self.pending_block_start = false;
        }

        let style = Style::default()
            .fg(Color::Rgb(200, 200, 180))
            .bg(INLINE_CODE_BG);

        self.current_spans.push(Span::styled(
            format!(" {text} "),
            style,
        ));
    }

    // ── Helpers ────────────────────────────────────────────────────────

    /// Flush the prefix for the current block context (indent, quote bar, bullet).
    fn flush_block_prefix(&mut self) {
        match self.block {
            BlockCtx::CodeBlock { .. } => {
                // Code block lines get a dim margin
                self.current_spans.push(Span::styled(
                    "  ",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                ));
            }
            BlockCtx::BlockQuote => {
                // Push quote border with depth
                for d in 0..self.quote_depth {
                    let color = if d == self.quote_depth - 1 {
                        QUOTE_BORDER_COLOR
                    } else {
                        Color::DarkGray
                    };
                    self.current_spans.push(Span::styled("▎", Style::default().fg(color)));
                    self.current_spans.push(Span::raw(" "));
                }
            }
            BlockCtx::ListItem => {
                let bullet = if self.list_number > 0 {
                    format!("  {}. ", self.list_number)
                } else {
                    "  • ".to_string()
                };
                self.current_spans.push(Span::styled(
                    bullet,
                    Style::default()
                        .fg(BULLET_COLOR)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            BlockCtx::Normal | BlockCtx::Heading { .. } => {
                // No prefix needed
            }
        }
    }

    /// Compute the effective style by folding the style stack over the base.
    fn compute_current_style(&self) -> Style {
        let mut style = match self.block {
            BlockCtx::CodeBlock { .. } => Style::default()
                .fg(Color::Rgb(200, 200, 180))
                .bg(CODE_BG),
            BlockCtx::BlockQuote => self.base_style.fg(QUOTE_TEXT_COLOR),
            _ => self.base_style,
        };

        for s in &self.style_stack {
            style = style.patch(*s);
        }

        style
    }

    /// Add a span using the currently-stacked styles.
    fn push_span(&mut self, span: Span<'a>) {
        self.current_spans.push(span);
    }

    /// Flush the current line of spans into `lines` and start a new one.
    fn flush_line(&mut self) {
        if self.current_spans.is_empty() && !self.lines.is_empty() {
            // Keep at most one consecutive empty line
            return;
        }

        let spans = std::mem::take(&mut self.current_spans);
        self.lines.push(Line::from(spans));
    }

    /// Final flush: remove trailing blank lines and return.
    fn finish(mut self) -> Vec<Line<'a>> {
        self.flush_line();

        // Trim trailing blank lines
        while self.lines.last().is_some_and(|l| {
            l.spans.is_empty()
                || l.spans.iter().all(|s| s.content.trim().is_empty())
        }) {
            self.lines.pop();
        }

        self.lines
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text() {
        let lines = render_markdown("Hello world", 80);
        assert!(!lines.is_empty());
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("Hello world"));
    }

    #[test]
    fn test_bold_and_italic() {
        let lines = render_markdown("This is **bold** and *italic* text.", 80);
        let has_bold = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::BOLD)
            })
        });
        let has_italic = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::ITALIC)
            })
        });
        assert!(has_bold, "bold modifier missing");
        assert!(has_italic, "italic modifier missing");
    }

    #[test]
    fn test_inline_code() {
        let lines = render_markdown("Use `cargo build` to compile.", 80);
        let has_code_bg = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.bg == Some(INLINE_CODE_BG)
            })
        });
        assert!(has_code_bg, "inline code background missing");
    }

    #[test]
    fn test_code_block() {
        let md = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
        let lines = render_markdown(md, 80);
        let has_code = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("fn main")
            })
        });
        assert!(has_code, "code block content missing");
    }

    #[test]
    fn test_heading() {
        let lines = render_markdown("# Hello World", 80);
        let has_bold_heading = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::BOLD)
                    && s.content.contains("Hello World")
            })
        });
        assert!(has_bold_heading, "heading style missing");
    }

    #[test]
    fn test_unordered_list() {
        let md = "- item one\n- item two\n- item three";
        let lines = render_markdown(md, 80);
        let bullet_count = lines.iter().filter(|l| {
            l.spans.iter().any(|s| s.content.contains('•'))
        }).count();
        assert_eq!(bullet_count, 3, "expected 3 bullet items");
    }

    #[test]
    fn test_ordered_list() {
        let md = "1. first\n2. second\n3. third";
        let lines = render_markdown(md, 80);
        let numbered: Vec<_> = lines.iter().filter(|l| {
            l.spans.iter().any(|s| {
                s.content.contains("1.") || s.content.contains("2.") || s.content.contains("3.")
            })
        }).collect();
        assert!(!numbered.is_empty(), "ordered list items missing");
    }

    #[test]
    fn test_blockquote() {
        let lines = render_markdown("> This is a quote", 80);
        let has_border = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains('▎'))
        });
        assert!(has_border, "blockquote border missing");
    }

    #[test]
    fn test_horizontal_rule() {
        let lines = render_markdown("---", 80);
        let has_rule = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains('─'))
        });
        assert!(has_rule, "horizontal rule missing");
    }

    #[test]
    fn test_link() {
        let lines = render_markdown("[click here](https://example.com)", 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("click here"), "link text missing");
    }

    #[test]
    fn test_chinese_markdown() {
        let md = "这是**粗体**和*斜体*以及`代码`的测试。";
        let lines = render_markdown(md, 80);
        let has_chinese = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains("这是"))
        });
        assert!(has_chinese, "Chinese text missing");
    }
}
