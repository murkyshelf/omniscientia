//! Minimal markdown → Ratatui styled Lines renderer.
//!
//! Handles the most common patterns produced by LLMs:
//!   **bold**, *italic*, `inline code`, ``` code blocks ```,
//!   # headers, - / * list items, horizontal rules (---).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Convert a markdown string into a list of styled Ratatui [`Line`]s.
pub fn render_markdown(input: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_fence = String::new();

    for raw in input.lines() {
        // ── Code-block fence ────────────────────────────────────────────
        if raw.trim_start().starts_with("```") {
            if in_code_block {
                // Closing fence
                in_code_block = false;
                code_fence.clear();
                lines.push(Line::from(Span::styled(
                    "─────────────────────────────────".to_string(),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                // Opening fence – extract language hint
                in_code_block = true;
                code_fence = raw.trim_start().trim_start_matches('`').trim().to_string();
                let label = if code_fence.is_empty() { "code".to_string() } else { code_fence.clone() };
                lines.push(Line::from(Span::styled(
                    format!("▌ {} ─────────────────────────────", label),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code_block {
            lines.push(Line::from(Span::styled(
                format!("  {}", raw),
                Style::default().fg(Color::Yellow),
            )));
            continue;
        }

        // ── Horizontal rule ─────────────────────────────────────────────
        let trimmed = raw.trim();
        if trimmed == "---" || trimmed == "===" || trimmed == "***" {
            lines.push(Line::from(Span::styled(
                "─".repeat(48),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // ── Headers ─────────────────────────────────────────────────────
        if let Some(rest) = trimmed.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                format!("  {}", rest),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                format!("  {}", rest),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }

        // ── List items ──────────────────────────────────────────────────
        let (list_prefix, rest_after_prefix) = if let Some(r) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            ("  • ", r)
        } else if let Some(r) = trimmed.strip_prefix("  - ").or_else(|| trimmed.strip_prefix("  * ")) {
            ("    ‣ ", r)
        } else {
            ("", trimmed)
        };

        // ── Inline spans ────────────────────────────────────────────────
        let mut spans = Vec::new();
        if !list_prefix.is_empty() {
            spans.push(Span::styled(list_prefix.to_string(), Style::default().fg(Color::Cyan)));
        }
        spans.extend(parse_inline(rest_after_prefix));
        if spans.is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(spans));
        }
    }

    lines
}

/// Parse inline markdown spans: **bold**, *italic*, `code`, plain text.
fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let s = text.to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut buf = String::new();

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span::raw(buf.clone()));
                buf.clear();
            }
        };
    }

    while i < chars.len() {
        // ** bold **
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing(&chars, i + 2, "**") {
                flush!();
                let inner: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(inner, Style::default().add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        }
        // __ bold __
        if i + 1 < chars.len() && chars[i] == '_' && chars[i + 1] == '_' {
            if let Some(end) = find_closing(&chars, i + 2, "__") {
                flush!();
                let inner: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(inner, Style::default().add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        }
        // * italic *  or  _ italic _
        if (chars[i] == '*' || chars[i] == '_') && (i == 0 || chars[i - 1] == ' ') {
            let delim = if chars[i] == '*' { "*" } else { "_" };
            if let Some(end) = find_closing(&chars, i + 1, delim) {
                flush!();
                let inner: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(inner, Style::default().add_modifier(Modifier::ITALIC)));
                i = end + 1;
                continue;
            }
        }
        // `inline code`
        if chars[i] == '`' {
            if let Some(end) = find_closing(&chars, i + 1, "`") {
                flush!();
                let inner: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    format!(" {} ", inner),
                    Style::default().fg(Color::Black).bg(Color::Yellow),
                ));
                i = end + 1;
                continue;
            }
        }

        buf.push(chars[i]);
        i += 1;
    }
    flush!();
    spans
}

fn find_closing(chars: &[char], start: usize, delim: &str) -> Option<usize> {
    let dc: Vec<char> = delim.chars().collect();
    let dl = dc.len();
    let mut i = start;
    while i + dl <= chars.len() {
        if &chars[i..i + dl] == dc.as_slice() {
            return Some(i);
        }
        i += 1;
    }
    None
}
