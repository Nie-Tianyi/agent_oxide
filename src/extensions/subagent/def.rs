//! Subagent definitions — parsed from Markdown files with YAML frontmatter.
//!
//! A **subagent definition** is a Markdown file (Claude Code `agents/*.md`
//! style) whose frontmatter declares routing metadata (name, description,
//! model, tool allowlist, limits) and whose body is the subagent's system
//! prompt:
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Review code for correctness bugs.
//! model: deepseek-v4-flash
//! tools: [read, grep, glob]
//! timeout_secs: 90
//! ---
//!
//! You are a code reviewer. Focus on correctness...
//! ```
//!
//! [`SubagentTool`](crate::subagent::SubagentTool::new) consumes a
//! definition directly: unset fields fall back to the internal config
//! defaults, and `model` falls back to the parent's model.

use serde::Deserialize;

use crate::util::md::{MdFrontmatterError, split_frontmatter};

use super::config::SubagentConfig;

/// A subagent definition parsed from a Markdown file (`agents/*.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentDef {
    /// Definition name — becomes the tool name the parent LLM calls.
    /// Restricted to `[A-Za-z0-9_-]` because it flows into tool names.
    pub name: String,
    /// Short routing signal — becomes the tool description the parent
    /// LLM sees when deciding which subagent to delegate to.
    pub description: String,
    /// LLM model for this subagent. `None` falls back to the parent's
    /// model when the definition is resolved.
    pub model: Option<String>,
    /// Names of parent tools the subagent may use.
    /// Empty = a reasoning-only subagent with no tools.
    pub tools: Vec<String>,
    /// Maximum ReAct loop iterations; `None` = config default.
    pub max_steps: Option<usize>,
    /// Maximum retries for transient LLM failures; `None` = config default.
    pub max_retries: Option<usize>,
    /// Whether to enable SSE streaming; `None` = config default.
    pub streaming: Option<bool>,
    /// Wall-clock timeout in seconds.
    ///
    /// - `Some(0)` — explicitly **no configured timeout** (the tool's
    ///   built-in 300 s safety fallback still applies).
    /// - `Some(n)` — hard timeout of `n` seconds.
    /// - `None` — not specified; defaults to the config default (`Some(120)`).
    pub timeout_secs: Option<u64>,
    /// Number of parent messages inherited as context; `None` = config default.
    pub inherit_context_messages: Option<usize>,
    /// The Markdown body — used as the subagent's system prompt.
    pub system_prompt: String,
}

impl SubagentDef {
    /// Project this definition onto the resolved run configuration.
    ///
    /// Unset fields take the internal config defaults; `model` falls back
    /// to `default_model` (pass the parent's model string — the same one
    /// given to `Agent::builder`).
    pub(crate) fn into_config(self, default_model: &str) -> SubagentConfig {
        let d = SubagentConfig::default();
        SubagentConfig {
            model: self.model.unwrap_or_else(|| default_model.to_owned()),
            system_prompt: self.system_prompt,
            max_steps: self.max_steps.unwrap_or(d.max_steps),
            max_retries: self.max_retries.unwrap_or(d.max_retries),
            streaming: self.streaming.unwrap_or(d.streaming),
            timeout_secs: match self.timeout_secs {
                Some(0) => None,
                Some(n) => Some(n),
                None => d.timeout_secs,
            },
            inherit_context_messages: self.inherit_context_messages,
        }
    }
}

/// Error returned when a subagent definition file cannot be parsed.
#[derive(Debug)]
pub enum SubagentError {
    /// Missing the opening `---` frontmatter delimiter.
    MissingOpeningDelimiter,
    /// Missing the closing `---` frontmatter delimiter.
    MissingClosingDelimiter,
    /// Invalid YAML, or a required field missing/invalid.
    InvalidFrontmatter(String),
    /// `name` is empty or contains characters outside `[A-Za-z0-9_-]`.
    InvalidToolName(String),
    /// The system-prompt body is empty.
    EmptyBody,
}

impl std::fmt::Display for SubagentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOpeningDelimiter => write!(f, "missing opening '---' delimiter"),
            Self::MissingClosingDelimiter => write!(f, "missing closing '---' delimiter"),
            Self::InvalidFrontmatter(msg) => write!(f, "invalid frontmatter: {msg}"),
            Self::InvalidToolName(name) => {
                write!(f, "invalid subagent name '{name}' (use [A-Za-z0-9_-])")
            }
            Self::EmptyBody => write!(f, "subagent body (system prompt) is empty"),
        }
    }
}

impl std::error::Error for SubagentError {}

/// YAML frontmatter of a subagent definition file.
///
/// Unknown fields are silently ignored (same leniency as the skills
/// parser) so files written against future versions keep loading.
#[derive(Debug, Deserialize)]
struct SubagentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    max_steps: Option<usize>,
    #[serde(default)]
    max_retries: Option<usize>,
    #[serde(default)]
    streaming: Option<bool>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    inherit_context_messages: Option<usize>,
}

/// Parse a subagent definition file into a [`SubagentDef`].
///
/// Expected format:
/// ```markdown
/// ---
/// name: code-reviewer
/// description: Review code for bugs.
/// ---
///
/// You are a code reviewer...
/// ```
pub(crate) fn parse_subagent_file(raw: &str) -> Result<SubagentDef, SubagentError> {
    let (yaml_block, body) = split_frontmatter(raw).map_err(|e| match e {
        MdFrontmatterError::MissingOpeningDelimiter => SubagentError::MissingOpeningDelimiter,
        MdFrontmatterError::MissingClosingDelimiter => SubagentError::MissingClosingDelimiter,
    })?;

    let fm: SubagentFrontmatter = serde_yaml::from_str(yaml_block)
        .map_err(|e| SubagentError::InvalidFrontmatter(e.to_string()))?;

    let name = fm.name.trim().to_string();
    let is_valid_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !is_valid_name {
        return Err(SubagentError::InvalidToolName(name));
    }

    let description = fm.description.trim().to_string();
    if description.is_empty() {
        return Err(SubagentError::InvalidFrontmatter(
            "description must not be empty".into(),
        ));
    }

    let system_prompt = body.trim().to_string();
    if system_prompt.is_empty() {
        return Err(SubagentError::EmptyBody);
    }

    Ok(SubagentDef {
        name,
        description,
        model: fm.model,
        tools: fm.tools.unwrap_or_default(),
        max_steps: fm.max_steps,
        max_retries: fm.max_retries,
        streaming: fm.streaming,
        timeout_secs: fm.timeout_secs,
        inherit_context_messages: fm.inherit_context_messages,
        system_prompt,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "\
---
name: code-reviewer
description: Review code for bugs.
---

You are a code reviewer.
";

    // ── Parsing ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_valid_definition() {
        let def = parse_subagent_file(MINIMAL).unwrap();
        assert_eq!(def.name, "code-reviewer");
        assert_eq!(def.description, "Review code for bugs.");
        assert_eq!(def.system_prompt, "You are a code reviewer.");
        assert!(def.model.is_none());
        assert!(def.tools.is_empty());
        assert!(def.max_steps.is_none());
        assert!(def.max_retries.is_none());
        assert!(def.streaming.is_none());
        assert!(def.timeout_secs.is_none());
        assert!(def.inherit_context_messages.is_none());
    }

    #[test]
    fn parse_definition_with_crlf() {
        let raw = "---\r\nname: crlf-agent\r\ndescription: A CRLF def.\r\n---\r\n\r\nBody here.";
        let def = parse_subagent_file(raw).unwrap();
        assert_eq!(def.name, "crlf-agent");
        assert_eq!(def.system_prompt, "Body here.");
    }

    #[test]
    fn parse_definition_all_fields() {
        let raw = "\
---
name: full
description: All fields.
model: my-model
tools: [read, grep, glob]
max_steps: 40
max_retries: 5
streaming: false
timeout_secs: 30
inherit_context_messages: 3
---

Body.
";
        let def = parse_subagent_file(raw).unwrap();
        assert_eq!(def.model.as_deref(), Some("my-model"));
        assert_eq!(def.tools, vec!["read", "grep", "glob"]);
        assert_eq!(def.max_steps, Some(40));
        assert_eq!(def.max_retries, Some(5));
        assert_eq!(def.streaming, Some(false));
        assert_eq!(def.timeout_secs, Some(30));
        assert_eq!(def.inherit_context_messages, Some(3));
    }

    #[test]
    fn parse_definition_extra_fields_ignored() {
        let raw = "\
---
name: extra
description: Has extra fields.
version: \"1.0\"
author: test
---

Body.
";
        let def = parse_subagent_file(raw).unwrap();
        assert_eq!(def.name, "extra");
        assert_eq!(def.system_prompt, "Body.");
    }

    #[test]
    fn parse_missing_name() {
        let raw = "---\ndescription: No name.\n---\nBody.";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::InvalidFrontmatter(_)));
    }

    #[test]
    fn parse_missing_description() {
        let raw = "---\nname: no-desc\n---\nBody.";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::InvalidFrontmatter(_)));
    }

    #[test]
    fn parse_empty_description() {
        let raw = "---\nname: x\ndescription: \"\"\n---\nBody.";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::InvalidFrontmatter(_)));
    }

    #[test]
    fn parse_empty_body() {
        let raw = "---\nname: x\ndescription: Desc.\n---\n\n   \n";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::EmptyBody));
    }

    #[test]
    fn parse_missing_opening_delimiter() {
        let raw = "name: x\ndescription: Desc.\n---\nBody.";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::MissingOpeningDelimiter));
    }

    #[test]
    fn parse_missing_closing_delimiter() {
        let raw = "---\nname: x\ndescription: Desc.\nBody without closing.";
        let err = parse_subagent_file(raw).unwrap_err();
        assert!(matches!(err, SubagentError::MissingClosingDelimiter));
    }

    #[test]
    fn parse_rejects_invalid_tool_name() {
        // Quoted YAML scalars so the value is parsed exactly (an unquoted
        // `code\nreview` would fold/parse as separate keys instead).
        for bad in ["code review", "a/b", ""] {
            let raw = format!("---\nname: \"{bad}\"\ndescription: Desc.\n---\nBody.");
            let err = parse_subagent_file(&raw).unwrap_err();
            assert!(matches!(err, SubagentError::InvalidToolName(_)), "{bad:?}");
        }
    }

    #[test]
    fn parse_accepts_slug_tool_name() {
        let raw = "---\nname: code-review_v2\ndescription: Desc.\n---\nBody.";
        let def = parse_subagent_file(raw).unwrap();
        assert_eq!(def.name, "code-review_v2");
    }

    #[test]
    fn parse_tools_omitted_defaults_empty() {
        let def = parse_subagent_file(MINIMAL).unwrap();
        assert!(def.tools.is_empty());
    }

    #[test]
    fn parse_timeout_zero_preserved_as_some_zero() {
        let raw = "---\nname: x\ndescription: Desc.\ntimeout_secs: 0\n---\nBody.";
        let def = parse_subagent_file(raw).unwrap();
        // Conversion happens in into_config, not at parse time.
        assert_eq!(def.timeout_secs, Some(0));
    }

    // ── into_config ──────────────────────────────────────────────────────────

    #[test]
    fn into_config_fills_defaults_and_model_fallback() {
        let def = parse_subagent_file(MINIMAL).unwrap();
        let cfg = def.into_config("parent-model");
        assert_eq!(cfg.model, "parent-model");
        assert_eq!(cfg.system_prompt, "You are a code reviewer.");
        assert_eq!(cfg.max_steps, 25);
        assert_eq!(cfg.max_retries, 2);
        assert!(cfg.streaming);
        assert_eq!(cfg.timeout_secs, Some(120));
        assert_eq!(cfg.inherit_context_messages, None);
    }

    #[test]
    fn into_config_explicit_model_wins() {
        let raw = "---\nname: x\ndescription: Desc.\nmodel: my-model\n---\nBody.";
        let def = parse_subagent_file(raw).unwrap();
        let cfg = def.into_config("parent-model");
        assert_eq!(cfg.model, "my-model");
    }

    #[test]
    fn into_config_timeout_zero_maps_to_none() {
        let raw = "---\nname: x\ndescription: Desc.\ntimeout_secs: 0\n---\nBody.";
        let def = parse_subagent_file(raw).unwrap();
        let cfg = def.into_config("parent-model");
        assert_eq!(cfg.timeout_secs, None);
    }

    #[test]
    fn into_config_explicit_timeout_used() {
        let raw = "---\nname: x\ndescription: Desc.\ntimeout_secs: 30\n---\nBody.";
        let def = parse_subagent_file(raw).unwrap();
        let cfg = def.into_config("parent-model");
        assert_eq!(cfg.timeout_secs, Some(30));
    }

    #[test]
    fn into_config_explicit_options_override_defaults() {
        let raw = "\
---
name: x
description: Desc.
max_steps: 7
max_retries: 9
streaming: false
inherit_context_messages: 2
---
Body.
";
        let def = parse_subagent_file(raw).unwrap();
        let cfg = def.into_config("parent-model");
        assert_eq!(cfg.max_steps, 7);
        assert_eq!(cfg.max_retries, 9);
        assert!(!cfg.streaming);
        assert_eq!(cfg.inherit_context_messages, Some(2));
    }
}
