//! Markdown YAML frontmatter splitting, shared by the `skills` and `subagent`
//! extensions.
//!
//! Both extensions load definitions from Markdown files whose first block is
//! a YAML frontmatter delimited by `---` lines.  The delimiter-splitting
//! logic lives here so the two parsers cannot drift apart.

/// Error returned when a Markdown frontmatter block cannot be split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MdFrontmatterError {
    /// Missing the opening `---` delimiter.
    MissingOpeningDelimiter,
    /// Missing the closing `---` delimiter.
    MissingClosingDelimiter,
}

impl std::fmt::Display for MdFrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOpeningDelimiter => write!(f, "missing opening '---' delimiter"),
            Self::MissingClosingDelimiter => write!(f, "missing closing '---' delimiter"),
        }
    }
}

/// Split `raw` into `(frontmatter_yaml, body)`.
///
/// Semantics are shared verbatim with the skills parser:
///
/// - leading whitespace (incl. blank lines) is skipped;
/// - the file must start with `---\n` or `---\r\n`;
/// - an immediate `---\n` after the opening delimiter means empty
///   frontmatter — returns `Ok(("", body))`, and the caller's YAML parse
///   then fails with its own error;
/// - otherwise the closing delimiter is the first `\n---\n` or `\r\n---\r\n`;
/// - the returned `body` may carry a leading `\r\n` (CRLF files) — callers
///   trim before use.
pub(crate) fn split_frontmatter(raw: &str) -> Result<(&str, &str), MdFrontmatterError> {
    let trimmed = raw.trim_start();

    // Must start with "---\n" or "---\r\n"
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or(MdFrontmatterError::MissingOpeningDelimiter)?;

    // Find closing "---\n" or "---\r\n"
    if let Some(rest) = after_open.strip_prefix("---\n") {
        Ok(("", rest))
    } else {
        let close_pos = after_open
            .find("\n---\n")
            .or_else(|| after_open.find("\r\n---\r\n"))
            .ok_or(MdFrontmatterError::MissingClosingDelimiter)?;

        // Split: the YAML block does not include the "\n---\n"
        let yaml = &after_open[..close_pos];
        let body_offset = close_pos + "\n---\n".len();
        Ok((yaml, &after_open[body_offset..]))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lf_delimiters() {
        let raw = "\
---
name: x
---
body";
        let (yaml, body) = split_frontmatter(raw).unwrap();
        assert_eq!(yaml, "name: x");
        assert_eq!(body, "body");
    }

    #[test]
    fn split_crlf_delimiters() {
        let raw = "---\r\nname: x\r\n---\r\n\r\nbody";
        let (yaml, body) = split_frontmatter(raw).unwrap();
        assert_eq!(yaml, "name: x");
        // CRLF body keeps a leading "\r\n" — callers trim.
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn split_skips_leading_whitespace() {
        let raw = "\n\n   \n---\nname: x\n---\nbody";
        let (yaml, body) = split_frontmatter(raw).unwrap();
        assert_eq!(yaml, "name: x");
        assert_eq!(body, "body");
    }

    #[test]
    fn split_empty_frontmatter_returns_empty_yaml() {
        let (yaml, body) = split_frontmatter("---\n---\nbody").unwrap();
        assert_eq!(yaml, "");
        assert_eq!(body, "body");
    }

    #[test]
    fn split_missing_opening() {
        let err = split_frontmatter("name: x\n---\nbody").unwrap_err();
        assert!(matches!(err, MdFrontmatterError::MissingOpeningDelimiter));
    }

    #[test]
    fn split_missing_closing() {
        let err = split_frontmatter("---\nname: x\nbody").unwrap_err();
        assert!(matches!(err, MdFrontmatterError::MissingClosingDelimiter));

        // "---\n" alone also lacks a closing delimiter.
        let err = split_frontmatter("---\n").unwrap_err();
        assert!(matches!(err, MdFrontmatterError::MissingClosingDelimiter));
    }

    #[test]
    fn split_keeps_delimiter_like_line_in_body() {
        let raw = "---\nname: x\n---\nline one\n---\nline two";
        let (yaml, body) = split_frontmatter(raw).unwrap();
        assert_eq!(yaml, "name: x");
        // The second "---" belongs to the body.
        assert_eq!(body, "line one\n---\nline two");
    }
}
