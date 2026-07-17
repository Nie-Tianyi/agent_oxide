//! Skill definitions, discovery, and registry for Agent Oxide.
//!
//! A **skill** is a Markdown file with YAML frontmatter that provides
//! specialized instructions for an LLM agent. This crate handles:
//!
//! - Parsing skill files (YAML frontmatter + Markdown body)
//! - Discovering skills from one or more directories on disk
//! - A read-only [`SkillRegistry`] for fast name-based lookup
//! - An [`ActiveSkills`] type alias for tracking which skills are active
//!
//! # Example
//!
//! ```no_run
//! use skills::SkillRegistry;
//! use std::path::PathBuf;
//!
//! let paths = vec![PathBuf::from("./skills")];
//! let registry = SkillRegistry::discover(&paths);
//! if let Some(skill) = registry.by_name("my-skill") {
//!     println!("Loaded: {} — {}", skill.name, skill.description);
//! }
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::Deserialize;

// ── SkillDef ─────────────────────────────────────────────────────────────────────

/// A loaded skill definition — parsed from a skill Markdown file.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDef {
    /// Unique name — used as the lookup key and tool argument.
    pub name: String,
    /// Short human-readable description shown in the skill list.
    pub description: String,
    /// The Markdown body (everything after the frontmatter).
    /// This content is injected as a System message when the skill is activated.
    pub content: String,
}

// ── ActiveSkills ─────────────────────────────────────────────────────────────────

/// Thread-safe set of currently active skills.
///
/// Maps skill name → skill content. Shared between the skill-loading tool
/// (writer) and the hook that injects System messages (reader).
pub type ActiveSkills = Arc<RwLock<HashMap<String, String>>>;

// ── SkillRegistry ────────────────────────────────────────────────────────────────

/// A read-only registry of discovered skills.
///
/// Created at startup via [`SkillRegistry::discover`] and never mutated
/// afterwards. Lookups are O(n) linear search over a `Vec` — acceptable
/// because skill counts are expected to stay in the single- or low-double-digit
/// range.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: Vec<SkillDef>,
}

impl SkillRegistry {
    /// Create an empty registry (no skills).
    pub fn empty() -> Self {
        Self { skills: Vec::new() }
    }

    /// Discover skills by scanning `*.md` files in the given search paths.
    ///
    /// Directories are scanned in order; later paths **override** earlier ones
    /// when a skill with the same `name` is found. This lets callers pass
    /// project-dir first, user-dir second, and have project skills win.
    ///
    /// Missing directories are silently skipped. Files that fail to parse
    /// are skipped with a `tracing::warn!` log.
    pub fn discover(search_paths: &[PathBuf]) -> Self {
        use std::collections::HashMap;

        let mut by_name: HashMap<String, SkillDef> = HashMap::new();

        for dir in search_paths {
            let pattern = dir.join("*.md");
            let pattern_str = pattern.display().to_string();

            let paths = match glob::glob(&pattern_str) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        "Invalid glob pattern for skill discovery"
                    );
                    continue;
                }
            };

            for entry in paths {
                let path = match entry {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "Glob walk error during skill discovery");
                        continue;
                    }
                };

                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Cannot read skill file"
                        );
                        continue;
                    }
                };

                match parse_skill_file(&content) {
                    Ok(skill) => {
                        tracing::info!(
                            name = %skill.name,
                            path = %path.display(),
                            "Discovered skill"
                        );
                        // Later entries overwrite earlier ones (project over user).
                        by_name.insert(skill.name.clone(), skill);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to parse skill file"
                        );
                    }
                }
            }
        }

        let mut skills: Vec<SkillDef> = by_name.into_values().collect();
        // Sort by name for deterministic ordering in the system prompt list.
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Self { skills }
    }

    /// Look up a skill by name.
    pub fn by_name(&self, name: &str) -> Option<&SkillDef> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Return all discovered skills.
    pub fn list(&self) -> &[SkillDef] {
        &self.skills
    }

    /// Return just the skill names (for error messages, tab-completion, etc.).
    pub fn names(&self) -> Vec<&str> {
        self.skills.iter().map(|s| s.name.as_str()).collect()
    }

    /// Whether no skills were discovered.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

// ── Frontmatter Parser ───────────────────────────────────────────────────────────

/// YAML frontmatter block inside a skill Markdown file.
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

/// Error returned when a skill file cannot be parsed.
#[derive(Debug)]
pub enum SkillError {
    /// Missing the opening `---` frontmatter delimiter.
    MissingOpeningDelimiter,
    /// Missing the closing `---` frontmatter delimiter.
    MissingClosingDelimiter,
    /// Required field missing or invalid YAML.
    InvalidFrontmatter(String),
    /// Skill body (content after frontmatter) is empty.
    EmptyContent,
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOpeningDelimiter => write!(f, "missing opening '---' delimiter"),
            Self::MissingClosingDelimiter => write!(f, "missing closing '---' delimiter"),
            Self::InvalidFrontmatter(msg) => write!(f, "invalid frontmatter: {msg}"),
            Self::EmptyContent => write!(f, "skill body is empty"),
        }
    }
}

/// Parse a skill Markdown file into a [`SkillDef`].
///
/// Expected format:
/// ```markdown
/// ---
/// name: my-skill
/// description: A short description.
/// ---
///
/// Skill instructions here...
/// ```
fn parse_skill_file(raw: &str) -> Result<SkillDef, SkillError> {
    let trimmed = raw.trim_start();

    // Must start with "---\n" or "---\r\n"
    let after_open = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
        .ok_or(SkillError::MissingOpeningDelimiter)?;

    // Find closing "---\n" or "---\r\n"
    let (yaml_block, body) = if let Some(rest) = after_open.strip_prefix("---\n") {
        ("", rest)
    } else {
        let close_pos = after_open
            .find("\n---\n")
            .or_else(|| after_open.find("\r\n---\r\n"))
            .ok_or(SkillError::MissingClosingDelimiter)?;

        // Split: the YAML block does not include the "\n---\n"
        let yaml = &after_open[..close_pos];
        let body_offset = close_pos + "\n---\n".len();
        (yaml, &after_open[body_offset..])
    };

    let fm: SkillFrontmatter = serde_yaml::from_str(yaml_block)
        .map_err(|e| SkillError::InvalidFrontmatter(e.to_string()))?;

    let content = body.trim().to_string();
    if content.is_empty() {
        return Err(SkillError::EmptyContent);
    }

    Ok(SkillDef {
        name: fm.name,
        description: fm.description,
        content,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parser tests ──────────────────────────────────────────────────────────

    #[test]
    fn test_parse_valid_skill() {
        let raw = "\
---
name: my-skill
description: Does something useful.
---

These are the skill instructions.
";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "Does something useful.");
        assert_eq!(skill.content, "These are the skill instructions.");
    }

    #[test]
    fn test_parse_skill_with_crlf() {
        let raw =
            "---\r\nname: my-skill\r\ndescription: A skill.\r\n---\r\n\r\nInstructions here.\r\n";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "A skill.");
        assert_eq!(skill.content, "Instructions here.");
    }

    #[test]
    fn test_parse_skill_with_extra_whitespace() {
        let raw = "\n\n   \n---\nname: my-skill\ndescription: Desc.\n---\nBody content.";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.content, "Body content.");
    }

    #[test]
    fn test_parse_skill_with_markdown_body() {
        let raw = "\
---
name: code-review
description: Review code changes.
---

# Code Review Skill

- Check for bugs
- Check for style
";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.name, "code-review");
        assert!(skill.content.contains("# Code Review Skill"));
        assert!(skill.content.contains("- Check for bugs"));
    }

    #[test]
    fn test_parse_missing_opening_delimiter() {
        let raw = "name: my-skill\ndescription: Desc.\n---\nBody.";
        let err = parse_skill_file(raw).unwrap_err();
        assert!(matches!(err, SkillError::MissingOpeningDelimiter));
    }

    #[test]
    fn test_parse_missing_closing_delimiter() {
        let raw = "---\nname: my-skill\ndescription: Desc.\nBody without closing.";
        let err = parse_skill_file(raw).unwrap_err();
        assert!(matches!(err, SkillError::MissingClosingDelimiter));
    }

    #[test]
    fn test_parse_missing_name_field() {
        let raw = "---\ndescription: Forgot the name field.\n---\nBody.";
        let err = parse_skill_file(raw).unwrap_err();
        assert!(matches!(err, SkillError::InvalidFrontmatter(_)));
    }

    #[test]
    fn test_parse_missing_description_field() {
        let raw = "---\nname: my-skill\n---\nBody.";
        let err = parse_skill_file(raw).unwrap_err();
        assert!(matches!(err, SkillError::InvalidFrontmatter(_)));
    }

    #[test]
    fn test_parse_empty_body() {
        let raw = "---\nname: my-skill\ndescription: Desc.\n---\n\n   \n";
        let err = parse_skill_file(raw).unwrap_err();
        assert!(matches!(err, SkillError::EmptyContent));
    }

    #[test]
    fn test_parse_trims_body_whitespace() {
        let raw = "---\nname: my-skill\ndescription: Desc.\n---\n\n  Body with surrounding whitespace.  \n\n";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.content, "Body with surrounding whitespace.");
    }

    #[test]
    fn test_parse_yaml_block_with_extra_fields() {
        let raw =
            "---\nname: my-skill\ndescription: Desc.\nversion: \"1.0\"\nauthor: test\n---\nBody.";
        let skill = parse_skill_file(raw).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "Desc.");
        // Extra fields are silently ignored (serde default behavior).
    }

    // ── Registry tests ────────────────────────────────────────────────────────

    #[test]
    fn test_registry_empty() {
        let reg = SkillRegistry::empty();
        assert!(reg.is_empty());
        assert!(reg.list().is_empty());
        assert!(reg.names().is_empty());
        assert!(reg.by_name("anything").is_none());
    }

    #[test]
    fn test_registry_discover_from_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_path = tmp.path().join("myskill.md");
        std::fs::write(
            &skill_path,
            "---\nname: myskill\ndescription: A test skill.\n---\nTest content.",
        )
        .unwrap();

        let paths = vec![tmp.path().to_path_buf()];
        let reg = SkillRegistry::discover(&paths);
        assert!(!reg.is_empty());
        assert_eq!(reg.list().len(), 1);

        let skill = reg.by_name("myskill").unwrap();
        assert_eq!(skill.name, "myskill");
        assert_eq!(skill.description, "A test skill.");
        assert_eq!(skill.content, "Test content.");
        assert_eq!(reg.names(), vec!["myskill"]);
    }

    #[test]
    fn test_registry_discover_missing_directory_ok() {
        let paths = vec![PathBuf::from("/nonexistent/dir/for/skills")];
        let reg = SkillRegistry::discover(&paths);
        assert!(reg.is_empty());
    }

    #[test]
    fn test_registry_discover_invalid_file_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a valid file and an invalid file.
        std::fs::write(
            tmp.path().join("valid.md"),
            "---\nname: valid\ndescription: OK.\n---\nContent.",
        )
        .unwrap();
        std::fs::write(tmp.path().join("invalid.md"), "No frontmatter here.").unwrap();

        let paths = vec![tmp.path().to_path_buf()];
        let reg = SkillRegistry::discover(&paths);
        assert_eq!(reg.list().len(), 1);
        assert!(reg.by_name("valid").is_some());
        assert!(reg.by_name("invalid").is_none());
    }

    #[test]
    fn test_registry_deduplication_later_wins() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        std::fs::write(
            dir1.path().join("s.md"),
            "---\nname: s\ndescription: From dir1 (project).\n---\nContent 1.",
        )
        .unwrap();
        std::fs::write(
            dir2.path().join("s.md"),
            "---\nname: s\ndescription: From dir2 (user) — should win.\n---\nContent 2.",
        )
        .unwrap();

        // dir2 is later, so it should win.
        let paths = vec![dir1.path().to_path_buf(), dir2.path().to_path_buf()];
        let reg = SkillRegistry::discover(&paths);
        assert_eq!(reg.list().len(), 1);
        let skill = reg.by_name("s").unwrap();
        assert_eq!(skill.description, "From dir2 (user) — should win.");
        assert_eq!(skill.content, "Content 2.");
    }

    #[test]
    fn test_registry_skills_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["zebra", "alpha", "mike"] {
            std::fs::write(
                tmp.path().join(format!("{name}.md")),
                format!("---\nname: {name}\ndescription: Skill {name}.\n---\nContent for {name}."),
            )
            .unwrap();
        }

        let paths = vec![tmp.path().to_path_buf()];
        let reg = SkillRegistry::discover(&paths);
        let names: Vec<&str> = reg.list().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zebra"]);
    }
}
