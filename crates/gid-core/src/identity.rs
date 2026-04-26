//! Core identity types and validation for GID.
//!
//! Provides [`Identity`], [`Organization`], and [`Group`] types with
//! email validation and signing key (fingerprint) validation.
//!
//! # Example
//!
//! ```
//! use gid_core::identity::{Identity, Organization, Group, SigningKey};
//!
//! let key = SigningKey::new("A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2").unwrap();
//! let identity = Identity::builder("alice")
//!     .email("alice@example.com").unwrap()
//!     .display_name("Alice Smith")
//!     .signing_key(key)
//!     .build()
//!     .unwrap();
//!
//! let org = Organization::new("acme-corp", "Acme Corp").unwrap();
//! let group = Group::new("backend-team", "Backend Team").unwrap();
//! ```

use std::fmt;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during identity validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The provided email address is invalid.
    InvalidEmail(String),
    /// The provided signing key fingerprint is invalid.
    InvalidSigningKey(String),
    /// A required field is missing.
    MissingField(&'static str),
    /// The identifier (slug) is invalid.
    InvalidIdentifier(String),
    /// Duplicate member in a group/organization.
    DuplicateMember(String),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEmail(email) => write!(f, "Invalid email address: {}", email),
            Self::InvalidSigningKey(key) => write!(f, "Invalid signing key fingerprint: {}", key),
            Self::MissingField(field) => write!(f, "Missing required field: {}", field),
            Self::InvalidIdentifier(id) => write!(f, "Invalid identifier '{}': must be lowercase alphanumeric with hyphens", id),
            Self::DuplicateMember(id) => write!(f, "Duplicate member: {}", id),
        }
    }
}

impl std::error::Error for IdentityError {}

// ---------------------------------------------------------------------------
// Email validation
// ---------------------------------------------------------------------------

/// A validated email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Email(String);

impl Email {
    /// Parse and validate an email address.
    ///
    /// Validates basic structure: `local@domain` where domain has at least one dot.
    pub fn new(email: &str) -> Result<Self, IdentityError> {
        if validate_email(email) {
            Ok(Self(email.to_lowercase()))
        } else {
            Err(IdentityError::InvalidEmail(email.to_string()))
        }
    }

    /// Return the email address as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Validate an email address.
///
/// Checks:
/// - Contains exactly one `@`
/// - Local part is non-empty and ≤ 64 chars
/// - Domain part is non-empty and ≤ 253 chars
/// - Domain has at least one dot
/// - Domain labels are non-empty, ≤ 63 chars, alphanumeric + hyphens, no leading/trailing hyphens
/// - No whitespace anywhere
fn validate_email(email: &str) -> bool {
    if email.contains(char::is_whitespace) {
        return false;
    }

    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 {
        return false;
    }

    let local = parts[0];
    let domain = parts[1];

    // Local part checks
    if local.is_empty() || local.len() > 64 {
        return false;
    }

    // Domain part checks
    if domain.is_empty() || domain.len() > 253 {
        return false;
    }

    // Domain must have at least one dot
    if !domain.contains('.') {
        return false;
    }

    // Validate domain labels
    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Signing key validation
// ---------------------------------------------------------------------------

/// A validated signing key fingerprint (e.g., PGP/GPG key fingerprint).
///
/// Stores a 40-character uppercase hex fingerprint (160-bit, SHA-1 format used by GPG).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SigningKey(String);

impl SigningKey {
    /// Parse and validate a signing key fingerprint.
    ///
    /// Accepts 40 hex characters (optionally with spaces or colons as separators).
    /// The stored form is uppercase hex with no separators.
    pub fn new(fingerprint: &str) -> Result<Self, IdentityError> {
        let cleaned = fingerprint
            .replace([' ', ':'], "")
            .to_uppercase();

        if validate_fingerprint(&cleaned) {
            Ok(Self(cleaned))
        } else {
            Err(IdentityError::InvalidSigningKey(fingerprint.to_string()))
        }
    }

    /// Return the fingerprint as a string slice (40 uppercase hex chars).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the last 16 hex characters (long key ID).
    pub fn long_id(&self) -> &str {
        &self.0[24..]
    }

    /// Return the last 8 hex characters (short key ID).
    pub fn short_id(&self) -> &str {
        &self.0[32..]
    }
}

impl fmt::Display for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display in groups of 4 for readability
        for (i, chunk) in self.0.as_bytes().chunks(4).enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{}", std::str::from_utf8(chunk).unwrap_or(""))?;
        }
        Ok(())
    }
}

impl AsRef<str> for SigningKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Validate a cleaned fingerprint: exactly 40 hex characters.
fn validate_fingerprint(cleaned: &str) -> bool {
    cleaned.len() == 40 && cleaned.chars().all(|c| c.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Identifier validation
// ---------------------------------------------------------------------------

/// Validate an identifier (slug): lowercase alphanumeric + hyphens, non-empty,
/// must start and end with alphanumeric.
fn validate_identifier(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9-]*[a-z0-9]$|^[a-z0-9]$").unwrap());
    re.is_match(id)
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A verified individual identity within the GID system.
///
/// Identities represent human actors (developers, reviewers, approvers) who
/// participate in rituals and approve phase transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// Unique identifier (slug), e.g. "alice" or "bob-smith".
    pub id: String,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// Validated email address.
    pub email: Option<Email>,
    /// Signing key fingerprint for commit/artifact verification.
    pub signing_key: Option<SigningKey>,
    /// When this identity was created.
    pub created_at: DateTime<Utc>,
    /// Arbitrary key-value metadata.
    pub metadata: std::collections::HashMap<String, String>,
}

impl Identity {
    /// Create a minimal identity with just an id.
    pub fn new(id: &str) -> Result<Self, IdentityError> {
        if !validate_identifier(id) {
            return Err(IdentityError::InvalidIdentifier(id.to_string()));
        }
        Ok(Self {
            id: id.to_string(),
            display_name: None,
            email: None,
            signing_key: None,
            created_at: Utc::now(),
            metadata: std::collections::HashMap::new(),
        })
    }

    /// Start building an identity with a fluent API.
    pub fn builder(id: &str) -> IdentityBuilder {
        IdentityBuilder::new(id)
    }

    /// Set the email, validating it.
    pub fn set_email(&mut self, email: &str) -> Result<(), IdentityError> {
        self.email = Some(Email::new(email)?);
        Ok(())
    }

    /// Set the signing key, validating the fingerprint.
    pub fn set_signing_key(&mut self, fingerprint: &str) -> Result<(), IdentityError> {
        self.signing_key = Some(SigningKey::new(fingerprint)?);
        Ok(())
    }

    /// Check whether this identity has a signing key.
    pub fn has_signing_key(&self) -> bool {
        self.signing_key.is_some()
    }

    /// Return a short description for display.
    pub fn display(&self) -> String {
        if let Some(ref name) = self.display_name {
            format!("{} ({})", name, self.id)
        } else if let Some(ref email) = self.email {
            format!("{} <{}>", self.id, email)
        } else {
            self.id.clone()
        }
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display())
    }
}

// ---------------------------------------------------------------------------
// Identity builder
// ---------------------------------------------------------------------------

/// Fluent builder for [`Identity`].
pub struct IdentityBuilder {
    id: String,
    display_name: Option<String>,
    email: Option<Email>,
    signing_key: Option<SigningKey>,
    metadata: std::collections::HashMap<String, String>,
    error: Option<IdentityError>,
}

impl IdentityBuilder {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            display_name: None,
            email: None,
            signing_key: None,
            metadata: std::collections::HashMap::new(),
            error: None,
        }
    }

    /// Set the display name.
    pub fn display_name(mut self, name: &str) -> Self {
        self.display_name = Some(name.to_string());
        self
    }

    /// Set and validate the email address.
    pub fn email(mut self, email: &str) -> Result<Self, IdentityError> {
        self.email = Some(Email::new(email)?);
        Ok(self)
    }

    /// Set a pre-validated signing key.
    pub fn signing_key(mut self, key: SigningKey) -> Self {
        self.signing_key = Some(key);
        self
    }

    /// Set a signing key from a fingerprint string, validating it.
    pub fn signing_key_str(mut self, fingerprint: &str) -> Result<Self, IdentityError> {
        self.signing_key = Some(SigningKey::new(fingerprint)?);
        Ok(self)
    }

    /// Add a metadata key-value pair.
    pub fn meta(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Build the identity, validating all fields.
    pub fn build(self) -> Result<Identity, IdentityError> {
        if let Some(err) = self.error {
            return Err(err);
        }

        if !validate_identifier(&self.id) {
            return Err(IdentityError::InvalidIdentifier(self.id));
        }

        Ok(Identity {
            id: self.id,
            display_name: self.display_name,
            email: self.email,
            signing_key: self.signing_key,
            created_at: Utc::now(),
            metadata: self.metadata,
        })
    }
}

// ---------------------------------------------------------------------------
// Organization
// ---------------------------------------------------------------------------

/// An organization that can contain identities and groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organization {
    /// Unique identifier (slug).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Member identity IDs.
    pub members: Vec<String>,
    /// Group IDs within this organization.
    pub groups: Vec<String>,
    /// When this organization was created.
    pub created_at: DateTime<Utc>,
    /// Arbitrary key-value metadata.
    pub metadata: std::collections::HashMap<String, String>,
}

impl Organization {
    /// Create a new organization.
    pub fn new(id: &str, name: &str) -> Result<Self, IdentityError> {
        if !validate_identifier(id) {
            return Err(IdentityError::InvalidIdentifier(id.to_string()));
        }

        Ok(Self {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            members: Vec::new(),
            groups: Vec::new(),
            created_at: Utc::now(),
            metadata: std::collections::HashMap::new(),
        })
    }

    /// Add a member identity ID. Returns error if duplicate.
    pub fn add_member(&mut self, identity_id: &str) -> Result<(), IdentityError> {
        if self.members.contains(&identity_id.to_string()) {
            return Err(IdentityError::DuplicateMember(identity_id.to_string()));
        }
        self.members.push(identity_id.to_string());
        Ok(())
    }

    /// Remove a member identity ID. Returns true if removed.
    pub fn remove_member(&mut self, identity_id: &str) -> bool {
        if let Some(pos) = self.members.iter().position(|m| m == identity_id) {
            self.members.remove(pos);
            true
        } else {
            false
        }
    }

    /// Check if an identity is a member.
    pub fn is_member(&self, identity_id: &str) -> bool {
        self.members.iter().any(|m| m == identity_id)
    }

    /// Add a group ID. Returns error if duplicate.
    pub fn add_group(&mut self, group_id: &str) -> Result<(), IdentityError> {
        if self.groups.contains(&group_id.to_string()) {
            return Err(IdentityError::DuplicateMember(group_id.to_string()));
        }
        self.groups.push(group_id.to_string());
        Ok(())
    }

    /// Remove a group ID. Returns true if removed.
    pub fn remove_group(&mut self, group_id: &str) -> bool {
        if let Some(pos) = self.groups.iter().position(|g| g == group_id) {
            self.groups.remove(pos);
            true
        } else {
            false
        }
    }
}

impl fmt::Display for Organization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, {} members)", self.name, self.id, self.members.len())
    }
}

// ---------------------------------------------------------------------------
// Group
// ---------------------------------------------------------------------------

/// A named group of identities within an organization.
///
/// Groups are used for role-based access and approval routing.
/// For example, a "reviewers" group might be required to approve design phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Unique identifier (slug).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description of this group's purpose.
    pub description: Option<String>,
    /// Member identity IDs.
    pub members: Vec<String>,
    /// When this group was created.
    pub created_at: DateTime<Utc>,
    /// Arbitrary key-value metadata.
    pub metadata: std::collections::HashMap<String, String>,
}

impl Group {
    /// Create a new group.
    pub fn new(id: &str, name: &str) -> Result<Self, IdentityError> {
        if !validate_identifier(id) {
            return Err(IdentityError::InvalidIdentifier(id.to_string()));
        }

        Ok(Self {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            members: Vec::new(),
            created_at: Utc::now(),
            metadata: std::collections::HashMap::new(),
        })
    }

    /// Add a member identity ID. Returns error if duplicate.
    pub fn add_member(&mut self, identity_id: &str) -> Result<(), IdentityError> {
        if self.members.contains(&identity_id.to_string()) {
            return Err(IdentityError::DuplicateMember(identity_id.to_string()));
        }
        self.members.push(identity_id.to_string());
        Ok(())
    }

    /// Remove a member identity ID. Returns true if removed.
    pub fn remove_member(&mut self, identity_id: &str) -> bool {
        if let Some(pos) = self.members.iter().position(|m| m == identity_id) {
            self.members.remove(pos);
            true
        } else {
            false
        }
    }

    /// Check if an identity is a member.
    pub fn is_member(&self, identity_id: &str) -> bool {
        self.members.iter().any(|m| m == identity_id)
    }

    /// Return the number of members.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, {} members)", self.name, self.id, self.members.len())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Email validation --------------------------------------------------

    #[test]
    fn test_valid_emails() {
        let valid = [
            "user@example.com",
            "alice.bob@sub.domain.org",
            "test+tag@gmail.com",
            "a@b.co",
            "user123@test-domain.io",
        ];
        for email in &valid {
            assert!(Email::new(email).is_ok(), "Expected valid: {}", email);
        }
    }

    #[test]
    fn test_invalid_emails() {
        let invalid = [
            "",
            "noat",
            "@domain.com",
            "user@",
            "user@domain",         // no dot in domain
            "user@.com",           // empty label
            "user@domain.",        // trailing dot → empty label
            "user@-domain.com",    // leading hyphen in label
            "user@domain-.com",    // trailing hyphen in label
            "user @domain.com",    // whitespace
            "user@dom ain.com",    // whitespace in domain
        ];
        for email in &invalid {
            assert!(Email::new(email).is_err(), "Expected invalid: '{}'", email);
        }
    }

    #[test]
    fn test_email_normalises_to_lowercase() {
        let email = Email::new("Alice@Example.COM").unwrap();
        assert_eq!(email.as_str(), "alice@example.com");
    }

    // -- Signing key validation --------------------------------------------

    #[test]
    fn test_valid_signing_keys() {
        let fp = "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2";
        let key = SigningKey::new(fp).unwrap();
        assert_eq!(key.as_str(), fp);
    }

    #[test]
    fn test_signing_key_with_spaces() {
        let fp = "A1B2 C3D4 E5F6 A1B2 C3D4 E5F6 A1B2 C3D4 E5F6 A1B2";
        let key = SigningKey::new(fp).unwrap();
        assert_eq!(key.as_str(), "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2");
    }

    #[test]
    fn test_signing_key_with_colons() {
        let fp = "A1B2:C3D4:E5F6:A1B2:C3D4:E5F6:A1B2:C3D4:E5F6:A1B2";
        let key = SigningKey::new(fp).unwrap();
        assert_eq!(key.as_str(), "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2");
    }

    #[test]
    fn test_signing_key_lowercase_normalised() {
        let fp = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let key = SigningKey::new(fp).unwrap();
        assert_eq!(key.as_str(), "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2");
    }

    #[test]
    fn test_invalid_signing_keys() {
        assert!(SigningKey::new("").is_err());
        assert!(SigningKey::new("tooshort").is_err());
        assert!(SigningKey::new("ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ").is_err()); // non-hex
        assert!(SigningKey::new("A1B2C3D4").is_err()); // too short
        assert!(SigningKey::new("A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2FF").is_err()); // too long
    }

    #[test]
    fn test_signing_key_ids() {
        let fp = "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2";
        let key = SigningKey::new(fp).unwrap();
        assert_eq!(key.long_id(), "A1B2C3D4E5F6A1B2");
        assert_eq!(key.short_id(), "E5F6A1B2");
    }

    #[test]
    fn test_signing_key_display() {
        let fp = "A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2";
        let key = SigningKey::new(fp).unwrap();
        let display = format!("{}", key);
        assert_eq!(display, "A1B2 C3D4 E5F6 A1B2 C3D4 E5F6 A1B2 C3D4 E5F6 A1B2");
    }

    // -- Identifier validation ---------------------------------------------

    #[test]
    fn test_valid_identifiers() {
        assert!(validate_identifier("alice"));
        assert!(validate_identifier("bob-smith"));
        assert!(validate_identifier("a"));
        assert!(validate_identifier("team-42"));
        assert!(validate_identifier("x1"));
    }

    #[test]
    fn test_invalid_identifiers() {
        assert!(!validate_identifier(""));
        assert!(!validate_identifier("-starts-with-dash"));
        assert!(!validate_identifier("ends-with-dash-"));
        assert!(!validate_identifier("has spaces"));
        assert!(!validate_identifier("UPPERCASE"));
        assert!(!validate_identifier("has_underscore"));
        assert!(!validate_identifier("has.dot"));
    }

    // -- Identity ----------------------------------------------------------

    #[test]
    fn test_identity_new() {
        let id = Identity::new("alice").unwrap();
        assert_eq!(id.id, "alice");
        assert!(id.email.is_none());
        assert!(id.signing_key.is_none());
    }

    #[test]
    fn test_identity_invalid_id() {
        assert!(Identity::new("").is_err());
        assert!(Identity::new("UPPER").is_err());
        assert!(Identity::new("-bad").is_err());
    }

    #[test]
    fn test_identity_set_email() {
        let mut id = Identity::new("alice").unwrap();
        id.set_email("alice@example.com").unwrap();
        assert_eq!(id.email.as_ref().unwrap().as_str(), "alice@example.com");
    }

    #[test]
    fn test_identity_set_invalid_email() {
        let mut id = Identity::new("alice").unwrap();
        assert!(id.set_email("not-an-email").is_err());
    }

    #[test]
    fn test_identity_set_signing_key() {
        let mut id = Identity::new("alice").unwrap();
        id.set_signing_key("A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2").unwrap();
        assert!(id.has_signing_key());
    }

    #[test]
    fn test_identity_builder() {
        let key = SigningKey::new("A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2").unwrap();
        let identity = Identity::builder("alice")
            .email("alice@example.com").unwrap()
            .display_name("Alice Smith")
            .signing_key(key)
            .meta("role", "lead")
            .build()
            .unwrap();

        assert_eq!(identity.id, "alice");
        assert_eq!(identity.display_name.as_deref(), Some("Alice Smith"));
        assert_eq!(identity.email.as_ref().unwrap().as_str(), "alice@example.com");
        assert!(identity.has_signing_key());
        assert_eq!(identity.metadata.get("role").unwrap(), "lead");
    }

    #[test]
    fn test_identity_builder_invalid_id() {
        let result = Identity::builder("BAD ID").build();
        assert!(result.is_err());
    }

    #[test]
    fn test_identity_builder_invalid_email() {
        let result = Identity::builder("alice").email("nope");
        assert!(result.is_err());
    }

    #[test]
    fn test_identity_display() {
        let id = Identity::builder("alice")
            .display_name("Alice Smith")
            .build()
            .unwrap();
        assert_eq!(format!("{}", id), "Alice Smith (alice)");

        let id2 = Identity::builder("bob")
            .email("bob@example.com").unwrap()
            .build()
            .unwrap();
        assert_eq!(format!("{}", id2), "bob <bob@example.com>");

        let id3 = Identity::new("charlie").unwrap();
        assert_eq!(format!("{}", id3), "charlie");
    }

    // -- Organization ------------------------------------------------------

    #[test]
    fn test_organization_new() {
        let org = Organization::new("acme", "Acme Corp").unwrap();
        assert_eq!(org.id, "acme");
        assert_eq!(org.name, "Acme Corp");
        assert!(org.members.is_empty());
        assert!(org.groups.is_empty());
    }

    #[test]
    fn test_organization_invalid_id() {
        assert!(Organization::new("BAD", "Bad Org").is_err());
    }

    #[test]
    fn test_organization_add_member() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_member("alice").unwrap();
        org.add_member("bob").unwrap();
        assert!(org.is_member("alice"));
        assert!(org.is_member("bob"));
        assert!(!org.is_member("charlie"));
    }

    #[test]
    fn test_organization_duplicate_member() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_member("alice").unwrap();
        assert!(org.add_member("alice").is_err());
    }

    #[test]
    fn test_organization_remove_member() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_member("alice").unwrap();
        assert!(org.remove_member("alice"));
        assert!(!org.is_member("alice"));
        assert!(!org.remove_member("alice")); // already removed
    }

    #[test]
    fn test_organization_add_group() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_group("backend").unwrap();
        assert!(org.groups.contains(&"backend".to_string()));
    }

    #[test]
    fn test_organization_duplicate_group() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_group("backend").unwrap();
        assert!(org.add_group("backend").is_err());
    }

    #[test]
    fn test_organization_remove_group() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_group("backend").unwrap();
        assert!(org.remove_group("backend"));
        assert!(!org.remove_group("backend"));
    }

    #[test]
    fn test_organization_display() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_member("alice").unwrap();
        org.add_member("bob").unwrap();
        assert_eq!(format!("{}", org), "Acme Corp (acme, 2 members)");
    }

    // -- Group -------------------------------------------------------------

    #[test]
    fn test_group_new() {
        let group = Group::new("reviewers", "Code Reviewers").unwrap();
        assert_eq!(group.id, "reviewers");
        assert_eq!(group.name, "Code Reviewers");
        assert!(group.members.is_empty());
    }

    #[test]
    fn test_group_invalid_id() {
        assert!(Group::new("BAD", "Bad Group").is_err());
    }

    #[test]
    fn test_group_add_member() {
        let mut group = Group::new("reviewers", "Reviewers").unwrap();
        group.add_member("alice").unwrap();
        assert!(group.is_member("alice"));
        assert_eq!(group.member_count(), 1);
    }

    #[test]
    fn test_group_duplicate_member() {
        let mut group = Group::new("reviewers", "Reviewers").unwrap();
        group.add_member("alice").unwrap();
        assert!(group.add_member("alice").is_err());
    }

    #[test]
    fn test_group_remove_member() {
        let mut group = Group::new("reviewers", "Reviewers").unwrap();
        group.add_member("alice").unwrap();
        assert!(group.remove_member("alice"));
        assert!(!group.is_member("alice"));
        assert_eq!(group.member_count(), 0);
    }

    #[test]
    fn test_group_display() {
        let mut group = Group::new("reviewers", "Code Reviewers").unwrap();
        group.add_member("alice").unwrap();
        assert_eq!(format!("{}", group), "Code Reviewers (reviewers, 1 members)");
    }

    // -- Serialization roundtrip -------------------------------------------

    #[test]
    fn test_identity_serde_roundtrip() {
        let identity = Identity::builder("alice")
            .email("alice@example.com").unwrap()
            .display_name("Alice")
            .signing_key_str("A1B2C3D4E5F6A1B2C3D4E5F6A1B2C3D4E5F6A1B2").unwrap()
            .build()
            .unwrap();

        let json = serde_json::to_string(&identity).unwrap();
        let deserialized: Identity = serde_json::from_str(&json).unwrap();

        assert_eq!(identity.id, deserialized.id);
        assert_eq!(identity.email, deserialized.email);
        assert_eq!(identity.signing_key, deserialized.signing_key);
        assert_eq!(identity.display_name, deserialized.display_name);
    }

    #[test]
    fn test_organization_serde_roundtrip() {
        let mut org = Organization::new("acme", "Acme Corp").unwrap();
        org.add_member("alice").unwrap();
        org.add_group("backend").unwrap();

        let json = serde_json::to_string(&org).unwrap();
        let deserialized: Organization = serde_json::from_str(&json).unwrap();

        assert_eq!(org.id, deserialized.id);
        assert_eq!(org.members, deserialized.members);
        assert_eq!(org.groups, deserialized.groups);
    }

    #[test]
    fn test_group_serde_roundtrip() {
        let mut group = Group::new("reviewers", "Reviewers").unwrap();
        group.add_member("alice").unwrap();

        let yaml = serde_yaml::to_string(&group).unwrap();
        let deserialized: Group = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(group.id, deserialized.id);
        assert_eq!(group.members, deserialized.members);
    }
}
