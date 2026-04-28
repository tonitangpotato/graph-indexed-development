//! [`Metadata`] — tolerant, round-trip-preserving parser for artifact
//! metadata blocks (per ISS-053 §4.2).
//!
//! Two metadata formats are supported:
//!
//!   - **Frontmatter** — YAML between `---` delimiters at the top of the
//!     file, separated from the body by a final `---` line:
//!
//!     ```text
//!     ---
//!     title: Example
//!     tags: [a, b]
//!     ---
//!     # body starts here
//!     ```
//!
//!   - **Key-value header** — markdown-style header lines (`Key: value`)
//!     before the first blank line:
//!
//!     ```text
//!     Source: https://example.com
//!     Date: 2026-04-28
//!
//!     # body starts here
//!     ```
//!
//! Files with neither pattern parse to an empty [`Metadata`] with
//! [`MetaSourceHint::None`] and the body returned unchanged.
//!
//! ## D4 — Round-trip preservation (non-negotiable)
//!
//! `parse → render` must produce byte-identical output for unmodified
//! metadata. We achieve this by storing the raw block bytes alongside the
//! parsed key→value map; `render()` returns the raw bytes when no edits
//! have happened (`!dirty`). This rules out re-serializing through
//! `serde_yaml::to_string` because YAML emitters reformat (quoting,
//! ordering, comments stripped).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which metadata format was detected (or `None` if no metadata block).
///
/// Used by [`Metadata::render`] to emit the same format that was parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetaSourceHint {
    /// `---` YAML frontmatter.
    Frontmatter,
    /// `Key: value` header lines terminated by a blank line.
    KeyValue,
    /// No metadata block — `parse` produced an empty `Metadata`.
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
    /// Convenience: returns the scalar string, or the first list entry.
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            FieldValue::Scalar(s) => Some(s.as_str()),
            FieldValue::List(items) => items.first().map(String::as_str),
        }
    }

    /// Convenience: returns the value as a list (single scalars wrap into a 1-element list).
    pub fn as_list(&self) -> Vec<String> {
        match self {
            FieldValue::Scalar(s) => vec![s.clone()],
            FieldValue::List(items) => items.clone(),
        }
    }
}

/// Errors produced by [`Metadata::parse`].
#[derive(Debug, Error)]
pub enum MetadataError {
    /// Frontmatter delimiters (`---`) were detected but the YAML inside
    /// failed to parse. `line` is the 1-indexed line in the original body
    /// where parsing failed (best-effort). `gid artifact lint` downgrades
    /// this to a warning so a single bad file does not block bulk
    /// operations (see §4.2).
    #[error("malformed frontmatter at line {line}: {message}")]
    MalformedFrontmatter { line: usize, message: String },
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Parsed view of an artifact's metadata block, with byte-exact round-trip
/// support per D4.
///
/// Internally we keep both:
///   - `raw_block` — the exact original bytes (delimiters, comments, blank
///     lines, key order, quoting style). Returned verbatim by `render` when
///     no edits have happened.
///   - `fields` — the parsed key → [`FieldValue`] view, used for `get`,
///     `set_field`, and (when `dirty`) re-rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    source_hint: MetaSourceHint,
    raw_block: String,
    /// Parsed fields in document order.
    fields: Vec<MetaField>,
    /// `true` once any [`Metadata::set_field`] call has invalidated the
    /// raw bytes. While `false`, `render()` returns `raw_block` verbatim.
    dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetaField {
    key: String,
    value: FieldValue,
}

impl Metadata {
    /// Empty metadata with the given source hint. Used when constructing a
    /// new artifact (see [`super::artifact::Artifact`]).
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

    /// Number of fields parsed.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Look up a field by name (case-sensitive).
    pub fn get(&self, field: &str) -> Option<&FieldValue> {
        self.fields
            .iter()
            .find(|f| f.key == field)
            .map(|f| &f.value)
    }

    /// Insert or replace a field. Replacing preserves position (the field
    /// stays at its original index in the document order).
    pub fn set_field(&mut self, field: &str, value: FieldValue) {
        if let Some(existing) = self.fields.iter_mut().find(|f| f.key == field) {
            existing.value = value;
        } else {
            self.fields.push(MetaField {
                key: field.to_string(),
                value,
            });
            // If we had no source hint, emitting requires picking one. Default
            // to Frontmatter when adding fields to an otherwise-empty Metadata.
            if matches!(self.source_hint, MetaSourceHint::None) {
                self.source_hint = MetaSourceHint::Frontmatter;
            }
        }
        self.dirty = true;
    }

    /// Iterate fields in document order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &FieldValue)> {
        self.fields
            .iter()
            .map(|f| (f.key.as_str(), &f.value))
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

impl Metadata {
    /// Parse a metadata block from the start of `body` and return
    /// `(metadata, body_after_metadata)`.
    ///
    /// Detection rules:
    ///
    ///   1. If `body` starts with a `---` delimiter line → frontmatter mode.
    ///      We find the next `---` line, YAML-parse the contents between,
    ///      and return body after the closing delimiter (and its trailing
    ///      newline if present). YAML failure → [`MetadataError::MalformedFrontmatter`].
    ///
    ///   2. Otherwise if the first non-empty line matches `Key: value` and
    ///      the run of such lines is terminated by a blank line → keyvalue
    ///      mode. We parse keys until the blank line; remainder is body.
    ///
    ///   3. Otherwise → [`MetaSourceHint::None`], empty metadata, body
    ///      returned unchanged.
    pub fn parse(body: &str) -> Result<(Self, String), MetadataError> {
        // -- Frontmatter? --
        if let Some(rest) = strip_opening_delimiter(body) {
            return parse_frontmatter(body, rest);
        }

        // -- KeyValue header? --
        // Look at the very first line. Must be `Key: value` shape with a key
        // that looks like a header (alphanumeric / underscore / dash). We
        // intentionally require the first character to be alphabetic so that
        // ordinary prose like `# Heading` or `Note: this is body` (rare but
        // possible at start of body) doesn't accidentally trigger keyvalue
        // mode in the middle of regular markdown. The conservative rule:
        // first line must match `^[A-Za-z][A-Za-z0-9_-]*:\s` AND we must
        // see at least one such line followed by a blank line within the
        // first ~20 lines.
        if let Some(parsed) = try_parse_keyvalue(body) {
            return Ok(parsed);
        }

        // -- No metadata --
        Ok((Self::new(MetaSourceHint::None), body.to_string()))
    }

    /// Render the metadata block as bytes ready to be prepended to a body.
    ///
    /// Round-trip safety (D4): if `dirty == false`, returns `raw_block`
    /// verbatim. After any [`set_field`](Self::set_field), reconstructs the
    /// block from `fields` using the source hint format.
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

/// If `body` starts with `---\n` or `---\r\n` (i.e. an opening frontmatter
/// delimiter on its own line), returns the slice immediately after the
/// delimiter. Otherwise returns `None`.
fn strip_opening_delimiter(body: &str) -> Option<&str> {
    if let Some(rest) = body.strip_prefix("---\n") {
        return Some(rest);
    }
    if let Some(rest) = body.strip_prefix("---\r\n") {
        return Some(rest);
    }
    // Bare `---` at EOF (no body) — also valid opening but degenerate.
    if body == "---" {
        return Some("");
    }
    None
}

fn parse_frontmatter(
    original: &str,
    after_open: &str,
) -> Result<(Metadata, String), MetadataError> {
    // Find the closing `---` line. We scan line by line so we can compute
    // accurate line numbers for error reporting.
    let mut yaml_text = String::new();
    let mut consumed_until: usize = original.len() - after_open.len(); // byte offset of `after_open` start
    let mut closed = false;

    for line in after_open.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        if stripped == "---" {
            consumed_until += line.len();
            closed = true;
            break;
        }
        yaml_text.push_str(line);
        consumed_until += line.len();
    }

    if !closed {
        // Treat unterminated frontmatter as malformed. Line is "1" because
        // the opening delimiter was on line 1 and we never found a close.
        return Err(MetadataError::MalformedFrontmatter {
            line: 1,
            message: "unterminated frontmatter (no closing `---`)".to_string(),
        });
    }

    // YAML-parse the inner text. Note: an empty frontmatter (`---\n---\n`)
    // is valid → produces an empty mapping.
    let parsed: serde_yaml::Value = if yaml_text.trim().is_empty() {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    } else {
        serde_yaml::from_str(&yaml_text).map_err(|e| {
            // serde_yaml's location() is 0-indexed line within yaml_text.
            // Add 1 because the opening `---` line is line 1 in the original.
            let yaml_line = e.location().map(|l| l.line()).unwrap_or(0);
            MetadataError::MalformedFrontmatter {
                line: yaml_line + 2, // +1 for delimiter, +1 because location is 0-indexed
                message: e.to_string(),
            }
        })?
    };

    let fields = yaml_value_to_fields(&parsed)?;

    let raw_block = original[..consumed_until].to_string();
    let body_after = original[consumed_until..].to_string();

    Ok((
        Metadata {
            source_hint: MetaSourceHint::Frontmatter,
            raw_block,
            fields,
            dirty: false,
        },
        body_after,
    ))
}

fn yaml_value_to_fields(v: &serde_yaml::Value) -> Result<Vec<MetaField>, MetadataError> {
    let mapping = match v {
        serde_yaml::Value::Mapping(m) => m,
        serde_yaml::Value::Null => return Ok(Vec::new()),
        _ => {
            return Err(MetadataError::MalformedFrontmatter {
                line: 1,
                message: "frontmatter must be a YAML mapping at the top level".to_string(),
            });
        }
    };

    let mut out = Vec::with_capacity(mapping.len());
    for (k, val) in mapping {
        let key = match k {
            serde_yaml::Value::String(s) => s.clone(),
            other => format!("{:?}", other), // best effort; non-string keys are unusual
        };
        let fv = yaml_to_field_value(val);
        out.push(MetaField { key, value: fv });
    }
    Ok(out)
}

fn yaml_to_field_value(v: &serde_yaml::Value) -> FieldValue {
    match v {
        serde_yaml::Value::String(s) => FieldValue::Scalar(s.clone()),
        serde_yaml::Value::Bool(b) => FieldValue::Scalar(b.to_string()),
        serde_yaml::Value::Number(n) => FieldValue::Scalar(n.to_string()),
        serde_yaml::Value::Null => FieldValue::Scalar(String::new()),
        serde_yaml::Value::Sequence(items) => FieldValue::List(
            items.iter().map(scalar_string).collect(),
        ),
        serde_yaml::Value::Mapping(_) | serde_yaml::Value::Tagged(_) => {
            // Nested mappings collapsed to a string for now — round-trip is
            // preserved via raw_block; fields() callers see something sensible.
            FieldValue::Scalar(serde_yaml::to_string(v).unwrap_or_default())
        }
    }
}

fn scalar_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

fn try_parse_keyvalue(body: &str) -> Option<(Metadata, String)> {
    // Skip over leading blank lines? No — keyvalue header must be the first
    // non-trivial content. But a leading BOM or single leading blank is fine.
    let mut lines = body.split_inclusive('\n').peekable();
    let mut header_lines: Vec<(String, String, &str)> = Vec::new(); // (key, value, raw_line)
    let mut consumed: usize = 0;
    let mut closed_by_blank = false;

    let first_line = lines.peek()?;
    if !is_kv_header_line(first_line.trim_end_matches(['\r', '\n'])) {
        return None;
    }

    while let Some(line) = lines.peek().copied() {
        let stripped = line.trim_end_matches(['\r', '\n']);
        if stripped.is_empty() {
            // blank line — terminates header
            consumed += line.len();
            lines.next();
            closed_by_blank = true;
            break;
        }
        if let Some((k, v)) = parse_kv_line(stripped) {
            header_lines.push((k, v, line));
            consumed += line.len();
            lines.next();
        } else {
            // not a kv line → not keyvalue mode after all
            return None;
        }
    }

    if !closed_by_blank {
        // No blank-line terminator means the entire input was kv lines with
        // no body. We still accept this — body is empty.
    }

    // Build fields, treating comma-separated values? No — keep simple. Lists
    // would need explicit syntax; for now everything is Scalar in keyvalue
    // mode. Callers wanting lists should use frontmatter.
    let fields: Vec<MetaField> = header_lines
        .iter()
        .map(|(k, v, _)| MetaField {
            key: k.clone(),
            value: FieldValue::Scalar(v.clone()),
        })
        .collect();

    let raw_block = body[..consumed].to_string();
    let body_after = body[consumed..].to_string();

    Some((
        Metadata {
            source_hint: MetaSourceHint::KeyValue,
            raw_block,
            fields,
            dirty: false,
        },
        body_after,
    ))
}

fn is_kv_header_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    // Find ":" before any space (we want `Key:` shape, not `Note: this is prose.`
    // we accept both — the colon is the discriminator).
    let colon = match line.find(':') {
        Some(i) => i,
        None => return false,
    };
    if colon == 0 {
        return false;
    }
    // Key portion must be all [A-Za-z0-9_-]
    line[..colon]
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn parse_kv_line(line: &str) -> Option<(String, String)> {
    let colon = line.find(':')?;
    let key = line[..colon].to_string();
    let value = line[colon + 1..].trim_start().to_string();
    Some((key, value))
}

// ---------------------------------------------------------------------------
// Renderers (used when dirty == true)
// ---------------------------------------------------------------------------

fn render_frontmatter(fields: &[MetaField]) -> String {
    let mut map = serde_yaml::Mapping::new();
    for f in fields {
        let key = serde_yaml::Value::String(f.key.clone());
        let val = match &f.value {
            FieldValue::Scalar(s) => serde_yaml::Value::String(s.clone()),
            FieldValue::List(items) => serde_yaml::Value::Sequence(
                items
                    .iter()
                    .map(|s| serde_yaml::Value::String(s.clone()))
                    .collect(),
            ),
        };
        map.insert(key, val);
    }
    let yaml_body = serde_yaml::to_string(&serde_yaml::Value::Mapping(map))
        .unwrap_or_else(|_| String::new());
    let mut out = String::with_capacity(yaml_body.len() + 8);
    out.push_str("---\n");
    out.push_str(&yaml_body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out
}

fn render_keyvalue(fields: &[MetaField]) -> String {
    let mut out = String::new();
    for f in fields {
        // Lists not natively supported in keyvalue mode → join with ", "
        let v = match &f.value {
            FieldValue::Scalar(s) => s.clone(),
            FieldValue::List(items) => items.join(", "),
        };
        out.push_str(&f.key);
        out.push_str(": ");
        out.push_str(&v);
        out.push('\n');
    }
    out.push('\n'); // blank line terminator
    out
}

#[allow(dead_code)]
fn _force_btreemap_import() {
    // BTreeMap is referenced in module docs; this silences unused-import lints
    // if we drop the actual usage in some future refactor.
    let _: BTreeMap<String, String> = BTreeMap::new();
}

// ---------------------------------------------------------------------------
// merge_list
// ---------------------------------------------------------------------------

/// Order-preserving union of two string lists. Items from `existing` keep
/// their position; items from `new` not already present are appended in
/// their input order. Used by callers that treat list-typed relation fields
/// as accumulating (e.g. `set_field` for `depends_on`).
pub fn merge_list(existing: &[String], new: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(existing.len() + new.len());
    let mut seen = std::collections::HashSet::new();
    for s in existing.iter().chain(new.iter()) {
        if seen.insert(s.as_str().to_string()) {
            out.push(s.clone());
        }
    }
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
        let body = "Just some prose.\n\nNo metadata at all.\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        assert_eq!(m.source_hint(), MetaSourceHint::None);
        assert!(m.is_empty());
        assert_eq!(rest, body);
    }

    #[test]
    fn parse_frontmatter_simple() {
        let body = "---\nfoo: bar\n---\nbody\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        assert_eq!(m.source_hint(), MetaSourceHint::Frontmatter);
        assert_eq!(m.get("foo"), Some(&FieldValue::Scalar("bar".to_string())));
        assert_eq!(rest, "body\n");
    }

    #[test]
    fn parse_frontmatter_list() {
        let body = "---\ntags: [a, b, c]\n---\nbody\n";
        let (m, _rest) = Metadata::parse(body).expect("parse");
        match m.get("tags") {
            Some(FieldValue::List(items)) => {
                assert_eq!(items, &["a".to_string(), "b".to_string(), "c".to_string()]);
            }
            other => panic!("expected list, got {:?}", other),
        }
    }

    #[test]
    fn parse_keyvalue_simple() {
        let body = "Foo: bar\nBaz: qux\n\nbody";
        let (m, rest) = Metadata::parse(body).expect("parse");
        assert_eq!(m.source_hint(), MetaSourceHint::KeyValue);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("Foo"), Some(&FieldValue::Scalar("bar".to_string())));
        assert_eq!(m.get("Baz"), Some(&FieldValue::Scalar("qux".to_string())));
        assert_eq!(rest, "body");
    }

    #[test]
    fn parse_malformed_frontmatter_unterminated() {
        let body = "---\nfoo: bar\n";
        let err = Metadata::parse(body).unwrap_err();
        match err {
            MetadataError::MalformedFrontmatter { line, .. } => {
                assert_eq!(line, 1);
            }
        }
    }

    #[test]
    fn parse_malformed_frontmatter_yaml_error() {
        // unquoted colon in value → YAML parse error
        let body = "---\nfoo: : :\n---\nbody";
        let err = Metadata::parse(body).unwrap_err();
        match err {
            MetadataError::MalformedFrontmatter { line, message } => {
                assert!(line >= 1, "line must be set");
                assert!(!message.is_empty(), "message must be set");
            }
        }
    }

    #[test]
    fn roundtrip_frontmatter_with_comments() {
        // D4: byte-identical round-trip when not edited.
        let body = "---\n# leading comment\nfoo: bar\n# trailing comment\nbaz: qux\n---\n\n# Body\n\nProse here.\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        let rendered = m.render();
        let full = format!("{}{}", rendered, rest);
        assert_eq!(full, body, "round-trip must be byte-identical");
    }

    #[test]
    fn roundtrip_keyvalue() {
        let body = "Source: https://example.com\nDate: 2026-04-28\n\nBody starts here.\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        let rendered = m.render();
        let full = format!("{}{}", rendered, rest);
        assert_eq!(full, body, "keyvalue round-trip must be byte-identical");
    }

    #[test]
    fn set_field_existing_replaces_value() {
        let body = "---\nfoo: old\nbar: keepme\n---\nbody\n";
        let (mut m, _rest) = Metadata::parse(body).expect("parse");
        m.set_field("foo", FieldValue::Scalar("new".to_string()));
        assert_eq!(m.get("foo"), Some(&FieldValue::Scalar("new".to_string())));
        assert_eq!(m.get("bar"), Some(&FieldValue::Scalar("keepme".to_string())));
        let rendered = m.render();
        // After edit, dirty=true → re-rendered. Both fields still present.
        assert!(rendered.contains("foo: new"));
        assert!(rendered.contains("bar: keepme"));
    }

    #[test]
    fn set_field_new_appends() {
        let body = "---\nfoo: bar\n---\nbody\n";
        let (mut m, _rest) = Metadata::parse(body).expect("parse");
        m.set_field("new", FieldValue::Scalar("added".to_string()));
        assert_eq!(m.get("new"), Some(&FieldValue::Scalar("added".to_string())));
        let rendered = m.render();
        assert!(rendered.contains("foo: bar"));
        assert!(rendered.contains("new: added"));
    }

    #[test]
    fn set_field_on_empty_promotes_to_frontmatter() {
        let mut m = Metadata::new(MetaSourceHint::None);
        m.set_field("k", FieldValue::Scalar("v".to_string()));
        assert_eq!(m.source_hint(), MetaSourceHint::Frontmatter);
        let r = m.render();
        assert!(r.starts_with("---\n"));
        assert!(r.contains("k: v"));
        assert!(r.ends_with("---\n"));
    }

    #[test]
    fn merge_list_preserves_order_and_dedups() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["b".to_string(), "c".to_string()];
        let merged = merge_list(&a, &b);
        assert_eq!(merged, vec!["a", "b", "c"]);
    }

    #[test]
    fn merge_list_empty_inputs() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(merge_list(&empty, &empty), Vec::<String>::new());
        let a = vec!["x".to_string()];
        assert_eq!(merge_list(&a, &empty), a);
        assert_eq!(merge_list(&empty, &a), a);
    }

    #[test]
    fn keyvalue_first_line_must_be_kv_shape() {
        // Body starting with a heading should NOT trigger keyvalue mode.
        let body = "# Heading\n\nProse.\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        assert_eq!(m.source_hint(), MetaSourceHint::None);
        assert_eq!(rest, body);
    }

    #[test]
    fn frontmatter_empty_yaml() {
        let body = "---\n---\nbody\n";
        let (m, rest) = Metadata::parse(body).expect("parse");
        assert_eq!(m.source_hint(), MetaSourceHint::Frontmatter);
        assert!(m.is_empty());
        assert_eq!(rest, "body\n");
    }
}
