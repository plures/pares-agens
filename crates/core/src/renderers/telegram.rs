//! Thin Telegram renderer for the event spine.
//!
//! Converts model markdown to Telegram HTML, chunks according to the channel
//! contract, and sends via `bot.send_message()`.  On HTML failure, retries
//! with plain text (all formatting stripped).

use crate::channel_contract::ChannelContract;

/// Convert model markdown output to Telegram-safe HTML.
///
/// Handles: `**bold**`, `*italic*`, `` `code` ``, ` ```block``` `,
/// `[text](url)`.  Everything else is HTML-escaped.
pub fn markdown_to_telegram_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 64);
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Fenced code blocks: ```...```
        if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            i += 3;
            // Skip optional language tag on same line
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            if i < len {
                i += 1; // skip newline
            }
            let start = i;
            // Find closing ```
            while i + 2 < len {
                if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
                    break;
                }
                i += 1;
            }
            let block: String = chars[start..i].iter().collect();
            out.push_str("<pre>");
            out.push_str(&html_escape(&block));
            out.push_str("</pre>");
            if i + 2 < len {
                i += 3; // skip closing ```
            }
            continue;
        }

        // Inline code: `...`
        if chars[i] == '`' {
            i += 1;
            let start = i;
            while i < len && chars[i] != '`' {
                i += 1;
            }
            let code: String = chars[start..i].iter().collect();
            out.push_str("<code>");
            out.push_str(&html_escape(&code));
            out.push_str("</code>");
            if i < len {
                i += 1;
            }
            continue;
        }

        // Bold: **...**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            i += 2;
            let start = i;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '*') {
                i += 1;
            }
            let inner: String = chars[start..i].iter().collect();
            out.push_str("<b>");
            out.push_str(&html_escape(&inner));
            out.push_str("</b>");
            if i + 1 < len {
                i += 2;
            }
            continue;
        }

        // Italic: *...*  (single asterisk, not followed by another *)
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            i += 1;
            let start = i;
            while i < len && chars[i] != '*' {
                i += 1;
            }
            let inner: String = chars[start..i].iter().collect();
            out.push_str("<i>");
            out.push_str(&html_escape(&inner));
            out.push_str("</i>");
            if i < len {
                i += 1;
            }
            continue;
        }

        // Links: [text](url)
        if chars[i] == '[' {
            let bracket_start = i;
            i += 1;
            let text_start = i;
            let mut depth = 1;
            while i < len && depth > 0 {
                if chars[i] == '[' {
                    depth += 1;
                } else if chars[i] == ']' {
                    depth -= 1;
                }
                if depth > 0 {
                    i += 1;
                }
            }
            if i < len && chars[i] == ']' && i + 1 < len && chars[i + 1] == '(' {
                let text: String = chars[text_start..i].iter().collect();
                i += 2; // skip ](
                let url_start = i;
                while i < len && chars[i] != ')' {
                    i += 1;
                }
                let url: String = chars[url_start..i].iter().collect();
                out.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    html_escape(&url),
                    html_escape(&text)
                ));
                if i < len {
                    i += 1; // skip )
                }
                continue;
            } else {
                // Not a valid link, output the [ literally
                i = bracket_start;
                out.push_str(&html_escape_char(chars[i]));
                i += 1;
                continue;
            }
        }

        // Default: HTML-escape and pass through
        out.push_str(&html_escape_char(chars[i]));
        i += 1;
    }

    out
}

/// Strip all markup, returning plain text.
pub fn strip_formatting(input: &str) -> String {
    // Remove HTML tags and common markdown symbols
    let mut s = input.to_string();
    // Remove code blocks
    while let Some(start) = s.find("```") {
        if let Some(end) = s[start + 3..].find("```") {
            let block = &s[start + 3..start + 3 + end];
            // strip optional language tag
            let block = block.strip_prefix(|c: char| c != '\n').map_or(block, |rest| {
                rest.strip_prefix('\n').unwrap_or(rest)
            });
            s = format!("{}{}{}", &s[..start], block, &s[start + 6 + end..]);
        } else {
            break;
        }
    }
    // Simple tag stripping for any remaining HTML
    let re_result = strip_html_tags(&s);
    re_result
        .replace("**", "")
        .replace(['*', '`'], "")
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' && in_tag {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Chunk a message into pieces no longer than `max_len` characters.
///
/// Tries to split at newlines; falls back to hard split if no newline found.
pub fn chunk_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }
        // Try to find a newline to break at
        let split_at = remaining[..max_len]
            .rfind('\n')
            .map(|pos| pos + 1) // include the newline
            .unwrap_or(max_len);
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    chunks
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn html_escape_char(c: char) -> String {
    match c {
        '&' => "&amp;".to_string(),
        '<' => "&lt;".to_string(),
        '>' => "&gt;".to_string(),
        _ => c.to_string(),
    }
}

/// Render model output for Telegram delivery.
///
/// Returns `(html_chunks, plain_chunks)`. The caller should attempt to send
/// with HTML parse mode first; if that fails, fall back to plain chunks.
pub fn render_for_telegram(
    content: &str,
    contract: &ChannelContract,
) -> (Vec<String>, Vec<String>) {
    let html = markdown_to_telegram_html(content);
    let plain = strip_formatting(content);
    let html_chunks = chunk_message(&html, contract.max_message_len);
    let plain_chunks = chunk_message(&plain, contract.max_message_len);
    (html_chunks, plain_chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_conversion() {
        assert_eq!(
            markdown_to_telegram_html("**hello**"),
            "<b>hello</b>"
        );
    }

    #[test]
    fn italic_conversion() {
        assert_eq!(
            markdown_to_telegram_html("*world*"),
            "<i>world</i>"
        );
    }

    #[test]
    fn inline_code() {
        assert_eq!(
            markdown_to_telegram_html("`code`"),
            "<code>code</code>"
        );
    }

    #[test]
    fn code_block() {
        let input = "```rust\nfn main() {}\n```";
        let result = markdown_to_telegram_html(input);
        assert_eq!(result, "<pre>fn main() {}\n</pre>");
    }

    #[test]
    fn link_conversion() {
        assert_eq!(
            markdown_to_telegram_html("[click](https://example.com)"),
            "<a href=\"https://example.com\">click</a>"
        );
    }

    #[test]
    fn html_entities_escaped() {
        assert_eq!(
            markdown_to_telegram_html("a < b & c > d"),
            "a &lt; b &amp; c &gt; d"
        );
    }

    #[test]
    fn chunking_splits_at_newline() {
        let text = "line1\nline2\nline3";
        let chunks = chunk_message(text, 12);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "line1\nline2\n");
        assert_eq!(chunks[1], "line3");
    }

    #[test]
    fn render_for_telegram_returns_both_formats() {
        let contract = ChannelContract::telegram();
        let (html, plain) = render_for_telegram("**bold** text", &contract);
        assert_eq!(html.len(), 1);
        assert!(html[0].contains("<b>bold</b>"));
        assert_eq!(plain.len(), 1);
        assert!(!plain[0].contains("<b>"));
    }
}
