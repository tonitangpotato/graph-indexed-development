//! [`Metadata`] — tolerant, round-trip-preserving parser for artifact
//! metadata blocks (per ISS-053 §4.2).
//!
//! Two metadata formats are supported:
//!
//!   - **Frontmatter** — YAML between `---` delimiters at the top of the
//!     file, separated from the body by a final `---` line.
//!   - **Key-value header** — markdown-style header lines (`Key: value`)
//!     before the first blank line.
//!
//! Files with neither pattern parse to an empty [`Metadata`] with
//! [`MetaSourceHint::None`] and the body returned unchanged.
//!
//! ## D4 — Round-trip preservation (non-negotiable)
//!
//! `parse → render` must produce byte-identical output for unmodified
//! metadata. We achieve this by storing the raw block bytes alongside the
//! parsed key→value map; `render()` returns the raw bytes when no edits
//! have happened (`!dirty`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which metadata format was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetaSourceHint {
    Frontmatter,
    KeyValue,
    None,
}

/// Value of a metadata field — either a single scalar or a list of strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    Scalar(String),
    List(Vec<String>),
}

impl FieldValue {
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            FieldValue::Scalar(s) => Some(s.as_str()),
            FieldValue::List(items) => items.first().map(String::as_str),
        }
    }

    pub fn as_list(&self) -> Vec<String> {
        match self {
            FieldValue::Scalar(s) => vec![s.clone()],
            FieldValue::List(items) => items.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("malformed frontmatter at line {line}: {message}")]
    MalformedFrontmatter { line: usize, message: String },
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Parsed view with byte-exact round-trip support per D4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    source_hint: MetaSourceHint,
    raw_block: String,
    fields: Vec<MetaField>,
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetaField {
    key: String,
    value: FieldValue,
}

impl Metadata {
    pub fn new(source_hint: MetaSourceHint) -> Self {
        Self {
            source_hint,
            raw_block: String::new(),
            fields: Vec::new(),
            dirty: false,
        }
    }

    pub fn source_hint(&self) -> MetaSourceHint {
        self.source_hint
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn get(&self, field: &str) -> Option<&FieldValue> {
        self.fields
            .iter()
            .find(|f| f.key == field)
            .map(|f| &f.value)
    }

    /// Insert or replace a field. Replacing preserves position.
    pub fn set_field(&mut self, field: &str, value: FieldValue) {
        if let Some(existing) = self.fields.iter_mut().find(|f| f.key == field) {
            existing.value = value;
        } else {
            self.fields.push(MetaField {
                key: field.to_string(),
                value,
            });
            if matches!(self.source_hint, MetaSourceHint::None) {
                self.source_hint = MetaSourceHint::Frontmatter;
            }
        }
        self.dirty = true;
    }

    pub fn fields(&self) -> impl Iterator<Item = (&str, &FieldValue)> {
        self.fields.iter().map(|f| (f.key.as_str(), &f.value))
    }

    /// Parse a metadata block from the start of `body`.
    pub fn parse(body: &str) -> Result<(Self, String), MetadataError> {
        if let Some((meta, rest)) = parse_frontmatter(body)? {
            return Ok((meta, rest));
        }
        if let Some((meta, rest)) = parse_keyvalue(body) {
            return Ok((meta, rest));
        }
        Ok((Self::new(MetaSourceHint::None), body.to_string()))
    }

    /// Render the metadata block. Byte-identical to the parsed input when
    /// no [`set_field`] has been called.
    pub fn render(&self) -> String {
        if !self.dirty {
            return self.raw_block.clone();
        }
        match self.source_hint {
            MetaSourceHint::None => String::new(),
            MetaSourceHint::Frontmatter => render_frontmatter(&self.fields),
            MetaSourceHint::KeyValue => render_keyvalue(&self.fields),
        }
    }
}

/// Order-preserving union of two string lists.
pub fn merge_list(existing: &[String], new: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(existing.len() + new.len());
    let mut seen = std::collections::HashSet::new();
    for s in existing.iter().chain(new.iter()) {
        if seen.insert(s.clone()) {
            out.push(s.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Frontmatter parser
// ---------------------------------------------------------------------------

fn parse_frontmatter(body: &str) -> Result<Option<(Metadata, String)>, MetadataError> {
    let after_open = if let Some(rest) = body.strip_prefix("---\r\n") {
        rest
    } else if let Some(rest) = body.strip_prefix("---\n") {
        rest
    } else {
        return Ok(None);
    };

    // Find closing `---` line.
    let mut yaml_end: Option<usize> = None;
    let mut close_end: Option<usize> = None;
    let mut cursor = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            yaml_end = Some(cursor);
            close_end = Some(cursor + line.len());
            break;
        }
        cursor += line.len();
    }
    if yaml_end.is_none() {
        let trimmed = after_open[cursor..].trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            yaml_end = Some(cursor);
            close_end = Some(after_open.len());
        }
    }

    let (yaml_end, close_end) = match (yaml_end, close_end) {
        (Some(y), Some(c)) => (y, c),
        _ => {
            return Err(MetadataError::MalformedFrontmatter {
                line: 1,
                message: "missing closing `---` delimiter".to_string(),
            });
        }
    };

    let yaml_src = &after_open[..yaml_end];
    let raw_block_len = (body.len() - after_open.len()) + close_end;
    let raw_block = body[..raw_block_len].to_string();
    let rest = body[raw_block_len..].to_string();

    let fields = parse_yaml_mapping(yaml_src)?;

    Ok(Some((
        Metadata {
            source_hint: MetaSourceHint::Frontmatter,
            raw_block,
            fields,
            dirty: false,
        },
        rest,
    )))
}

fn parse_yaml_mapping(yaml_src: &str) -> Result<Vec<MetaField>, MetadataError> {
    use serde_yaml::Value;

    if yaml_src.trim().is_empty() {
        return Ok(Vec::new());
    }

    let value: Value = serde_yaml::from_str(yaml_src).map_err(|e| {
        let line = e.location().map(|l| l.line()).unwrap_or(1);
        MetadataError::MalformedFrontmatter {
            line,
            message: e.to_string(),
        }
    })?;

    let mapping = match value {
        Value::Mapping(m) => m,
        Value::Null => return Ok(Vec::new()),
        _ => {
            return Err(MetadataError::MalformedFrontmatter {
                line: 1,
                message: "frontmatter must be a YAML mapping".to_string(),
            });
        }
    };

    let mut fields = Vec::with_capacity(mapping.len());
    for (k, v) in mapping {
        let key = match k {
            Value::String(s) => s,
            other => {
                return Err(MetadataError::MalformedFrontmatter {
                    line: 1,
                    message: format!("non-string key: {:?}", other),
                });
            }
        };
        let value = match yaml_to_field_value(&v) {
            Some(fv) => fv,
            None => {
                return Err(MetadataError::MalformedFrontmatter {
                    line: 1,
                    message: format!("unsupported value shape for key `{}`", key),
                });
            }
        };
        fields.push(MetaField { key, value });
    }
    Ok(fields)
}

fn yaml_to_field_value(v: &serde_yaml::Value) -> Option<FieldValue> {
    use serde_yaml::Value;
    match v {
        Value::String(s) => Some(FieldValue::Scalar(s.clone())),
        Value::Bool(b) => Some(FieldValue::Scalar(b.to_string())),
        Value::Number(n) => Some(FieldValue::Scalar(n.to_string())),
        Value::Null => Some(FieldValue::Scalar(String::new())),
        Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                match it {
                    Value::String(s) => out.push(s.clone()),
                    Value::Bool(b) => out.push(b.to_string()),
                    Value::Number(n) => out.push(n.to_string()),
                    Value::Null => out.push(String::new()),
                    _ => return None,
                }
            }
            Some(FieldValue::List(out))
        }
        Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Key-value header parser
// ---------------------------------------------------------------------------

fn parse_keyvalue(body: &str) -> Option<(Metadata, String)> {
    if body.is_empty() {
        return None;
    }

    let first_line_end = body.find('\n').unwrap_or(body.len());
    let first_line = body[..first_line_end].trim_end_matches('\r');
    if !is_keyvalue_line(first_line) {
        return None;
    }

    let mut fields: Vec<MetaField> = Vec::new();
    let mut cursor = 0usize;
    let block_end: usize;

    loop {
        let line_end_rel = body[cursor..].find('\n');
        let (line, advance) = match line_end_rel {
            Some(rel) => (&body[cursor..cursor + rel], rel + 1),
            None => (&body[cursor..], body.len() - cursor),
        };
        let line_no_cr = line.trim_end_matches('\r');

        if line_no_cr.is_empty() {
            block_end = cursor + advance;
            break;
        }

        if !is_keyvalue_line(line_no_cr) {
            return None;
        }

        let (key, value_str) = split_keyvalue(line_no_cr);
        fields.push(MetaField {
            key: key.to_string(),
            value: FieldValue::Scalar(value_str.to_string()),
        });

        cursor += advance;
        if cursor >= body.len() {
            // EOF without a terminating blank line — accept anyway: the
            // entire input is metadata, body is empty.
            block_end = body.len();
            break;
        }
    }

    let raw_block = body[..block_end].to_string();
    let rest = body[block_end..].to_string();

    Some((
        Metadata {
            source_hint: MetaSourceHint::KeyValue,
            raw_block,
            fields,
            dirty: false,
        },
        rest,
    ))
}

fn is_keyvalue_line(line: &str) -> bool {
    let Some(colon_idx) = line.find(':') else {
        return false;
    };
    let key = &line[..colon_idx];
    if key.is_empty() {
        return false;
    }
    if key.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return false;
    }
    if !key.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    true
}

fn split_keyvalue(line: &str) -> (&str, &str) {
    let colon_idx = line.find(':').expect("checked by is_keyvalue_line");
    let key = &line[..colon_idx];
    let after_colon = &line[colon_idx + 1..];
    let value = after_colon.strip_prefix(' ').unwrap_or(after_colon);
    (key, value)
}

// ---------------------------------------------------------------------------
// Renderers (used only when `dirty`)
// ---------------------------------------------------------------------------

fn render_frontmatter(fields: &[MetaField]) -> String {
    let mut mapping = serde_yaml::Mapping::new();
    for f in fields {
        let key = serde_yaml::Value::String(f.key.clone());
        let value = field_value_to_yaml(&f.value);
        mapping.insert(key, value);
    }
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping))
        .unwrap_or_else(|_| String::new());
    let yaml = yaml.trim_end_matches('\n');
    format!("---\n{}\n---\n", yaml)
}

fn field_value_to_yaml(v: &FieldValue) -> serde_yaml::Value {
    match v {
        FieldValue::Scalar(s) => serde_yaml::Value::String(s.clone()),
        FieldValue::List(items) => serde_yaml::Value::Sequence(
            items
                .iter()
                .map(|s| serde_yaml::Value::String(s.clone()))
                .collect(),
        ),
    }
}

fn render_keyvalue(fields: &[MetaField]) -> String {
    let mut out = String::new();
    for f in fields {
        match &f.value {
            FieldValue::Scalar(s) => {
                out.push_str(&f.key);
                out.push_str(": ");
                out.push_str(s);
                out.push('\n');
            }
            FieldValue::List(items) => {
                out.push_str(&f.key);
                out.push_str(": ");
                out.push_str(&items.join(", "));
                out.push('\n');
            }
        }
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_metadata() {
        let body = "# Hello\n\nJust a doc.\n";
        let (meta, rest) = Metadata::parse(body).unwrap();
        assert_eq!(meta.source_hint(), MetaSourceHint::None);
        assert!(meta.is_empty());
        assert_eq!(rest, body);
    }

    #[test]
    fn parse_frontmatter_simple() {
        let body = "---\nfoo: bar\n---\nbody\n";
        let (meta, rest) = Metadata::parse(body).unwrap();
        assert_eq!(meta.source_hint(), MetaSourceHint::Frontmatter);
        assert_eq!(meta.len(), 1);
        assert_eq!(
            meta.get("foo"),
            Some(&FieldValue::Scalar("bar".to_string()))
        );
        assert_eq!(rest, "body\n");
    }

    #[test]
    fn parse_frontmatter_list() {
        let body = "---\ntags:\n  - a\n  - b\n---\nbody";
        let (meta, _rest) = Metadata::parse(body).unwrap();
        assert_eq!(
            meta.get("tags"),
            Some(&FieldValue::List(vec!["a".to_string(), "b".to_string()]))
        );
    }

    #[test]
    fn parse_keyvalue_simple() {
        let body = "Foo: bar\nBaz: qux\n\nbody";
        let (meta, rest) = Metadata::parse(body).unwrap();
        assert_eq!(meta.source_hint(), MetaSourceHint::KeyValue);
        assert_eq!(meta.len(), 2);
        assert_eq!(meta.get("Foo").unwrap().as_scalar(), Some("bar"));
        assert_eq!(meta.get("Baz").unwrap().as_scalar(), Some("qux"));
        assert_eq!(rest, "body");
    }

    #[test]
    fn parse_malformed_frontmatter_yaml() {
        // `: :` is invalid YAML.
        let body = "---\nfoo: : :\n---\nbody";
        let err = Metadata::parse(body).unwrap_err();
        assert!(matches!(err, MetadataError::MalformedFrontmatter { .. }));
    }

    #[test]
    fn parse_malformed_frontmatter_missing_close() {
        let body = "---\nfoo: bar\nbody without close";
        let err = Metadata::parse(body).unwrap_err();
        match err {
            MetadataError::MalformedFrontmatter { message, .. } => {
                assert!(message.contains("closing"));
            }
        }
    }

    #[test]
    fn roundtrip_frontmatter_with_comments() {
        // D4: byte-exact round-trip preserves comments, blank lines, ordering.
        let body = "---\n# top comment\nfoo: bar\n\nbaz: \"quoted\"\n# trailing comment\n---\n# Body Heading\nstuff\n";
        let (meta, rest) = Metadata::parse(body).unwrap();
        let rendered = format!("{}{}", meta.render(), rest);
        assert_eq!(rendered, body, "round-trip must be byte-identical");
    }

    #[test]
    fn roundtrip_keyvalue() {
        let body = "Source: https://example.com\nDate: 2026-04-28\n\nbody starts here\n";
        let (meta, rest) = Metadata::parse(body).unwrap();
        let rendered = format!("{}{}", meta.render(), rest);
        assert_eq!(rendered, body);
    }

    #[test]
    fn set_field_existing_replaces_position() {
        let body = "---\nfoo: bar\nbaz: qux\n---\nbody\n";
        let (mut meta, _rest) = Metadata::parse(body).unwrap();
        meta.set_field("foo", FieldValue::Scalar("updated".to_string()));
        let out = meta.render();
        // After edit, dirty path emits canonical YAML; the replaced field
        // keeps its position (foo before baz).
        assert!(out.contains("foo: updated"));
        assert!(out.contains("baz: qux"));
        let foo_idx = out.find("foo:").unwrap();
        let baz_idx = out.find("baz:").unwrap();
        assert!(foo_idx < baz_idx);
    }

    #[test]
    fn set_field_new_appends() {
        let body = "---\nfoo: bar\n---\nbody\n";
        let (mut meta, _rest) = Metadata::parse(body).unwrap();
        meta.set_field("new_key", FieldValue::Scalar("new_val".to_string()));
        let out = meta.render();
        assert!(out.contains("new_key: new_val"));
        assert!(out.contains("foo: bar"));
    }

    #[test]
    fn set_field_on_empty_picks_frontmatter() {
        let mut meta = Metadata::new(MetaSourceHint::None);
        meta.set_field("k", FieldValue::Scalar("v".to_string()));
        assert_eq!(meta.source_hint(), MetaSourceHint::Frontmatter);
        let out = meta.render();
        assert!(out.starts_with("---\n"));
        assert!(out.contains("k: v"));
    }

    #[test]
    fn merge_list_preserves_order_and_dedups() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["b".to_string(), "c".to_string()];
        let merged = merge_list(&a, &b);
        assert_eq!(merged, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_no_leading_block_when_first_line_isnt_keyvalue() {
        // A markdown heading must NOT be parsed as a metadata block.
        let body = "# Heading\n\nKey: value\n\nbody";
        let (meta, rest) = Metadata::parse(body).unwrap();
        assert_eq!(meta.source_hint(), MetaSourceHint::None);
        assert_eq!(rest, body);
    }
}
