pub mod error;
pub mod trait_def;
pub mod schema;

// Re-export key types for convenience.
pub use error::{StorageError, StorageOp, StorageResult};
pub use trait_def::{BatchOp, GraphStorage, NodeFilter};
pub use schema::SCHEMA_SQL;
