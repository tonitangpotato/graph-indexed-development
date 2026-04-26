//! Telemetry — append-only JSONL event logging and statistics.
//!
//! All execution events are logged to a JSONL file for crash recovery,
//! debugging, and post-execution statistics. The format is append-only
//! to prevent data loss on crash (GUARD-8).

use std::io::Write;
use std::path::PathBuf;
use anyhow::{Context, Result};
use tracing::debug;

use super::types::{ExecutionEvent, ExecutionStats};

/// Append-only JSONL telemetry logger.
///
/// Events are written immediately to disk (no buffering) to ensure
/// no data loss on crash (GUARD-8). Each line is a complete JSON object.
///
/// Default log path: `.gid/execution-log.jsonl`
pub struct TelemetryLogger {
    /// Path to the JSONL log file.
    pub log_path: PathBuf,
}

impl TelemetryLogger {
    /// Create a new telemetry logger writing to the given path.
    pub fn new(log_path: impl Into<PathBuf>) -> Self {
        Self {
            log_path: log_path.into(),
        }
    }

    /// Log an execution event (append to JSONL file).
    ///
    /// Each event is serialized as a single JSON line and flushed immediately.
    pub fn log_event(&self, event: &ExecutionEvent) -> Result<()> {
        let json = serde_json::to_string(event)
            .context("Failed to serialize execution event")?;

        debug!(event = %json, "Logging telemetry event");

        // Ensure parent directory exists
        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .context("Failed to open telemetry log")?;

        writeln!(file, "{}", json)
            .context("Failed to write telemetry event")?;

        file.flush()
            .context("Failed to flush telemetry log")?;

        Ok(())
    }

    /// Compute execution statistics from the log file.
    ///
    /// Reads all events and aggregates: tasks completed/failed,
    /// total turns/tokens, duration, and estimation accuracy.
    pub fn compute_stats(&self) -> Result<ExecutionStats> {
        let content = std::fs::read_to_string(&self.log_path)
            .context("Failed to read telemetry log")?;

        let mut tasks_completed: usize = 0;
        let mut tasks_failed: usize = 0;
        let mut total_turns: u32 = 0;
        let mut total_tokens: u64 = 0;
        let mut total_duration_s: u64 = 0;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Ok(event) = serde_json::from_str::<ExecutionEvent>(line) {
                match event {
                    ExecutionEvent::TaskDone { turns, tokens, duration_s, .. } => {
                        tasks_completed += 1;
                        total_turns += turns;
                        total_tokens += tokens;
                        total_duration_s += duration_s;
                    }
                    ExecutionEvent::TaskFailed { turns, .. } => {
                        tasks_failed += 1;
                        total_turns += turns;
                    }
                    ExecutionEvent::Complete { duration_s, .. } => {
                        // Use the overall duration from complete event if available
                        total_duration_s = duration_s;
                    }
                    _ => {}
                }
            }
        }

        let task_count = tasks_completed + tasks_failed;
        let avg_turns = if task_count > 0 {
            total_turns as f32 / task_count as f32
        } else {
            0.0
        };

        Ok(ExecutionStats {
            tasks_completed,
            tasks_failed,
            total_turns,
            avg_turns_per_task: avg_turns,
            total_tokens,
            duration_secs: total_duration_s,
        })
    }

    /// Read all events from the log file.
    ///
    /// Useful for crash recovery — inspect what happened before the crash.
    pub fn read_events(&self) -> Result<Vec<ExecutionEvent>> {
        let content = std::fs::read_to_string(&self.log_path)
            .unwrap_or_default();

        let mut events = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<ExecutionEvent>(line) {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Clear the log file (for testing or fresh runs).
    pub fn clear(&self) -> Result<()> {
        if self.log_path.exists() {
            std::fs::write(&self.log_path, "")
                .context("Failed to clear telemetry log")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_log_and_read_events() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("test.jsonl");
        let logger = TelemetryLogger::new(&log_path);

        // Log a plan event
        let plan_event = ExecutionEvent::Plan {
            total_tasks: 5,
            layers: 3,
            timestamp: Utc::now(),
        };
        logger.log_event(&plan_event).unwrap();

        // Log a task start
        let start_event = ExecutionEvent::TaskStart {
            task_id: "auth".to_string(),
            layer: 0,
            timestamp: Utc::now(),
        };
        logger.log_event(&start_event).unwrap();

        // Log a task done
        let done_event = ExecutionEvent::TaskDone {
            task_id: "auth".to_string(),
            turns: 12,
            tokens: 15000,
            duration_s: 90,
            verify: "pass".to_string(),
            timestamp: Utc::now(),
        };
        logger.log_event(&done_event).unwrap();

        // Read back
        let events = logger.read_events().unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_compute_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("stats.jsonl");
        let logger = TelemetryLogger::new(&log_path);

        // Log some events
        logger.log_event(&ExecutionEvent::Plan {
            total_tasks: 3,
            layers: 2,
            timestamp: Utc::now(),
        }).unwrap();

        logger.log_event(&ExecutionEvent::TaskDone {
            task_id: "a".to_string(),
            turns: 10,
            tokens: 5000,
            duration_s: 60,
            verify: "pass".to_string(),
            timestamp: Utc::now(),
        }).unwrap();

        logger.log_event(&ExecutionEvent::TaskDone {
            task_id: "b".to_string(),
            turns: 20,
            tokens: 8000,
            duration_s: 120,
            verify: "pass".to_string(),
            timestamp: Utc::now(),
        }).unwrap();

        logger.log_event(&ExecutionEvent::TaskFailed {
            task_id: "c".to_string(),
            reason: "verify failed".to_string(),
            turns: 5,
            timestamp: Utc::now(),
        }).unwrap();

        let stats = logger.compute_stats().unwrap();
        assert_eq!(stats.tasks_completed, 2);
        assert_eq!(stats.tasks_failed, 1);
        assert_eq!(stats.total_turns, 35);
        assert_eq!(stats.total_tokens, 13000);
        assert!((stats.avg_turns_per_task - 11.67).abs() < 0.1);
    }

    #[test]
    fn test_empty_log_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("empty.jsonl");
        std::fs::write(&log_path, "").unwrap();

        let logger = TelemetryLogger::new(&log_path);
        let stats = logger.compute_stats().unwrap();
        assert_eq!(stats.tasks_completed, 0);
        assert_eq!(stats.tasks_failed, 0);
        assert_eq!(stats.total_turns, 0);
    }

    #[test]
    fn test_clear_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("clear.jsonl");
        let logger = TelemetryLogger::new(&log_path);

        logger.log_event(&ExecutionEvent::Plan {
            total_tasks: 1,
            layers: 1,
            timestamp: Utc::now(),
        }).unwrap();

        assert!(!std::fs::read_to_string(&log_path).unwrap().is_empty());

        logger.clear().unwrap();
        assert_eq!(std::fs::read_to_string(&log_path).unwrap(), "");
    }

    #[test]
    fn test_append_only() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("append.jsonl");
        let logger = TelemetryLogger::new(&log_path);

        // Write two events
        logger.log_event(&ExecutionEvent::Plan {
            total_tasks: 1,
            layers: 1,
            timestamp: Utc::now(),
        }).unwrap();

        logger.log_event(&ExecutionEvent::Plan {
            total_tasks: 2,
            layers: 2,
            timestamp: Utc::now(),
        }).unwrap();

        // Both should be present
        let content = std::fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "JSONL should have 2 lines (append-only)");
    }
}
