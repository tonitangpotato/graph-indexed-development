//! Validation extensions beyond the legacy `Validator` (graph-internal cycles
//! / orphan-edges / etc.). Drift detection (ISS-059) lives here under
//! `validate::drift`. Future ledger / commit-linkage layers (ISS-060) will
//! attach to this module too.
pub mod drift;
