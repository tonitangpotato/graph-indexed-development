//! Skill loader — reads a skill's prompt and metadata from disk.
//!
//! Resolves a skill by name in the following priority:
//! 1. `<project_root>/.gid/skills/<name>.md` — project-local override.
//! 2. `$HOME/rustclaw/skills/<name>/SKILL.md` — RustClaw bundled skills.
//! 3. Built-in prompts compiled into the binary (`prompts/*.txt`) — fallback
//!    so that a fresh checkout can still run core phases without an
//!    external skills directory.
//!
//! ISS-052 T06: parses optional YAML frontmatter to extract a
//! `file_policy: required | optional | forbidden` declaration. The policy
//! drives the `run_skill` post-condition gate (§5.4). Skills that omit the
//! field default to `Optional` (claim-driven, no file gate). External
//! callers should never hardcode a name → policy match — the policy lives
//! with the skill's contract on disk.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Whether a skill is expected to mutate the workspace filesystem.
///
/// Drives the post-condition gate in `V2Executor::run_skill`. The variant
/// names map 1:1 to the values accepted in `SKILL.md` frontmatter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillFilePolicy {
    /// Skill must produce at least one file change. Empty diff after the
    /// LLM call → `SkillFailed { reason: ZeroFileChanges }` (ISS-038 gate).
    Required,
    /// Skill may or may not change files; either outcome is success.
    /// This is the default when the field is absent — the conservative
    /// choice for backwards compatibility with skills that predate this
    /// field.
    #[default]
    Optional,
    /// Skill must NOT change files. Any non-empty diff →
    /// `SkillFailed { reason: UnexpectedFileChanges }`. Used for review /
    /// inspection skills (`review-design`, `triage`, …) that should never
    /// edit source.
    Forbidden,
}

/// Subset of `SKILL.md` frontmatter that the ritual engine cares about.
///
/// Other fields (name, description, triggers, …) are skill-author
/// metadata used by the skill *trigger* matcher, not the ritual engine,
/// and `#[serde(default)]` lets us ignore them silently.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    /// File policy declaration. Missing → `SkillFilePolicy::Optional`.
    #[serde(default)]
    file_policy: Option<SkillFilePolicy>,
}

/// A loaded skill: prompt text + resolved metadata.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    /// Full skill prompt body (frontmatter stripped if it was present).
    pub prompt: String,
    /// Resolved file-policy declaration. Defaults to
    /// `SkillFilePolicy::Optional` when frontmatter is absent or omits
    /// the `file_policy` field.
    pub file_policy: SkillFilePolicy,
}

impl LoadedSkill {
    /// Construct a `LoadedSkill` with explicit fields. Used by tests and
    /// by the built-in fallback path which has no frontmatter to parse.
    pub(crate) fn new(prompt: impl Into<String>, file_policy: SkillFilePolicy) -> Self {
        Self {
            prompt: prompt.into(),
            file_policy,
        }
    }
}

/// Load a skill by name from disk, falling back to compiled-in prompts.
///
/// `project_root` is the ritual's workspace; the project-local override
/// path is computed relative to it. Returns an error only if the name is
/// unknown across all resolution layers — IO errors on a layer that
/// exists are propagated.
pub fn load_skill(skill_name: &str, project_root: &Path) -> Result<LoadedSkill> {
    // Layer 1: project-local override (.gid/skills/<name>.md)
    let gid_skill = project_root
        .join(".gid")
        .join("skills")
        .join(format!("{}.md", skill_name));

    if gid_skill.exists() {
        let content = std::fs::read_to_string(&gid_skill).with_context(|| {
            format!(
                "reading project-local skill at {}",
                gid_skill.display()
            )
        })?;
        return Ok(parse_skill_content(&content));
    }

    // Layer 2: home-relative bundled skills (RustClaw, …).
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let rustclaw_skill = home
            .join("rustclaw")
            .join("skills")
            .join(skill_name)
            .join("SKILL.md");

        if rustclaw_skill.exists() {
            let content = std::fs::read_to_string(&rustclaw_skill).with_context(|| {
                format!(
                    "reading bundled skill at {}",
                    rustclaw_skill.display()
                )
            })?;
            return Ok(parse_skill_content(&content));
        }
    }

    // Layer 3: built-in fallback prompts. Compiled-in prompts have no
    // frontmatter; their policy is hardcoded here intentionally — these
    // are the *core* ritual phases bundled with gid-core itself, and
    // changing their contract requires a code change anyway.
    let (prompt, policy) = match skill_name {
        "draft-design" => (
            include_str!("prompts/draft_design.txt").to_string(),
            SkillFilePolicy::Optional,
        ),
        "update-design" => (
            include_str!("prompts/update_design.txt").to_string(),
            SkillFilePolicy::Optional,
        ),
        "generate-graph" | "design-to-graph" => (
            include_str!("prompts/generate_graph.txt").to_string(),
            SkillFilePolicy::Required,
        ),
        "update-graph" => (
            include_str!("prompts/update_graph.txt").to_string(),
            SkillFilePolicy::Required,
        ),
        "implement" => (
            include_str!("prompts/implement.txt").to_string(),
            SkillFilePolicy::Required,
        ),
        _ => anyhow::bail!("Unknown skill: {}", skill_name),
    };

    Ok(LoadedSkill::new(prompt, policy))
}

/// Parse a raw SKILL.md (or .gid/skills/<name>.md) string into a
/// `LoadedSkill`. Handles three formats:
///
/// - Standard YAML frontmatter delimited by `---` lines.
/// - Plain markdown with no frontmatter (entire body is the prompt;
///   policy defaults to `Optional`).
/// - Frontmatter that is malformed or missing fields → silently fall
///   back to defaults rather than error. The skill loader's contract is
///   "give me whatever the file says, and use sensible defaults for
///   anything missing"; failing the whole ritual because a skill author
///   typo'd `file_polcy:` would be hostile.
fn parse_skill_content(content: &str) -> LoadedSkill {
    if let Some((frontmatter, body)) = split_frontmatter(content) {
        let parsed: SkillFrontmatter = serde_yaml::from_str(frontmatter).unwrap_or_default();
        let policy = parsed.file_policy.unwrap_or_default();
        return LoadedSkill::new(body.to_string(), policy);
    }

    LoadedSkill::new(content.to_string(), SkillFilePolicy::default())
}

/// Split a markdown document into (frontmatter, body) if it begins with
/// a YAML frontmatter block. Returns `None` if no frontmatter is found.
///
/// A well-formed frontmatter block:
/// - Begins with a line that is exactly `---` (after optional leading
///   whitespace / BOM is trimmed).
/// - Ends with another line that is exactly `---`.
/// - Has at least one line of YAML between the delimiters (an empty
///   block `---\n---` is treated as "no frontmatter" — there's nothing
///   to parse).
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let stripped = content.strip_prefix('\u{feff}').unwrap_or(content);
    let trimmed_start = stripped.trim_start_matches(['\r', '\n']);

    // The first non-empty line must be exactly `---` for this to be a
    // frontmatter block. We compare against trimmed_start so that any
    // leading whitespace before `---` already stripped.
    let after_open = trimmed_start.strip_prefix("---")?;
    // The `---` opener must be followed by a newline (not part of a
    // longer string like `---foo`).
    let after_open = after_open.strip_prefix('\n').or_else(|| after_open.strip_prefix("\r\n"))?;

    // Find the closing delimiter — a line containing exactly `---`.
    // We search line-by-line so we don't accidentally match `---` that
    // appears inside a YAML string value.
    let mut byte_offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        let line_no_newline = line.trim_end_matches(['\r', '\n']);
        if line_no_newline == "---" {
            let frontmatter = &after_open[..byte_offset];
            let body_start = byte_offset + line.len();
            let body = &after_open[body_start..];
            if frontmatter.trim().is_empty() {
                return None;
            }
            return Some((frontmatter, body));
        }
        byte_offset += line.len();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(root: &Path, name: &str, content: &str) {
        let dir = root.join(".gid").join("skills");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.md", name)), content).unwrap();
    }

    #[test]
    fn parses_required_policy_from_frontmatter() {
        let content = "---\nname: implement\nfile_policy: required\n---\n# Body\nDo work.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Required);
        assert!(parsed.prompt.starts_with("# Body"));
    }

    #[test]
    fn parses_forbidden_policy_from_frontmatter() {
        let content = "---\nname: review-design\nfile_policy: forbidden\n---\nReview only.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Forbidden);
    }

    #[test]
    fn parses_optional_policy_from_frontmatter() {
        let content = "---\nname: research\nfile_policy: optional\n---\nResearch.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
    }

    #[test]
    fn missing_file_policy_defaults_to_optional() {
        // Frontmatter present but no file_policy field → default Optional
        // (back-compat with existing SKILL.md files).
        let content = "---\nname: capture-idea\ndescription: foo\n---\n# Body";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
        assert_eq!(parsed.prompt, "# Body");
    }

    #[test]
    fn no_frontmatter_defaults_to_optional() {
        let content = "# Just a markdown file.\nNo frontmatter at all.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
        assert_eq!(parsed.prompt, content);
    }

    #[test]
    fn malformed_frontmatter_silently_falls_back() {
        // Typo in field name should not crash the ritual.
        let content = "---\nname: foo\nfile_polcy: required\n---\nBody";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
    }

    #[test]
    fn unknown_policy_value_falls_back_to_optional() {
        // serde_yaml errors on an unknown variant; we swallow and default.
        let content = "---\nfile_policy: bogus\n---\nBody";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
    }

    #[test]
    fn unclosed_frontmatter_treats_whole_file_as_body() {
        // Missing closing `---` → no frontmatter detected, whole content
        // is the body.
        let content = "---\nname: foo\nfile_policy: required\n# No closer";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Optional);
        assert_eq!(parsed.prompt, content);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let content = "---\r\nfile_policy: required\r\n---\r\nBody.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Required);
    }

    #[test]
    fn handles_utf8_bom() {
        let content = "\u{feff}---\nfile_policy: forbidden\n---\nBody.";
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Forbidden);
    }

    #[test]
    fn ignores_unrelated_frontmatter_fields() {
        // Real-world SKILL.md files carry many fields the skill_loader
        // doesn't care about (name, description, triggers, tags,
        // priority, …). serde must accept them and just extract our
        // one field — otherwise every external skill breaks.
        let content = r#"---
name: review-design
description: Some description
file_policy: forbidden
version: "1.1.0"
author: potato
triggers:
  patterns:
    - "review design"
  regex:
    - "(?i)review.*design"
tags:
  - quality
priority: 55
always_load: false
recommended_iterations: 50
max_body_size: 8192
subagent_preamble: |
  Multi-line preamble.
  With several lines.
---
# Review Design

Body content."#;
        let parsed = parse_skill_content(content);
        assert_eq!(parsed.file_policy, SkillFilePolicy::Forbidden);
        assert!(parsed.prompt.starts_with("# Review Design"));
    }

    #[test]
    fn project_local_skill_takes_precedence_over_builtin() {
        // .gid/skills/implement.md should override the compiled-in
        // implement prompt. This guards against accidental shadowing
        // bugs (e.g., loading the wrong file when names overlap).
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "implement",
            "---\nfile_policy: forbidden\n---\nProject-local body.",
        );
        let loaded = load_skill("implement", tmp.path()).unwrap();
        assert_eq!(loaded.file_policy, SkillFilePolicy::Forbidden);
        assert!(loaded.prompt.contains("Project-local body"));
    }

    #[test]
    fn builtin_implement_defaults_to_required() {
        let tmp = TempDir::new().unwrap();
        // Use a HOME that doesn't have rustclaw skills, to skip layer 2.
        std::env::set_var("HOME", tmp.path());
        let loaded = load_skill("implement", tmp.path()).unwrap();
        assert_eq!(loaded.file_policy, SkillFilePolicy::Required);
    }

    #[test]
    fn unknown_skill_returns_error() {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp.path());
        let err = load_skill("definitely-not-a-real-skill-xyz", tmp.path()).unwrap_err();
        assert!(err.to_string().contains("Unknown skill"));
    }
}
