//! Layout — pattern-driven artifact path layout (ISS-053 §4.4, D2).
//!
//! Bidirectional pattern engine: same DSL is used for both matching paths
//! to (kind + slots) and rendering paths from (kind + slots).
//!
//! Adding a new kind = edit `.gid/layout.yml`, no code change. (D2.)

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::metadata::MetaSourceHint;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub type SlotMap = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeqScope {
    Project,
    Parent { rel: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRule {
    pub kind: String,
    pub metadata_format: MetaSourceHint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPattern {
    pub pattern: String,
    pub kind: String,
    pub metadata_format: MetaSourceHint,
    #[serde(default = "default_seq_scope")]
    pub seq_scope: SeqScope,
}

fn default_seq_scope() -> SeqScope {
    SeqScope::Project
}

impl LayoutPattern {
    /// Inner template of `{id:TEMPLATE}` if present, e.g.
    /// `"issues/{id:ISS-{seq:04}}/issue.md"` → `Some("ISS-{seq:04}")`.
    pub fn id_template_str(&self) -> Option<String> {
        let bytes = self.pattern.as_bytes();
        let mut i = 0;
        while i + 4 <= bytes.len() {
            if &bytes[i..i + 4] == b"{id:" {
                let start = i + 4;
                let mut depth = 1usize;
                let mut j = start;
                while j < bytes.len() {
                    match bytes[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(self.pattern[start..j].to_string());
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                return None;
            }
            i += 1;
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub patterns: Vec<LayoutPattern>,
    pub fallback: FallbackRule,
    #[serde(default = "default_relation_fields")]
    pub relation_fields: Vec<String>,
}

fn default_relation_fields() -> Vec<String> {
    vec![
        "related".into(),
        "blocks".into(),
        "blocked_by".into(),
        "supersedes".into(),
        "derives_from".into(),
        "applies_to".into(),
        "references".into(),
        "depends_on".into(),
        "satisfies".into(),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult {
    pub kind: String,
    pub slots: SlotMap,
    pub fallback: bool,
}

#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("unknown kind: {0}")]
    UnknownKind(String),

    #[error("missing slot {slot:?} for kind {kind:?}")]
    MissingSlot { kind: String, slot: String },

    #[error("sequence exhausted for pattern {pattern:?}: max value {max}")]
    SeqExhausted { pattern: String, max: u64 },

    #[error("malformed pattern {pattern:?}: {message}")]
    MalformedPattern { pattern: String, message: String },
}

// ---------------------------------------------------------------------------
// Default layout (covers today's empirical .gid/ reality, sampled 2026-04-26)
// ---------------------------------------------------------------------------

impl Default for Layout {
    fn default() -> Self {
        Self {
            patterns: default_patterns(),
            fallback: FallbackRule {
                kind: "note".into(),
                metadata_format: MetaSourceHint::None,
            },
            relation_fields: default_relation_fields(),
        }
    }
}

fn default_patterns() -> Vec<LayoutPattern> {
    use MetaSourceHint::Frontmatter as FM;
    use MetaSourceHint::None as MNone;
    vec![
        LayoutPattern {
            pattern: "issues/{id:ISS-{seq:03}}/issue.md".into(),
            kind: "issue".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "issues/{parent_id}/design.md".into(),
            kind: "design".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "issues/{parent_id}/requirements.md".into(),
            kind: "requirements".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "issues/{parent_id}/reviews/{name}.md".into(),
            kind: "review".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Parent {
                rel: "issues/{parent_id}/reviews".into(),
            },
        },
        LayoutPattern {
            pattern: "issues/{parent_id}/{any}.md".into(),
            kind: "issue-doc".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        // ISS-053 Phase G2 Gap 4: nested issue subdirs
        // (e.g. `issues/ISS-024/wip/README.md`).
        // Goes BEFORE feature patterns so the literal `issues/` prefix
        // is matched before the generic `{slug}/{any}.md` fallback could
        // intercept anything 4-deep.
        LayoutPattern {
            pattern: "issues/{parent_id}/{slug}/{any}.md".into(),
            kind: "issue-doc-nested".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{slug}/requirements.md".into(),
            kind: "feature-requirements".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{slug}/design.md".into(),
            kind: "feature-design".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{slug}/reviews/{name}.md".into(),
            kind: "feature-review".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Parent {
                rel: "features/{slug}/reviews".into(),
            },
        },
        LayoutPattern {
            pattern: "features/{slug}/{any}.md".into(),
            kind: "feature-doc".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        // ISS-053 Phase G2 Gap 2: top-level legacy designs at
        // `features/{name}.md` (rustclaw has 5 of these). Two segments,
        // so it cannot collide with the three-segment `features/{slug}/...`
        // patterns above. Listed AFTER them so the slug-dir form wins
        // when a sibling slug-dir exists.
        LayoutPattern {
            pattern: "features/{name}.md".into(),
            kind: "feature-doc-toplevel".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        // ISS-053 Phase G2 Gap 1: nested feature subfolders
        // (engram knowledge-compiler meta-feature: 12 files at
        // `features/{slug}/{subslug}/...`). 4- and 5-segment patterns;
        // outer feature slug captured as `parent_id`, inner sub-feature
        // slug captured as `slug`. SlotMap is BTreeMap<String,String>
        // so the inner `slug` is what survives — the outer is in
        // `parent_id`, which is exactly the round-trip semantic we want.
        LayoutPattern {
            pattern: "features/{parent_id}/{slug}/requirements.md".into(),
            kind: "nested-feature-requirements".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{parent_id}/{slug}/design.md".into(),
            kind: "nested-feature-design".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "features/{parent_id}/{slug}/reviews/{name}.md".into(),
            kind: "nested-feature-review".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Parent {
                rel: "features/{parent_id}/{slug}/reviews".into(),
            },
        },
        LayoutPattern {
            pattern: "features/{parent_id}/{slug}/{any}.md".into(),
            kind: "nested-feature-doc".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "docs/{name}.md".into(),
            kind: "doc".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        // ISS-053 Phase G2 Gap 3: `docs/reviews/{name}.md` —
        // engram has architecture review docs here. Must come BEFORE
        // the generic `docs/{slug}/{name}.md` since `reviews` is a
        // valid `{slug}` value and the first match wins.
        LayoutPattern {
            pattern: "docs/reviews/{name}.md".into(),
            kind: "doc-review".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        // ISS-053 Phase G2 Gap 3: nested docs subdir (engram
        // `docs/reviews/`, rustclaw `docs/discussion/`).
        LayoutPattern {
            pattern: "docs/{slug}/{name}.md".into(),
            kind: "doc-nested".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "reviews/{name}.md".into(),
            kind: "global-review".into(),
            metadata_format: FM,
            seq_scope: SeqScope::Project,
        },
        LayoutPattern {
            pattern: "{slug}/{any}.md".into(),
            kind: "note".into(),
            metadata_format: MNone,
            seq_scope: SeqScope::Project,
        },
    ]
}

// ---------------------------------------------------------------------------
// Layout API
// ---------------------------------------------------------------------------

impl Layout {
    pub fn relation_fields(&self) -> &[String] {
        &self.relation_fields
    }

    pub fn match_path(&self, rel: &str) -> MatchResult {
        for pat in &self.patterns {
            if let Some(slots) = pattern_match(&pat.pattern, rel) {
                return MatchResult {
                    kind: pat.kind.clone(),
                    slots,
                    fallback: false,
                };
            }
        }
        MatchResult {
            kind: self.fallback.kind.clone(),
            slots: SlotMap::new(),
            fallback: true,
        }
    }

    pub fn resolve(&self, kind: &str, slots: &SlotMap) -> Result<PathBuf, LayoutError> {
        let pat = self
            .patterns
            .iter()
            .find(|p| p.kind == kind)
            .ok_or_else(|| LayoutError::UnknownKind(kind.to_string()))?;
        let rendered = pattern_render(&pat.pattern, slots, kind)?;
        Ok(PathBuf::from(rendered))
    }

    pub fn pattern_for_kind(&self, kind: &str) -> Option<&LayoutPattern> {
        self.patterns.iter().find(|p| p.kind == kind)
    }
}

// ---------------------------------------------------------------------------
// DSL — tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(String),
    Slug,
    Name,
    ParentId,
    Any,
    Seq { width: usize },
    Id { template: Vec<IdToken> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IdToken {
    Literal(String),
    Seq { width: usize },
}

fn tokenize_segment(seg: &str) -> Result<Vec<Token>, String> {
    let mut out: Vec<Token> = Vec::new();
    let bytes = seg.as_bytes();
    let mut i = 0usize;
    let mut lit_start = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if lit_start < i {
                out.push(Token::Literal(seg[lit_start..i].to_string()));
            }
            let start = i;
            let mut depth = 1usize;
            let mut j = i + 1;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            if depth != 0 {
                return Err(format!(
                    "unterminated `{{` at byte {} in segment `{}`",
                    start, seg
                ));
            }
            let inner = &seg[start + 1..j];
            out.push(parse_placeholder(inner)?);
            i = j + 1;
            lit_start = i;
        } else if bytes[i] == b'}' {
            return Err(format!(
                "unexpected `}}` at byte {} in segment `{}`",
                i, seg
            ));
        } else {
            i += 1;
        }
    }
    if lit_start < bytes.len() {
        out.push(Token::Literal(seg[lit_start..].to_string()));
    }
    Ok(out)
}

fn parse_placeholder(inner: &str) -> Result<Token, String> {
    if let Some(rest) = inner.strip_prefix("seq:") {
        let width: usize = rest
            .parse()
            .map_err(|_| format!("invalid seq width in `{{seq:{}}}`", rest))?;
        if width == 0 {
            return Err("seq width must be ≥ 1".into());
        }
        return Ok(Token::Seq { width });
    }
    if let Some(rest) = inner.strip_prefix("id:") {
        return Ok(Token::Id {
            template: parse_id_template(rest)?,
        });
    }
    match inner {
        "slug" => Ok(Token::Slug),
        "name" => Ok(Token::Name),
        "parent_id" => Ok(Token::ParentId),
        "any" => Ok(Token::Any),
        other => Err(format!("unknown placeholder `{{{}}}`", other)),
    }
}

fn parse_id_template(inner: &str) -> Result<Vec<IdToken>, String> {
    let mut out: Vec<IdToken> = Vec::new();
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    let mut lit_start = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if lit_start < i {
                out.push(IdToken::Literal(inner[lit_start..i].to_string()));
            }
            let start = i;
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'}' {
                j += 1;
            }
            if j >= bytes.len() {
                return Err(format!(
                    "unterminated `{{` at byte {} in id template",
                    start
                ));
            }
            let token_inner = &inner[start + 1..j];
            if let Some(rest) = token_inner.strip_prefix("seq:") {
                let width: usize = rest
                    .parse()
                    .map_err(|_| format!("invalid seq width inside id template: `{}`", rest))?;
                if width == 0 {
                    return Err("seq width must be ≥ 1".into());
                }
                out.push(IdToken::Seq { width });
            } else {
                return Err(format!(
                    "id template may only contain `{{seq:NN}}`, found `{{{}}}`",
                    token_inner
                ));
            }
            i = j + 1;
            lit_start = i;
        } else if bytes[i] == b'}' {
            return Err(format!("unexpected `}}` at byte {} in id template", i));
        } else {
            i += 1;
        }
    }
    if lit_start < bytes.len() {
        out.push(IdToken::Literal(inner[lit_start..].to_string()));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pattern matching
// ---------------------------------------------------------------------------

fn pattern_match(pattern: &str, path: &str) -> Option<SlotMap> {
    let pat_segs: Vec<&str> = pattern.split('/').collect();
    let path_segs: Vec<&str> = path.split('/').collect();
    if pat_segs.len() != path_segs.len() {
        return None;
    }
    let mut slots = SlotMap::new();
    for (pseg, vseg) in pat_segs.iter().zip(path_segs.iter()) {
        let tokens = tokenize_segment(pseg).ok()?;
        if !match_segment(&tokens, vseg, &mut slots) {
            return None;
        }
    }
    Some(slots)
}

fn match_segment(tokens: &[Token], seg: &str, slots: &mut SlotMap) -> bool {
    let mut cursor = 0usize;
    let mut idx = 0usize;
    while idx < tokens.len() {
        let token = &tokens[idx];
        match token {
            Token::Literal(lit) => {
                if !seg[cursor..].starts_with(lit.as_str()) {
                    return false;
                }
                cursor += lit.len();
            }
            _ => {
                let next_lit = tokens[idx + 1..].iter().find_map(|t| {
                    if let Token::Literal(l) = t {
                        Some(l.as_str())
                    } else {
                        None
                    }
                });
                let value_end = match next_lit {
                    Some(lit) => match seg[cursor..].find(lit) {
                        Some(rel) => cursor + rel,
                        None => return false,
                    },
                    None => seg.len(),
                };
                let value = &seg[cursor..value_end];
                if !match_placeholder(token, value, slots) {
                    return false;
                }
                cursor = value_end;
            }
        }
        idx += 1;
    }
    cursor == seg.len()
}

fn match_placeholder(token: &Token, value: &str, slots: &mut SlotMap) -> bool {
    match token {
        Token::Slug => {
            if value.is_empty()
                || !value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                return false;
            }
            slots.insert("slug".into(), value.into());
            true
        }
        Token::Name => {
            if value.is_empty() {
                return false;
            }
            slots.insert("name".into(), value.into());
            true
        }
        Token::ParentId => {
            if value.is_empty() {
                return false;
            }
            slots.insert("parent_id".into(), value.into());
            true
        }
        Token::Any => {
            if value.is_empty() {
                return false;
            }
            slots.insert("any".into(), value.into());
            true
        }
        Token::Seq { width } => {
            if value.len() < *width || !value.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            slots.insert("seq".into(), value.into());
            true
        }
        Token::Id { template } => match_id_template(template, value, slots),
        Token::Literal(_) => unreachable!("literals handled above"),
    }
}

fn match_id_template(template: &[IdToken], value: &str, slots: &mut SlotMap) -> bool {
    let mut cursor = 0usize;
    let mut captured_seq: Option<String> = None;
    let mut i = 0usize;
    while i < template.len() {
        let tk = &template[i];
        match tk {
            IdToken::Literal(lit) => {
                if !value[cursor..].starts_with(lit.as_str()) {
                    return false;
                }
                cursor += lit.len();
            }
            IdToken::Seq { width } => {
                let next_lit = template[i + 1..].iter().find_map(|t| {
                    if let IdToken::Literal(l) = t {
                        Some(l.as_str())
                    } else {
                        None
                    }
                });
                let value_end = match next_lit {
                    Some(lit) => match value[cursor..].find(lit) {
                        Some(r) => cursor + r,
                        None => return false,
                    },
                    None => value.len(),
                };
                let seq_val = &value[cursor..value_end];
                if seq_val.len() < *width || !seq_val.chars().all(|c| c.is_ascii_digit()) {
                    return false;
                }
                captured_seq = Some(seq_val.into());
                cursor = value_end;
            }
        }
        i += 1;
    }
    if cursor != value.len() {
        return false;
    }
    slots.insert("id".into(), value.into());
    if let Some(seq) = captured_seq {
        slots.insert("seq".into(), seq);
    }
    true
}

// ---------------------------------------------------------------------------
// Pattern rendering
// ---------------------------------------------------------------------------

fn pattern_render(pattern: &str, slots: &SlotMap, kind: &str) -> Result<String, LayoutError> {
    let mut out = String::with_capacity(pattern.len());
    let segs: Vec<&str> = pattern.split('/').collect();
    for (idx, seg) in segs.iter().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        let tokens = tokenize_segment(seg).map_err(|message| LayoutError::MalformedPattern {
            pattern: pattern.into(),
            message,
        })?;
        for token in tokens {
            render_token(&token, slots, kind, pattern, &mut out)?;
        }
    }
    Ok(out)
}

fn render_token(
    token: &Token,
    slots: &SlotMap,
    kind: &str,
    pattern: &str,
    out: &mut String,
) -> Result<(), LayoutError> {
    match token {
        Token::Literal(lit) => {
            out.push_str(lit);
            Ok(())
        }
        Token::Slug => fetch_slot(slots, kind, "slug").map(|v| out.push_str(v)),
        Token::Name => fetch_slot(slots, kind, "name").map(|v| out.push_str(v)),
        Token::ParentId => fetch_slot(slots, kind, "parent_id").map(|v| out.push_str(v)),
        Token::Any => fetch_slot(slots, kind, "any").map(|v| out.push_str(v)),
        Token::Seq { width } => {
            let s = fetch_slot(slots, kind, "seq")?;
            check_seq_width(s, *width, pattern)?;
            pad_into(out, s, *width);
            Ok(())
        }
        Token::Id { template } => render_id_token(template, slots, kind, pattern, out),
    }
}

fn render_id_token(
    template: &[IdToken],
    slots: &SlotMap,
    kind: &str,
    pattern: &str,
    out: &mut String,
) -> Result<(), LayoutError> {
    if let Some(id) = slots.get("id") {
        out.push_str(id);
        return Ok(());
    }
    for tk in template {
        match tk {
            IdToken::Literal(lit) => out.push_str(lit),
            IdToken::Seq { width } => {
                let s = fetch_slot(slots, kind, "seq")?;
                check_seq_width(s, *width, pattern)?;
                pad_into(out, s, *width);
            }
        }
    }
    Ok(())
}

fn pad_into(out: &mut String, s: &str, width: usize) {
    if s.len() < width {
        for _ in 0..(width - s.len()) {
            out.push('0');
        }
    }
    out.push_str(s);
}

fn fetch_slot<'a>(slots: &'a SlotMap, kind: &str, slot: &str) -> Result<&'a str, LayoutError> {
    slots
        .get(slot)
        .map(String::as_str)
        .ok_or_else(|| LayoutError::MissingSlot {
            kind: kind.to_string(),
            slot: slot.to_string(),
        })
}

fn check_seq_width(value: &str, width: usize, pattern: &str) -> Result<(), LayoutError> {
    let parsed: u64 = value.parse().map_err(|_| LayoutError::MalformedPattern {
        pattern: pattern.into(),
        message: format!("seq slot value `{}` is not numeric", value),
    })?;
    let max = 10u64.saturating_pow(width as u32) - 1;
    if parsed > max {
        return Err(LayoutError::SeqExhausted {
            pattern: pattern.into(),
            max,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn slots_of(pairs: &[(&str, &str)]) -> SlotMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- tokenizer ---

    #[test]
    fn tokenize_literal_only() {
        assert_eq!(
            tokenize_segment("issue.md").unwrap(),
            vec![Token::Literal("issue.md".into())]
        );
    }

    #[test]
    fn tokenize_simple_slug() {
        assert_eq!(tokenize_segment("{slug}").unwrap(), vec![Token::Slug]);
    }

    #[test]
    fn tokenize_id_with_nested_seq() {
        assert_eq!(
            tokenize_segment("{id:ISS-{seq:04}}").unwrap(),
            vec![Token::Id {
                template: vec![
                    IdToken::Literal("ISS-".into()),
                    IdToken::Seq { width: 4 }
                ]
            }]
        );
    }

    #[test]
    fn tokenize_unterminated_brace_errors() {
        let err = tokenize_segment("{slug").unwrap_err();
        assert!(err.contains("unterminated"));
    }

    #[test]
    fn tokenize_unknown_placeholder_errors() {
        let err = tokenize_segment("{wat}").unwrap_err();
        assert!(err.contains("unknown placeholder"));
    }

    #[test]
    fn tokenize_id_disallows_other_slots() {
        let err = tokenize_segment("{id:{slug}-{seq:04}}").unwrap_err();
        assert!(err.contains("id template may only contain"));
    }

    // --- match_path ---

    #[test]
    fn match_issue_path() {
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-0042/issue.md");
        assert_eq!(m.kind, "issue");
        assert!(!m.fallback);
        assert_eq!(m.slots.get("id"), Some(&"ISS-0042".to_string()));
        assert_eq!(m.slots.get("seq"), Some(&"0042".to_string()));
    }

    #[test]
    fn match_issue_review_path() {
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-0042/reviews/design-r2.md");
        assert_eq!(m.kind, "review");
        assert!(!m.fallback);
        assert_eq!(m.slots.get("parent_id"), Some(&"ISS-0042".to_string()));
        assert_eq!(m.slots.get("name"), Some(&"design-r2".to_string()));
    }

    #[test]
    fn match_feature_requirements() {
        let layout = Layout::default();
        let m = layout.match_path("features/dim-extract/requirements.md");
        assert_eq!(m.kind, "feature-requirements");
        assert_eq!(m.slots.get("slug"), Some(&"dim-extract".to_string()));
    }

    #[test]
    fn match_falls_back_for_top_level_md() {
        let layout = Layout::default();
        let m = layout.match_path("README.md");
        assert!(m.fallback);
        assert_eq!(m.kind, "note");
    }

    // --- resolve ---

    #[test]
    fn resolve_issue_with_id_slot() {
        let layout = Layout::default();
        let path = layout
            .resolve("issue", &slots_of(&[("id", "ISS-042")]))
            .unwrap();
        assert_eq!(path, PathBuf::from("issues/ISS-042/issue.md"));
    }

    #[test]
    fn resolve_issue_with_seq_only() {
        let layout = Layout::default();
        let path = layout
            .resolve("issue", &slots_of(&[("seq", "042")]))
            .unwrap();
        assert_eq!(path, PathBuf::from("issues/ISS-042/issue.md"));
    }

    #[test]
    fn resolve_issue_with_short_seq_pads() {
        let layout = Layout::default();
        let path = layout.resolve("issue", &slots_of(&[("seq", "5")])).unwrap();
        assert_eq!(path, PathBuf::from("issues/ISS-005/issue.md"));
    }

    #[test]
    fn resolve_review_under_issue() {
        let layout = Layout::default();
        let path = layout
            .resolve(
                "review",
                &slots_of(&[("parent_id", "ISS-0042"), ("name", "design-r2")]),
            )
            .unwrap();
        assert_eq!(path, PathBuf::from("issues/ISS-0042/reviews/design-r2.md"));
    }

    #[test]
    fn resolve_unknown_kind_errors() {
        let layout = Layout::default();
        let err = layout.resolve("nope", &SlotMap::new()).unwrap_err();
        assert!(matches!(err, LayoutError::UnknownKind(ref s) if s == "nope"));
    }

    #[test]
    fn resolve_missing_slot_errors() {
        let layout = Layout::default();
        let err = layout.resolve("issue", &SlotMap::new()).unwrap_err();
        match err {
            LayoutError::MissingSlot { kind, slot } => {
                assert_eq!(kind, "issue");
                assert_eq!(slot, "seq");
            }
            other => panic!("expected MissingSlot, got {:?}", other),
        }
    }

    #[test]
    fn resolve_seq_overflow_errors() {
        let layout = Layout::default();
        let err = layout
            .resolve("issue", &slots_of(&[("seq", "10000")]))
            .unwrap_err();
        match err {
            LayoutError::SeqExhausted { max, .. } => assert_eq!(max, 999),
            other => panic!("expected SeqExhausted, got {:?}", other),
        }
    }

    // --- bidirectional round-trip ---

    #[test]
    fn match_then_resolve_round_trip_issue() {
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-0042/issue.md");
        let path = layout.resolve(&m.kind, &m.slots).unwrap();
        assert_eq!(path.to_str().unwrap(), "issues/ISS-0042/issue.md");
    }

    #[test]
    fn match_then_resolve_round_trip_review() {
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-0042/reviews/design-r2.md");
        let path = layout.resolve(&m.kind, &m.slots).unwrap();
        assert_eq!(
            path.to_str().unwrap(),
            "issues/ISS-0042/reviews/design-r2.md"
        );
    }

    // --- id_template_str ---

    #[test]
    fn id_template_str_extracts_inner() {
        let pat = LayoutPattern {
            pattern: "issues/{id:ISS-{seq:04}}/issue.md".into(),
            kind: "issue".into(),
            metadata_format: MetaSourceHint::Frontmatter,
            seq_scope: SeqScope::Project,
        };
        assert_eq!(pat.id_template_str(), Some("ISS-{seq:04}".into()));
    }

    #[test]
    fn id_template_str_none_when_absent() {
        let pat = LayoutPattern {
            pattern: "features/{slug}/design.md".into(),
            kind: "feature-design".into(),
            metadata_format: MetaSourceHint::Frontmatter,
            seq_scope: SeqScope::Project,
        };
        assert_eq!(pat.id_template_str(), None);
    }

    // ---------------------------------------------------------------------
    // ISS-053 Phase G2 — corpus regression: 4 default-Layout gaps
    //
    // Each test names the originating corpus + path so anyone reading a
    // failure can trace back to the exact real-world file the pattern was
    // added for. See `.gid/issues/ISS-053/phase-g/verification-2026-04-28.md`
    // for the full inventory.
    // ---------------------------------------------------------------------

    // Gap 1 — engram knowledge-compiler meta-feature
    #[test]
    fn iss053_gap1_nested_feature_requirements() {
        let layout = Layout::default();
        let m = layout.match_path("features/knowledge-compiler/compilation/requirements.md");
        assert_eq!(m.kind, "nested-feature-requirements");
        assert!(!m.fallback);
        assert_eq!(
            m.slots.get("parent_id"),
            Some(&"knowledge-compiler".to_string())
        );
        assert_eq!(m.slots.get("slug"), Some(&"compilation".to_string()));
    }

    #[test]
    fn iss053_gap1_nested_feature_design() {
        let layout = Layout::default();
        let m = layout.match_path("features/knowledge-compiler/maintenance/design.md");
        assert_eq!(m.kind, "nested-feature-design");
        assert!(!m.fallback);
    }

    #[test]
    fn iss053_gap1_nested_feature_review() {
        let layout = Layout::default();
        let m = layout.match_path(
            "features/knowledge-compiler/platform/reviews/design-r1.md",
        );
        assert_eq!(m.kind, "nested-feature-review");
        assert!(!m.fallback);
        assert_eq!(
            m.slots.get("parent_id"),
            Some(&"knowledge-compiler".to_string())
        );
        assert_eq!(m.slots.get("slug"), Some(&"platform".to_string()));
        assert_eq!(m.slots.get("name"), Some(&"design-r1".to_string()));
    }

    #[test]
    fn iss053_gap1_nested_feature_round_trip() {
        let layout = Layout::default();
        let path = "features/knowledge-compiler/compilation/design.md";
        let m = layout.match_path(path);
        let resolved = layout.resolve(&m.kind, &m.slots).unwrap();
        assert_eq!(resolved.to_str().unwrap(), path);
    }

    // Gap 2 — rustclaw legacy `features/DESIGN-*.md`
    #[test]
    fn iss053_gap2_toplevel_feature_doc() {
        let layout = Layout::default();
        let m = layout.match_path("features/DESIGN-claude-proxy.md");
        assert_eq!(m.kind, "feature-doc-toplevel");
        assert!(!m.fallback);
        assert_eq!(
            m.slots.get("name"),
            Some(&"DESIGN-claude-proxy".to_string())
        );
    }

    #[test]
    fn iss053_gap2_toplevel_feature_doc_round_trip() {
        let layout = Layout::default();
        let path = "features/BATCH3-DESIGN.md";
        let m = layout.match_path(path);
        let resolved = layout.resolve(&m.kind, &m.slots).unwrap();
        assert_eq!(resolved.to_str().unwrap(), path);
    }

    // Slug-dir form must still win over toplevel form
    #[test]
    fn iss053_gap2_slug_dir_wins_over_toplevel() {
        let layout = Layout::default();
        let m = layout.match_path("features/dim-extract/requirements.md");
        assert_eq!(m.kind, "feature-requirements");
    }

    // Gap 3 — `docs/reviews/`, `docs/discussion/`
    #[test]
    fn iss053_gap3_doc_review() {
        let layout = Layout::default();
        let m = layout.match_path("docs/reviews/architecture-r1.md");
        assert_eq!(m.kind, "doc-review");
        assert!(!m.fallback);
        assert_eq!(m.slots.get("name"), Some(&"architecture-r1".to_string()));
    }

    #[test]
    fn iss053_gap3_doc_nested() {
        let layout = Layout::default();
        let m = layout.match_path("docs/discussion/04-16.md");
        assert_eq!(m.kind, "doc-nested");
        assert!(!m.fallback);
        assert_eq!(m.slots.get("slug"), Some(&"discussion".to_string()));
        assert_eq!(m.slots.get("name"), Some(&"04-16".to_string()));
    }

    #[test]
    fn iss053_gap3_doc_review_takes_precedence_over_doc_nested() {
        // `reviews` is a syntactically valid {slug}, so without the
        // explicit literal-`reviews` pattern positioned first, this would
        // fall into doc-nested. Verify the ordering.
        let layout = Layout::default();
        let m = layout.match_path("docs/reviews/something.md");
        assert_eq!(m.kind, "doc-review");
    }

    #[test]
    fn iss053_gap3_flat_docs_still_works() {
        // Single-segment `docs/{name}.md` must not regress.
        let layout = Layout::default();
        let m = layout.match_path("docs/architecture.md");
        assert_eq!(m.kind, "doc");
    }

    // Gap 4 — engram `issues/ISS-024/wip/README.md`
    #[test]
    fn iss053_gap4_issue_doc_nested() {
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-024/wip/README.md");
        assert_eq!(m.kind, "issue-doc-nested");
        assert!(!m.fallback);
        assert_eq!(m.slots.get("parent_id"), Some(&"ISS-024".to_string()));
        assert_eq!(m.slots.get("slug"), Some(&"wip".to_string()));
        assert_eq!(m.slots.get("any"), Some(&"README".to_string()));
    }

    #[test]
    fn iss053_gap4_issue_subdir_does_not_regress_review() {
        // `issues/{parent_id}/reviews/{name}.md` must still match `review`,
        // not the new nested-doc pattern. `reviews` is a valid {slug}
        // so the earlier-listed reviews pattern must win.
        let layout = Layout::default();
        let m = layout.match_path("issues/ISS-042/reviews/design-r2.md");
        assert_eq!(m.kind, "review");
    }

    // Sanity: noop fallback still kicks in for genuinely unknown shapes
    #[test]
    fn iss053_g2_unknown_shape_still_falls_back_to_note() {
        let layout = Layout::default();
        // Two-segment unknown path matches the global `{slug}/{any}.md`
        // noop pattern (fallback=false but kind=note).
        let m = layout.match_path("randomdir/somefile.md");
        assert_eq!(m.kind, "note");
        // Top-level single-segment path falls all the way through to the
        // explicit fallback (no pattern is one segment unanchored).
        let m2 = layout.match_path("README.md");
        assert_eq!(m2.kind, "note");
        assert!(m2.fallback);
    }
}
