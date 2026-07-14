//! Debug overlay — scrollable table of recent trace events.
//!
//! Toggled via `Ctrl+O` or `/debug`. Displays trace events from the
//! shared [`TraceStore`] in a scrollable overlay panel.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use observability::{Timestamped, TraceEvent, TraceStore};

/// Maximum number of recent events to display.
const MAX_DISPLAY_EVENTS: usize = 100;

/// Scrollable debug overlay showing recent trace events.
#[derive(Default)]
pub struct DebugOverlay {
    /// Whether the overlay is currently visible.
    pub visible: bool,
    /// Current scroll offset (0 = most recent event at bottom).
    pub scroll_offset: usize,
    /// Events accumulated since last drain.
    events: Vec<Timestamped<TraceEvent>>,
}

impl DebugOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sync events from the trace store.
    /// Called once per render frame from the TUI event loop.
    pub fn sync(&mut self, store: &TraceStore) {
        let mut new_events = store.drain_events();
        self.events.append(&mut new_events);
        // Keep only the most recent events.
        if self.events.len() > MAX_DISPLAY_EVENTS {
            let excess = self.events.len() - MAX_DISPLAY_EVENTS;
            self.events.drain(0..excess);
        }
        // Clamp scroll
        if self.scroll_offset >= self.events.len() {
            self.scroll_offset = self.events.len().saturating_sub(1);
        }
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Return the number of buffered events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns `true` if no events are buffered.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

// ── Rendering ──────────────────────────────────────────────────────────────────────

/// Draw the debug overlay on top of the chat area.
pub fn draw_debug_overlay(frame: &mut Frame, area: Rect, overlay: &DebugOverlay) {
    let popup_width = (area.width as f32 * 0.85) as u16;
    let popup_height = (area.height as f32 * 0.85) as u16;

    let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;

    let popup_rect = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear area behind popup.
    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Debug Trace — {} events ",
            overlay.events.len()
        ))
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Rgb(15, 18, 25)));

    let inner = block.inner(popup_rect);
    let available_rows = inner.height.saturating_sub(2) as usize; // 2 for header + footer

    // Build table lines.
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled(
            "  Event                 Dur     Tokens   Detail",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]));

    // Reverse iterate for display (newest at bottom).
    let total = overlay.events.len();
    let start = overlay.scroll_offset;
    let end = (start + available_rows).min(total);

    // We show the most recent N events — iterate from newest to oldest
    // but display from top to bottom (oldest first in the visible range).
    let visible_start = total.saturating_sub(end);
    let visible_end = total.saturating_sub(start);

    for idx in visible_start..visible_end {
        if let Some(ts) = overlay.events.get(idx) {
            let line = format_trace_event(ts);
            lines.push(line);
        }
    }

    // Pad remaining rows
    while lines.len() < available_rows + 1 {
        // +1 for header
        lines.push(Line::from(""));
    }

    // Footer
    let scrolled = if overlay.scroll_offset > 0 {
        format!(" ↑{} scrolled ", overlay.scroll_offset)
    } else {
        String::new()
    };
    let footer_text = format!(
        " ↑↓ scroll  Esc close  {} events{scrolled} ",
        overlay.events.len()
    );
    lines.push(Line::from(Span::styled(
        footer_text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )));

    let paragraph = Paragraph::new(ratatui::text::Text::from(lines)).block(block);
    frame.render_widget(paragraph, popup_rect);
}

/// Format a single trace event for display in the debug overlay.
fn format_trace_event(ts: &Timestamped<TraceEvent>) -> Line<'_> {
    use TraceEvent::*;
    let style = Style::default().fg(Color::Rgb(200, 200, 210));

    match &ts.inner {
        RunStarted { session_id: _, user_input, .. } => {
            let detail = if user_input.len() > 60 {
                format!("{}…", &user_input[..60])
            } else {
                user_input.clone()
            };
            Line::from(vec![
                Span::styled(" ▶ RunStarted           ", style),
                Span::styled(format!("        {detail}"), Style::default().fg(Color::DarkGray)),
            ])
        }
        RunFinished { outcome, total_duration, total_steps, total_llm_calls, total_tool_calls, cumulative_usage } => {
            let dur = format_duration(*total_duration);
            let tok = format_tokens_short(cumulative_usage.total_tokens);
            Line::from(vec![
                Span::styled(format!(" ✓ RunFinished          {dur:>6} {tok:>6}  "), Style::default().fg(Color::Green)),
                Span::styled(
                    format!("s={total_steps} llm={total_llm_calls} tools={total_tool_calls} {outcome}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        StepStarted { step } => {
            Line::from(vec![
                Span::styled(format!(" # Step {step:<61}"), Style::default().fg(Color::Cyan)),
            ])
        }
        LlmCallStarted { step, attempt, message_count } => {
            let label = if *attempt > 0 {
                format!(" → LLM #{step} retry#{attempt}    ")
            } else {
                format!(" → LLM #{step}              ")
            };
            Line::from(vec![
                Span::styled(label, Style::default().fg(Color::Yellow)),
                Span::styled(format!("        {message_count} msgs"), Style::default().fg(Color::DarkGray)),
            ])
        }
        LlmCallFinished { step: _, attempt: _, duration, usage, finish_reason } => {
            let dur = format_duration(*duration);
            let tok = format_tokens_short(usage.total_tokens);
            let reason = finish_reason.as_deref().unwrap_or("-");
            Line::from(vec![
                Span::styled(format!(" ← LLM done             {dur:>6} {tok:>6}  "), Style::default().fg(Color::Green)),
                Span::styled(reason, Style::default().fg(Color::DarkGray)),
            ])
        }
        LlmCallFailed { step: _, attempt, error, will_retry, duration: _ } => {
            let retry = if *will_retry { "will retry" } else { "terminal" };
            Line::from(vec![
                Span::styled(format!(" ✗ LLM fail (retry #{attempt})     "), Style::default().fg(Color::Red)),
                Span::styled(format!("{retry}: {error}"), Style::default().fg(Color::DarkGray)),
            ])
        }
        ToolCallStarted { tool_call_id: _, tool_name, step } => {
            Line::from(vec![
                Span::styled(format!(" ◌ {tool_name:<17} (#{step})  "), Style::default().fg(Color::Yellow)),
            ])
        }
        ToolCallFinished { tool_call_id: _, tool_name, duration, success, output_size_bytes } => {
            let dur = format_duration(*duration);
            let marker = if *success { "✓" } else { "✗" };
            let color = if *success { Color::Green } else { Color::Red };
            Line::from(vec![
                Span::styled(format!(" {marker} {tool_name:<17} {dur:>6}         "), Style::default().fg(color)),
                Span::styled(format!("{output_size_bytes} bytes"), Style::default().fg(Color::DarkGray)),
            ])
        }
        ToolCallRejected { tool_call_id: _, tool_name, reason } => {
            Line::from(vec![
                Span::styled(format!(" ⊘ {tool_name:<17} REJECTED "), Style::default().fg(Color::Red)),
                Span::styled(reason.as_str(), Style::default().fg(Color::DarkGray)),
            ])
        }
        StreamingSummary { step, content_chunks, reasoning_chunks } => {
            Line::from(vec![
                Span::styled(format!(" ~ Stream step#{step:<52}"), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("content={content_chunks} reasoning={reasoning_chunks}"), Style::default().fg(Color::DarkGray)),
            ])
        }
        SubagentFinished { description, steps, llm_calls, tool_calls, usage, duration } => {
            let dur = format_duration(*duration);
            let tok = format_tokens_short(usage.total_tokens);
            Line::from(vec![
                Span::styled(format!(" 📦 Subagent            {dur:>6} {tok:>6}  "), Style::default().fg(Color::Magenta)),
                Span::styled(format!("{description} s={steps} llm={llm_calls} tools={tool_calls}"), Style::default().fg(Color::DarkGray)),
            ])
        }
    }
}

/// Format a duration for display (e.g., "1.2s" or "345ms").
fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Format tokens for the compact column (e.g., "1.2K", "345").
fn format_tokens_short(t: u32) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}K", t as f64 / 1_000.0)
    } else if t > 0 {
        t.to_string()
    } else {
        "-".into()
    }
}
