use std::fmt;

/// Categories of storage operations, used for error context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageOp {
    Open,
    Read,
    Write,
    Delete,
    Search,
    Migrate,
    Snapshot,
}

impl fmt::Display for StorageOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageOp::Open => write!(f, "open"),
            StorageOp::Read => write!(f, "read"),
            StorageOp::Write => write!(f, "write"),
            StorageOp::Delete => write!(f, "delete"),
            StorageOp::Search => write!(f, "search"),
            StorageOp::Migrate => write!(f, "migrate"),
            StorageOp::Snapshot => write!(f, "snapshot"),
        }
    }
}

/// Errors originating from the storage layer.
#[derive(Debug)]
pub enum StorageError {
    /// I/O error (file system, path issues).
    Io {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// SQLite-level error (query failure, constraint violation, busy, etc.).
    Sqlite {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Requested entity (node, edge, metadata key) was not found.
    NotFound {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Attempted to create an entity that already exists.
    AlreadyExists {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Schema migration failure.
    Migration {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Data could not be deserialized or is otherwise malformed.
    InvalidData {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Detected database corruption (integrity check failure, unexpected state).
    Corruption {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Database is locked by another writer (SQLITE_BUSY after timeout).
    /// GOAL-1.17: write contention handling.
    DatabaseLocked {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Foreign key constraint violation (SQLITE_CONSTRAINT_FOREIGNKEY).
    /// ISS-015: Distinguish FK from other constraints.
    ForeignKeyViolation {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Unique constraint violation (SQLITE_CONSTRAINT_UNIQUE).
    /// ISS-015: Distinguish UNIQUE from other constraints.
    UniqueViolation {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Check constraint violation (SQLITE_CONSTRAINT_CHECK).
    /// ISS-015: Distinguish CHECK from other constraints.
    CheckViolation {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Not-null constraint violation (SQLITE_CONSTRAINT_NOTNULL).
    /// ISS-015: Distinguish NOT NULL from other constraints.
    NotNullViolation {
        op: StorageOp,
        detail: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Schema version mismatch between expected and found versions.
    SchemaMismatch {
        expected: String,
        found: String,
    },
}

impl StorageError {
    /// Helper to extract the operation from any variant.
    pub fn op(&self) -> StorageOp {
        match self {
            StorageError::Io { op, .. }
            | StorageError::Sqlite { op, .. }
            | StorageError::NotFound { op, .. }
            | StorageError::AlreadyExists { op, .. }
            | StorageError::Migration { op, .. }
            | StorageError::InvalidData { op, .. }
            | StorageError::Corruption { op, .. }
            | StorageError::DatabaseLocked { op, .. }
            | StorageError::ForeignKeyViolation { op, .. }
            | StorageError::UniqueViolation { op, .. }
            | StorageError::CheckViolation { op, .. }
            | StorageError::NotNullViolation { op, .. } => *op,
            StorageError::SchemaMismatch { .. } => StorageOp::Migrate,
        }
    }

    /// Helper to extract the detail message from any variant.
    pub fn detail(&self) -> &str {
        match self {
            StorageError::Io { detail, .. }
            | StorageError::Sqlite { detail, .. }
            | StorageError::NotFound { detail, .. }
            | StorageError::AlreadyExists { detail, .. }
            | StorageError::Migration { detail, .. }
            | StorageError::InvalidData { detail, .. }
            | StorageError::Corruption { detail, .. }
            | StorageError::DatabaseLocked { detail, .. }
            | StorageError::ForeignKeyViolation { detail, .. }
            | StorageError::UniqueViolation { detail, .. }
            | StorageError::CheckViolation { detail, .. }
            | StorageError::NotNullViolation { detail, .. } => detail,
            StorageError::SchemaMismatch { expected, .. } => expected,
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io { op, detail, .. } => {
                write!(f, "storage I/O error during {op}: {detail}")
            }
            StorageError::Sqlite { op, detail, .. } => {
                write!(f, "SQLite error during {op}: {detail}")
            }
            StorageError::NotFound { op, detail, .. } => {
                write!(f, "not found during {op}: {detail}")
            }
            StorageError::AlreadyExists { op, detail, .. } => {
                write!(f, "already exists during {op}: {detail}")
            }
            StorageError::Migration { op, detail, .. } => {
                write!(f, "migration error during {op}: {detail}")
            }
            StorageError::InvalidData { op, detail, .. } => {
                write!(f, "invalid data during {op}: {detail}")
            }
            StorageError::Corruption { op, detail, .. } => {
                write!(f, "corruption detected during {op}: {detail}")
            }
            StorageError::DatabaseLocked { op, detail, .. } => {
                write!(f, "database is locked during {op}: {detail}")
            }
            StorageError::ForeignKeyViolation { op, detail, .. } => {
                write!(f, "foreign key violation during {op}: {detail}")
            }
            StorageError::UniqueViolation { op, detail, .. } => {
                write!(f, "unique constraint violation during {op}: {detail}")
            }
            StorageError::CheckViolation { op, detail, .. } => {
                write!(f, "check constraint violation during {op}: {detail}")
            }
            StorageError::NotNullViolation { op, detail, .. } => {
                write!(f, "not-null constraint violation during {op}: {detail}")
            }
            StorageError::SchemaMismatch { expected, found } => {
                write!(f, "schema version mismatch: expected {expected}, found {found}")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let src = match self {
            StorageError::Io { source, .. }
            | StorageError::Sqlite { source, .. }
            | StorageError::NotFound { source, .. }
            | StorageError::AlreadyExists { source, .. }
            | StorageError::Migration { source, .. }
            | StorageError::InvalidData { source, .. }
            | StorageError::Corruption { source, .. }
            | StorageError::DatabaseLocked { source, .. }
            | StorageError::ForeignKeyViolation { source, .. }
            | StorageError::UniqueViolation { source, .. }
            | StorageError::CheckViolation { source, .. }
            | StorageError::NotNullViolation { source, .. } => source,
            StorageError::SchemaMismatch { .. } => return None,
        };
        src.as_ref().map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for StorageError {
    fn from(err: std::io::Error) -> Self {
        StorageError::Io {
            op: StorageOp::Read,
            detail: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(err: serde_json::Error) -> Self {
        StorageError::InvalidData {
            op: StorageOp::Read,
            detail: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

/// Convenience alias for storage results.
pub type StorageResult<T> = Result<T, StorageError>;
