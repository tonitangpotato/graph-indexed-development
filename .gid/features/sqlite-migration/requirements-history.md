# Requirements: History System on SQLite

## Feature Overview

The history system provides snapshot-based copies of the entire graph database, enabling users to save, list, compare, and restore previous states. Instead of copying YAML files (the current approach), it uses SQLite's backup API to snapshot the entire database to `.gid/history/{timestamp}.db`, giving instant, consistent snapshots with full queryability of historical data.

*Parent: [requirements.md](requirements.md) — see GUARDs there for cross-cutting constraints.*

## Goals

### Saving Snapshots

- **GOAL-3.1** [P0]: Running `gid history save` (with optional `--message "description"`) creates a complete copy of `.gid/graph.db` at `.gid/history/{ISO-8601-timestamp}.db` using rusqlite's `backup` method (wrapping SQLite's online backup API) to ensure a consistent snapshot even if reads are in progress. The snapshot is a fully functional SQLite database that can be opened independently. *(ref: discussion, History System — snapshot-based, SQLite backup API)*

- **GOAL-3.2** [P1]: The snapshot filename uses a filesystem-safe ISO 8601 format (e.g., `2026-04-06T19-40-12Z.db`). Snapshot metadata is stored in `.gid/history/index.json` (not inside each snapshot DB) containing: timestamp, message (if provided), node count, edge count, and current git commit hash (if inside a git repo). The `index.json` is the authoritative source for listing snapshots (GOAL-3.4). If `index.json` is missing or corrupt, it is rebuilt by scanning snapshot files and reading their metadata. *(ref: discussion, History System — snapshot metadata)*

- **GOAL-3.3** [P1]: The history directory retains a maximum of 50 snapshots. When saving a new snapshot would exceed this limit, the oldest snapshots are deleted until exactly 50 remain (including the new one). If more than 50 snapshots already exist (e.g., from manual copying), extras are pruned on the next save. The limit is hardcoded at 50 in v1. Auto-save snapshots from restore operations (GOAL-3.5) count against this limit. *(ref: existing code, history.rs — MAX_HISTORY_ENTRIES: usize = 50)*

### Listing Snapshots

- **GOAL-3.4** [P0]: Running `gid history list` displays all available snapshots in reverse chronological order, showing: timestamp, message (or "—" if none), node count, and edge count. *(ref: discussion, History System — gid history list)*

### Restoring Snapshots

- **GOAL-3.5** [P0]: Running `gid history restore {timestamp}` first auto-saves the current state as a new snapshot (with message "auto-save before restore"), then replaces `.gid/graph.db` with the specified snapshot. The auto-save step verifies that the target snapshot is not the oldest snapshot that would be deleted by the 50-snapshot limit (GOAL-3.3) before proceeding; if it would be, the limit is temporarily exceeded by one to preserve the restore target. After restore, all `gid` commands operate on the restored data. *(ref: discussion, History System — restore auto-saves current state first)*

- **GOAL-3.6** [P1]: If the specified timestamp does not match any existing snapshot, `gid history restore` prints an error listing available snapshots and exits with non-zero status. *(ref: discussion, History System — error handling)*

### Comparing Snapshots

- **GOAL-3.7** [P0]: Running `gid history diff {timestamp}` compares the specified snapshot against the current database and reports: nodes added, nodes removed, nodes modified (any column change), edges added, and edges removed — with counts and up to 10 example IDs per category in CLI output. The library function (`gid-core`) returns the full lists without truncation. *(ref: discussion, History System — diff uses ATTACH to compare two DBs)*

- **GOAL-3.8** [P2]: Running `gid history diff {timestamp1} {timestamp2}` compares two historical snapshots against each other (neither needs to be the current state). *(ref: discussion, History System — diff between any two snapshots)*

### Observability

- **GOAL-3.9** [P1]: `gid history save` logs elapsed time and snapshot file size to stderr. `gid history restore` logs the snapshot being restored and elapsed time. `gid history diff` logs traversal statistics (nodes compared, edges compared) to stderr. *(ref: review FINDING-16, observability)*

**9 GOALs** (4 P0 / 4 P1 / 1 P2)
