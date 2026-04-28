# ISS-052 + ISS-051 + rustclaw/ISS-051 + rustclaw/ISS-050 — Unified Design

> **Driver issue:** gid-rs ISS-052 (rustclaw RitualRunner bypasses V2Executor)
>
> **Bundles:**
> - gid-rs ISS-052 — rustclaw has parallel dispatcher (P0)
> - gid-rs ISS-051 — `save_state` IO failure silent wedge (P1)
> - rustclaw ISS-051 — file_snapshot post-condition never runs in production (P0)
> - rustclaw ISS-050 — ritual wedges silently when save_state hits IO error (P1)
>
> **Root cause (single):** rustclaw maintains its own `RitualAction` dispatcher (`src/ritual_runner.rs`, ~2954 LOC, two parallel match blocks at L575 and L813) that duplicates `gid-core::V2Executor::execute()` (`v2_executor.rs:202`). Quality gates added to V2Executor (file_snapshot, ISS-038) never run in production. `save_state` is called from at least 7 places across both repos with no unified failure semantics.
>
> **Root fix (single):** introduce `RitualHooks` trait in gid-core; make V2Executor the *only* dispatcher; rustclaw becomes a thin hooks impl. Save_state failures become first-class state-machine events. Self-review loop bug fixed in same change.

---

## §1. Goals & Non-Goals

### Goals

- **G1.** Eliminate duplicate `RitualAction` dispatch. There must be exactly **one** code path that turns a `RitualAction` into an effect + `RitualEvent`.
- **G2.** Make `file_snapshot` post-condition (ISS-038) actually run in production rustclaw rituals. Today it does not run at all; ISS-038's gate is dead code on the prod path.
- **G3.** Make `save_state` IO failures first-class state-machine events. No silent swallow, no untyped `unwrap`, no log-and-continue.
- **G4.** Make rustclaw's adapter responsibilities (Telegram notify, daemon PID stamping, on-disk persistence path conventions, optional Engram side-writes) pluggable via `RitualHooks` without giving rustclaw the ability to skip gates.
- **G5.** Fix self-review loop turn-limit-equals-pass bug (rustclaw ISS-051 §"Self-review loop fixes") in the same change, since it lives in the same file and is the last line of defense behind file_snapshot.
- **G6.** Net LOC reduction: target **−1500 LOC in rustclaw, +400 LOC in gid-core**. Net −1100. (Rough; measured in §10.)

### Non-Goals

- **NG1.** Not rewriting the `RitualState` state machine itself. The phase enum (`Idle`, `Implementing`, `Verifying`, …) stays as-is and no new top-level phases are added. Persist-degradation is modelled as a *side-channel flag on `RitualState`* (see §6.3), **not** as a new wrapping phase, so the existing transition table is unchanged.
- **NG2.** Not changing the on-disk JSON format of ritual state files. Migrations are out of scope.
- **NG3.** Not redesigning skill loading / prompt composition. `Composer`, `template.rs`, skills layout untouched.
- **NG4.** Not touching ISS-053 (artifact-as-first-class). Artifact work proceeds independently. We may share a release window (§10) but no API coupling.
- **NG5.** Not introducing async hooks beyond what V2Executor already needs. The trait uses `#[async_trait]` only on the IO-shaped methods (`notify`, `persist_state`) where the embedder *already* needs `.await` (Telegram send, async file write); pure-data methods (`resolve_workspace`, `stamp_metadata`, `should_cancel`, `on_action_start/finish`, `on_phase_transition`) stay synchronous and return immediately. No new async runtime is required of the hooks impl beyond the runtime V2Executor itself runs on. The `async_trait` macro overhead (boxed futures) is accepted on the two async methods only.

---

## §2. Current State (forensic)

### 2.1 Three parallel dispatchers exist today

**gid-core `V2Executor::execute()`** — `crates/gid-core/src/ritual/v2_executor.rs:202`

```rust
pub async fn execute(&self, action: &RitualAction, state: &RitualState) -> Option<RitualEvent> {
    match action {
        RitualAction::DetectProject => Some(self.detect_project().await),
        RitualAction::RunTriage { task } => Some(self.run_triage(task, state).await),
        RitualAction::RunSkill { name, context } => Some(self.run_skill(name, context, state).await),
        RitualAction::RunShell { command } => Some(self.run_shell(command).await),
        RitualAction::RunPlanning => Some(self.run_planning(state).await),
        RitualAction::RunHarness { tasks } => Some(self.run_harness(tasks, state).await),
        RitualAction::Notify { message } => { /* default no-op */ None }
        RitualAction::SaveState => { self.save_state(state); None }
        RitualAction::UpdateGraph { description } => …
        RitualAction::ApplyReview { approved } => …
        RitualAction::Cleanup => { … None }
    }
}
```

This is the *intended* dispatcher. It is the one that runs `file_snapshot` diff inside `run_skill` (L374, ISS-038). It is the one called by `run_ritual` (L1281) — which is exercised only by gid-core integration tests.

**rustclaw `RitualRunner` dispatcher #1** — `src/ritual_runner.rs:575`

```rust
match action {
    RitualAction::DetectProject => { /* rustclaw-local impl */ }
    RitualAction::RunSkill { name, context } => { /* rustclaw-local impl */ }
    RitualAction::RunShell { command } => { /* rustclaw-local impl */ }
    RitualAction::RunTriage { task } => { /* rustclaw-local impl */ }
    RitualAction::RunPlanning => { /* rustclaw-local impl */ }
    RitualAction::RunHarness { tasks } => { /* rustclaw-local impl */ }
    /* + Notify / SaveState / UpdateGraph / Cleanup / ApplyReview branches */
}
```

**rustclaw `RitualRunner` dispatcher #2** — `src/ritual_runner.rs:813`

A second, *near-identical* `match action { … }` block. Same variants, slightly diverged implementations. Almost certainly one was copy-pasted from the other and they drifted. **Either one is enough to bypass V2Executor entirely; having two is just extra rope.**

Neither rustclaw dispatcher calls `V2Executor::execute()`. Neither runs `file_snapshot` post-condition. Neither runs the `run_skill` body that contains the ISS-038 diff check.

### 2.2 `save_state` call graph

`save_state` is called from **at least 7 sites** across the two repos:

| File | Line | Failure handling |
|---|---|---|
| `gid-core/.../v2_executor.rs` | 731 | returns `()`; caller can't tell |
| `gid-core/.../v2_executor.rs` | 217 (`SaveState` dispatch) | result swallowed |
| `rustclaw/src/ritual_runner.rs` | 225 | returns `Result<()>`; ok |
| `rustclaw/src/ritual_runner.rs` | 256 | `?` propagates — but to what? caller logs and continues |
| `rustclaw/src/ritual_runner.rs` | 292 | same |
| `rustclaw/src/ritual_runner.rs` | 368 | same |
| `rustclaw/src/ritual_runner.rs` | 490 | `let _ = runner.save_state(...)` — **explicitly** ignored |
| `rustclaw/src/ritual_runner.rs` | 607 | logs error, continues to next action |
| `rustclaw/src/ritual_runner.rs` | 1409 | `?` propagates |

Two failure modes coexist: gid-core swallows, rustclaw mostly propagates-but-callers-ignore. There is no `RitualEvent` variant that represents "we failed to durably record progress." Disk-full leaves the in-memory state correct and the on-disk file stale → next process load resumes from earlier state → silent rollback.

### 2.3 Forensic evidence — ritual `r-950ebf` (engram ISS-028+029)

This ritual is the canary that surfaced both bugs:

- **Symptom A (Bug B / ISS-052):** ritual transitioned `Implementing → Verifying` and finally `Done`, but `git status` in the engram repo showed zero changes from the ritual's `implement` phase. The skill claim in `state.json` recorded `success: true`. ISS-038's `file_snapshot` diff would have caught this — but ISS-038 lives in V2Executor, and V2Executor was never called.
- **Symptom B (Bug A / ISS-051):** during the same ritual, two state writes between phases left the on-disk file **older than** the latest event in the in-memory log. No error was logged. (Hypothesis: transient FS lag or a previous `save_state` that hit an error swallowed at L607.)
- **Symptom C (rustclaw ISS-051 self-review):** the `review-design` self-review subloop ran 4 turns, each hit the LLM turn limit without producing a verdict, and the loop terminated in `accept` state. Same file, same dispatcher, no gate.

Three independent symptoms, one ritual run, all rooted in `ritual_runner.rs` taking dispatch into its own hands.

### 2.4 Why this happened (history, not blame)

rustclaw's `ritual_runner.rs` predates V2Executor's `execute()` becoming public. When V2Executor grew its own dispatch path (for gid-core integration tests), rustclaw kept its own to preserve adapter behavior — Telegram notifications, daemon-PID stamping, custom workspace path resolution. Nobody noticed that *every quality gate added to V2Executor after that point silently stopped applying to production*. ISS-038 (file_snapshot, 2026-04-something) is the cleanest example: ~200 LOC + tests, none of it on the live path.

This is a textbook "two implementations of the same interface drift" failure.

## §3. Target Architecture

### 3.1 One dispatcher rule

```
┌─────────────────────────────────────────────────────────────┐
│  rustclaw process                                           │
│  ┌──────────────────────────────────────────┐               │
│  │ RitualRunner (thin)                      │               │
│  │  - owns RitualState lifecycle            │               │
│  │  - owns persistence path                 │               │
│  │  - owns Telegram channel                 │               │
│  │  - constructs V2Executor with hooks      │               │
│  │                                          │               │
│  │  loop {                                  │               │
│  │    actions = state_machine.transition(); │               │
│  │    for action in actions {               │               │
│  │      event = executor.execute(           │               │
│  │        &action, &state                   │               │
│  │      ).await;                            │               │
│  │      state.apply(event);                 │               │
│  │    }                                     │               │
│  │  }                                       │               │
│  └──────────────┬───────────────────────────┘               │
│                 │ injected as &dyn RitualHooks              │
│                 ▼                                           │
│  ┌──────────────────────────────────────────┐               │
│  │ RustclawHooks: RitualHooks               │               │
│  │  - notify(msg) → Telegram                │               │
│  │  - persist_state(state) → ritual JSON    │               │
│  │  - workspace_root() → resolved via reg   │               │
│  │  - on_skill_started / on_skill_finished  │               │
│  │  - …                                     │               │
│  └──────────────┬───────────────────────────┘               │
└─────────────────┼───────────────────────────────────────────┘
                  │ called by V2Executor at well-defined points
                  ▼
┌─────────────────────────────────────────────────────────────┐
│  gid-core::ritual::V2Executor                               │
│   - the ONE dispatcher: execute(action, state) → event      │
│   - owns run_skill, run_shell, run_triage, run_planning,    │
│     run_harness, file_snapshot diff, gating, scope          │
│   - calls hooks.* at side-effect boundaries                 │
│   - never directly touches Telegram, daemon PID, or disk    │
│     persistence path conventions                            │
└─────────────────────────────────────────────────────────────┘
```

**Invariant:** if a `RitualAction` becomes a `RitualEvent`, the transformation went through `V2Executor::execute`. Period. There is no rustclaw-side branch.

### 3.2 What rustclaw still owns

These are **adapter concerns**, not dispatch concerns:

- **Persistence path resolution.** `~/.gid/runtime/rituals/<id>.json` vs project-local — rustclaw decides. Hook: `persist_state(&state)`.
- **Channel side effects.** `Notify { message }` → Telegram. Hook: `notify(msg)`.
- **Workspace resolution.** rustclaw's project registry (`~/.config/gid/projects.yml`). Hook: `resolve_workspace(&work_unit) -> PathBuf`.
- **Daemon metadata.** `adapter_pid`, daemon-uuid stamping. Hook: `stamp_metadata(&mut state)`.
- **Cancellation signal source.** rustclaw watches its own `ritual cancel` Telegram command. Hook: `should_cancel() -> bool` (polled between actions).
- **Engram side-writes.** Optional. rustclaw may persist ritual milestones to Engram. Hook: `on_phase_transition(from, to)`.

### 3.3 What V2Executor owns (and rustclaw must NOT do)

- All `RitualAction` → `RitualEvent` translation.
- `run_skill` body, including LLM call, prompt composition, file_snapshot pre/post.
- Self-review subloop body (currently buggy — see §8).
- The decision "skill claimed success but produced zero file changes → override to `SkillFailed`."
- The decision "self-review subloop ran out of turns → fail."
- `save_state` invocation (delegated to hook, but invoked from V2Executor).

If rustclaw needs to influence any of the above, it does so via hook callbacks, **not** by reimplementing.

### 3.4 Why a trait, not a callback struct or message channel

Considered alternatives:

- **Callback struct (FnMut closures).** Verbose to construct (10+ closures). Hard to share state between callbacks (every closure needs its own `Arc<Mutex<…>>`). Rejected.
- **mpsc channel** (V2Executor sends `AdapterRequest` enum, rustclaw drives loop). Inverts control flow — V2Executor would block waiting for replies. Adds deadlock surface. Rejected.
- **Trait object (`&dyn RitualHooks`).** Standard Rust pattern. Easy to mock in tests (`MockHooks`). Rustclaw impl can hold `Arc<TelegramClient>`, `Arc<RitualPersister>` as fields. **Chosen.**

---

## §4. `RitualHooks` Trait — Full Signature

Define in **new file** `crates/gid-core/src/ritual/hooks.rs`. Re-export from `ritual::mod`.

```rust
use std::path::PathBuf;
use async_trait::async_trait;

use super::{RitualState, RitualEvent, WorkUnit};

/// Adapter hooks invoked by `V2Executor` at side-effect boundaries.
///
/// Implementations are owned by the embedder (rustclaw, gid CLI, tests) and
/// passed to `V2Executor` as `Arc<dyn RitualHooks>`.
///
/// # Threading
/// V2Executor calls hooks from its async task. Implementations must be
/// `Send + Sync`. Methods are async only where the embedder is likely to
/// perform IO (notify, persist); pure-data methods stay sync.
///
/// # Failure semantics
/// - `notify` failure: logged by V2Executor, ritual continues. Notifications
///   are best-effort.
/// - `persist_state` failure: V2Executor emits `StatePersistFailed` event;
///   state machine handles via §6 retry/escalate logic. Hooks MUST return
///   the underlying IO error rather than swallowing.
/// - `resolve_workspace` failure: ritual aborts with `WorkspaceUnresolved`
///   event. No retry — config error, not transient.
#[async_trait]
pub trait RitualHooks: Send + Sync {
    // ── Side-effect channels ───────────────────────────────────────────

    /// Send a user-facing notification (e.g. Telegram message, log line).
    /// Best-effort. Errors logged but do not fail the ritual.
    async fn notify(&self, message: &str);

    /// Durably persist ritual state. MUST return error on IO failure.
    /// V2Executor turns errors into `StatePersistFailed` events.
    ///
    /// # Atomicity contract
    /// Implementations MUST write atomically (write-to-tempfile + `rename(2)`,
    /// or equivalent platform primitive). Partial writes are a contract
    /// violation: if the function returns `Err`, on-disk state must be either
    /// the previous successful version or absent — never a truncated or
    /// half-serialized blob. V2Executor's retry loop (§6.2) overwrites on
    /// success, so non-atomic implementations would corrupt the file across
    /// retries (attempt 1 partial → attempt 2 different partial → attempt 3
    /// fail → on-disk file is unparseable, ritual unrecoverable).
    async fn persist_state(&self, state: &RitualState) -> std::io::Result<()>;

    // ── Resolution / configuration ─────────────────────────────────────

    /// Resolve the project workspace root for this work unit.
    /// Called once at ritual start. Errors abort the ritual.
    fn resolve_workspace(&self, work_unit: &WorkUnit) -> Result<PathBuf, WorkspaceError>;

    /// Stamp adapter-specific metadata into state at start
    /// (e.g. daemon PID, hostname, adapter version).
    /// Called once on transition into the first non-Idle state.
    fn stamp_metadata(&self, state: &mut RitualState);

    // ── Lifecycle observation ──────────────────────────────────────────

    /// Called immediately before each `RitualAction` is dispatched.
    /// Default: no-op. Used for tracing, metrics, Engram side-writes.
    fn on_action_start(&self, _action: &RitualAction, _state: &RitualState) {}

    /// Called immediately after the action's `RitualEvent` is produced
    /// (before it is applied to state). Default: no-op.
    fn on_action_finish(&self, _action: &RitualAction, _event: &RitualEvent) {}

    /// Called on every state phase transition (e.g. Implementing → Verifying).
    /// Default: no-op. Receives only the phase enum (not full `RitualState`)
    /// because most embedders only care about transition shape, not state
    /// payload, and a `&Phase` is `Copy`-cheap to pass.
    fn on_phase_transition(&self, _from: &Phase, _to: &Phase) {}

    // ── Cancellation ───────────────────────────────────────────────────

    /// Polled between actions. Return `true` to request cooperative
    /// cancellation. Default: never cancel.
    ///
    /// Cancellation produces a `Cancelled { reason }` event; ritual
    /// transitions to terminal Cancelled state, runs Cleanup action,
    /// persists, exits.
    ///
    /// # Latency
    /// Cancellation is polled **between** actions, not within them. A single
    /// `RunSkill` action can run for the duration of the LLM skill timeout
    /// (typically up to 10 minutes). Therefore the worst-case observed
    /// latency from `/ritual cancel` to ritual termination is bounded by the
    /// longest in-flight action. This is an accepted trade-off for the
    /// simpler poll model; mid-action cancellation (e.g. interrupting an
    /// in-progress LLM turn) is out of scope for this design and tracked as
    /// a follow-up if it surfaces as a real UX problem.
    fn should_cancel(&self) -> Option<CancelReason> { None }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkspaceError {
    #[error("project not found in registry: {0}")]
    NotFound(String),
    #[error("registry read failed: {0}")]
    RegistryError(String),
    #[error("path does not exist: {0}")]
    PathMissing(PathBuf),
}

#[derive(Debug, Clone)]
pub struct CancelReason {
    pub source: CancelSource,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSource {
    UserCommand,    // /ritual cancel
    Timeout,        // adapter-imposed wall-clock limit
    DaemonShutdown, // host process is exiting
}
```

### 4.1 Test double: `NoopHooks`

Ship in same module, `#[cfg(test)]` AND public for downstream test use:

```rust
/// No-op hooks for gid-core integration tests.
/// All notifications discarded, persist writes to a tempdir, workspace
/// resolved to a passed-in path.
pub struct NoopHooks {
    pub workspace: PathBuf,
    pub persist_dir: PathBuf,
    pub notifications: Mutex<Vec<String>>,
    pub cancel_requested: AtomicBool,
}
```

### 4.2 Test double: `FailingPersistHooks`

For §9 testing — exercises §6 save_state failure paths:

```rust
pub struct FailingPersistHooks {
    pub fail_after_n_calls: AtomicUsize,
    pub call_count: AtomicUsize,
    pub workspace: PathBuf,
}
```

## §5. V2Executor Changes

### 5.1 Constructor takes hooks

```rust
// Before
pub struct V2Executor {
    config: V2ExecutorConfig,
    // …
}
impl V2Executor {
    pub fn new(config: V2ExecutorConfig) -> Self { … }
}

// After
pub struct V2Executor {
    config: V2ExecutorConfig,
    hooks: Arc<dyn RitualHooks>,
}
impl V2Executor {
    pub fn new(config: V2ExecutorConfig, hooks: Arc<dyn RitualHooks>) -> Self { … }
}
```

Existing internal tests construct with `Arc::new(NoopHooks::for_tempdir())`.

### 5.2 `execute()` invokes hooks at boundaries

```rust
pub async fn execute(&self, action: &RitualAction, state: &RitualState) -> Option<RitualEvent> {
    self.hooks.on_action_start(action, state);

    if let Some(reason) = self.hooks.should_cancel() {
        return Some(RitualEvent::Cancelled { reason });
    }

    let event = match action {
        RitualAction::DetectProject => Some(self.detect_project().await),
        RitualAction::RunTriage { task } => Some(self.run_triage(task, state).await),
        RitualAction::RunSkill { name, context } => Some(self.run_skill(name, context, state).await),
        RitualAction::RunShell { command } => Some(self.run_shell(command).await),
        RitualAction::RunPlanning => Some(self.run_planning(state).await),
        RitualAction::RunHarness { tasks } => Some(self.run_harness(tasks, state).await),
        RitualAction::Notify { message } => {
            self.hooks.notify(message).await;
            None  // Notify produces no state event
        }
        RitualAction::SaveState => Some(self.persist_state(state).await),  // §6
        RitualAction::UpdateGraph { description } => Some(self.update_graph(description, state).await),
        RitualAction::ApplyReview { approved } => Some(self.apply_review(approved, state).await),
        RitualAction::Cleanup => { self.cleanup(state).await; None }
    };

    if let Some(ref ev) = event {
        self.hooks.on_action_finish(action, ev);
    }
    event
}
```

Key changes:
- `Notify` now calls `hooks.notify` instead of being a no-op
- `SaveState` becomes `persist_state` returning a real event (§6)
- Cancellation polled at top of every action
- Lifecycle hooks fire around the dispatch

### 5.3 `run_skill` calls hook lifecycle, file_snapshot already inside

`run_skill` (L374) is **mostly unchanged**. The file_snapshot pre/post (ISS-038) already lives inside it. After this design, that code finally runs in production because rustclaw routes through `execute()`.

Add to `run_skill`:
- `hooks.on_action_start` is already called at execute() level — no double-call
- `hooks.notify` for "starting skill X" / "skill X done" optional UX (gated by `config.notify_on_skill: bool`)

### 5.4 `run_skill` — phase-aware file_snapshot policy

ISS-038's gate was written assuming all skills produce file changes. That is wrong for some phases (e.g., `triage`, `review-design` — review skills inspect, they don't write code). Make the gate **phase-aware**, with the policy declared **per skill in its `SKILL.md` frontmatter** (not hardcoded in gid-core):

```yaml
---
name: implement
description: …
file_policy: required   # required | optional | forbidden — see below
---
```

```rust
enum SkillProducesFiles {
    Required,      // implement, generate-graph, update-graph, apply-review (when approving)
    Optional,      // research, planning
    Forbidden,     // review-design, review-requirements, review-tasks, triage
}

/// Resolved by the skill loader from the loaded skill's frontmatter.
/// Default `Optional` if the field is absent (backward compat with existing
/// SKILL.md files that don't declare it).
fn skill_file_policy(skill: &LoadedSkill) -> SkillProducesFiles {
    skill.frontmatter.file_policy.unwrap_or(SkillProducesFiles::Optional)
}
```

**Why frontmatter, not a hardcoded match in gid-core:**
- Skills in rustclaw are user-defined, dynamically loaded from `skills/<name>/SKILL.md`.
- Adding a new `Required` skill should not require a gid-core release.
- Renaming a skill should not silently flip the gate to `Optional` (a hardcoded match would fall through to `_ => Optional`).
- The policy belongs with the skill's contract, where the skill author edits it. Aligns with project convention "符合 purpose."

The recommended `file_policy` value is documented in the skill author guide. Skills bundled with this PR get explicit declarations:

| Skill | `file_policy` |
|---|---|
| `implement`, `execute-tasks`, `generate-graph`, `update-graph`, `apply-review` | `required` |
| `research`, `plan`, `design` | `optional` |
| `review-design`, `review-requirements`, `review-tasks`, `triage` | `forbidden` |
| (any new / unannotated skill) | `optional` (fallback) |

Post-condition logic:

| Policy | Files changed | Outcome |
|---|---|---|
| Required | yes | `SkillSucceeded` |
| Required | no | **`SkillFailed { reason: ZeroFileChanges }`** ← the ISS-038 gate |
| Optional | yes / no | `SkillSucceeded` (claim-driven) |
| Forbidden | no | `SkillSucceeded` |
| Forbidden | yes | **`SkillFailed { reason: UnexpectedFileChanges }`** ← review skills must not write code |

The `Forbidden + yes` case is a NEW gate (mentioned in rustclaw ISS-051 §"Self-review loop fixes" but never wired). Worth ~30 LOC.

### 5.5 New events introduced

> **Disambiguation (FINDING-3):** gid-core today has **two** unrelated public types named `RitualEvent`:
> - `crates/gid-core/src/ritual/state_machine.rs::RitualEvent` — the **state-machine event** (`SkillCompleted`, `SkillFailed`, `Start`, …). Drives `transition()`.
> - `crates/gid-core/src/ritual/notifier.rs::RitualEvent` — a **notification category** enum (`RitualStart`, `PhaseComplete`, `ApprovalRequired`, …). Drives Telegram filters.
>
> **Everything in this section refers to `state_machine::RitualEvent`.** No new variants are added to `notifier::RitualEvent`. The name collision is a latent code smell tracked separately (filed as a follow-up; rename one of the enums in a future PR — out of scope here to avoid expanding the diff).

#### 5.5.1 Existing `SkillFailed` shape — backward-compatible extension

`state_machine::RitualEvent::SkillFailed` already exists today as:

```rust
SkillFailed { phase: String, error: String }
```

and is matched in **8+ state-machine arms** (`state_machine.rs` lines 885, 1039, 1051, 1068, 1085, 1095, 1108), **multiple tests** (1578, 1590, 1664, 1676, 2150), and **`run_ritual`** (`v2_executor.rs:1303`). A field rename `phase` → `skill` plus replacing `error: String` with a structured `reason` would be a breaking change touching ~15 sites with no behavioral upside for the existing arms.

**Decision: extend, do not replace.** The variant becomes:

```rust
SkillFailed {
    phase: String,                       // existing — keep for back-compat
    error: String,                       // existing — human-readable summary
    reason: Option<SkillFailureReason>,  // NEW — set by the new gates; None for legacy emit sites
}
```

- Existing call sites keep emitting `SkillFailed { phase, error, reason: None }` — one-line edit per site (`..` rest pattern in matches stays valid).
- New gates (zero-file, forbidden-file, turn-limit-no-verdict) emit `SkillFailed { phase: <skill name>, error: <human msg>, reason: Some(<reason>) }`.
- State-machine match arms that need to branch on the structured reason use `if let Some(r) = reason { … }`; arms that don't care continue to ignore via `..`.
- For the new gates, the convention is `phase` carries the skill name (e.g., `"implement"`), matching how the existing arms already use the field for skill-shaped phases.

#### 5.5.2 Other new events (no existing variant to merge with)

```rust
pub enum RitualEvent {
    // … existing variants, including the extended SkillFailed above …

    /// `hooks.persist_state` returned an error after retry exhaustion.
    /// State machine handles via §6 retry/escalate logic.
    StatePersistFailed {
        attempt: u32,
        error: String,  // displayed; not used for control flow
    },

    /// `hooks.persist_state` succeeded (used by §6 to record attempts).
    StatePersisted { attempt: u32 },

    /// `hooks.should_cancel` returned `Some`.
    Cancelled { reason: CancelReason },

    /// `hooks.resolve_workspace` failed at ritual start.
    WorkspaceUnresolved { error: String },

    /// Self-review subloop produced a verdict (used by §8).
    SelfReviewCompleted {
        skill: String,
        verdict: ReviewVerdict,
        turns_used: u32,
    },
}

pub enum SkillFailureReason {
    ZeroFileChanges,        // Required policy, no diff
    UnexpectedFileChanges,  // Forbidden policy, diff present
    LlmTurnLimitNoVerdict,  // self-review subloop (§8)
    ReviewRejected,         // self-review subloop returned `reject`
    ExplicitClaim(String),  // skill itself reported failure
}
```

> **Migration scope** for the `SkillFailed` extension (informative, not prescriptive — implementer measures during PR):
> - 8 state-machine match arms: each gains `reason: None` (emit) or `reason: _` (ignore in match) as appropriate.
> - 5 test sites: same one-line edits.
> - `run_ritual` engine-error emit (v2_executor.rs:1303): emits `reason: None`.
> - No call site needs to change behavior; all are mechanical. Estimated <30 LOC of churn for the extension itself.

### 5.6 `run_ritual` (L1281) — breaking signature change, becomes the canonical loop

#### 5.6.1 Existing signature (must be replaced)

`crates/gid-core/src/ritual/v2_executor.rs:1281` today:

```rust
pub async fn run_ritual(
    task: &str,
    executor: &V2Executor,
) -> Result<RitualState>
```

This signature is **incompatible** with the design requirement that hooks be passed in and that workspace resolution + metadata stamping happen up front. **The signature changes are a breaking change** to a `pub` function in gid-core's public API.

**Existing callers** (search: `grep -rn "run_ritual" crates/`):
- gid-core internal integration tests (within `crates/gid-core/tests/` and `v2_executor.rs` test module). All co-located with gid-core; rewritten in the same PR.
- No external (downstream) callers known today besides rustclaw, which currently does **not** call `run_ritual` (it has its own dispatcher — that's the bug this PR fixes). After this PR, rustclaw is the primary external caller.

**Decision:** **break the signature, do not rename.** A `run_ritual_v2` would leave the old `run_ritual` as a dead-but-public function pretending to be a viable entry point — exactly the "two dispatchers" smell this PR exists to delete. Bump gid-core minor version and document the migration in CHANGELOG (§10.1, §12 AC12).

#### 5.6.2 New signature

```rust
pub async fn run_ritual(
    initial_state: RitualState,
    config: V2ExecutorConfig,
    hooks: Arc<dyn RitualHooks>,
) -> RitualOutcome {
    let executor = V2Executor::new(config, hooks.clone());
    let mut state = initial_state;

    // Resolve workspace first
    let workspace = match hooks.resolve_workspace(&state.work_unit) {
        Ok(p) => p,
        Err(e) => return RitualOutcome::failed(format!("workspace resolution: {e}")),
    };
    hooks.stamp_metadata(&mut state);

    loop {
        let actions = state_machine::transition(&state);
        if actions.is_empty() && state.is_terminal() { break; }

        for action in actions {
            // Single dispatcher: always go through executor.execute() (singular).
            // execute_actions() is demoted to a private helper in this PR — see §5.6.3.
            let event = executor.execute(&action, &state).await;
            if let Some(ev) = event {
                let prev_phase = state.phase().clone();
                state.apply(ev);
                if prev_phase != *state.phase() {
                    hooks.on_phase_transition(&prev_phase, state.phase());
                }
            }
        }
    }

    RitualOutcome::from_state(state)
}
```

Notes on the body (corrections vs the prior draft):
- The hook call is `on_phase_transition(&Phase, &Phase)`, matching the trait signature in §4 (FINDING-7 typo `on_phase_transition_phase` removed).
- Phase comparison uses `prev_phase != *state.phase()` rather than `&prev_phase != state.phase()` — both work, but the dereference form is closer to how phase enums are normally compared in the existing state machine code.
- `executor.execute()` (singular) is the only dispatch entry point used; see §5.6.3.

#### 5.6.3 `execute_actions` (plural) — demoted to private helper

`v2_executor.rs:239` today defines:

```rust
pub async fn execute_actions(&self, actions: &[RitualAction], state: &RitualState) -> Option<RitualEvent>
```

Existing `run_ritual` (line 1292+) calls `execute_actions`, not `execute`. After this PR there must be exactly one public dispatcher per G1 ("single code path that turns a `RitualAction` into an effect + `RitualEvent`"). Therefore:

- **`execute()` (singular) stays `pub`** and is the hook-instrumented dispatcher (§5.2).
- **`execute_actions()` (plural) loses `pub`** — becomes a private helper inside `V2Executor` (or is inlined into the new `run_ritual` loop and deleted entirely; equivalent outcome). All existing callers are within `v2_executor.rs` itself + co-located tests, all updated in the same commit.
- This is recorded in §7 deletion plan as part of commit 3.

The invariant after this change: every `RitualAction` produces its `RitualEvent` via exactly one code path (`V2Executor::execute`), which always calls `hooks.on_action_start` / `on_action_finish`. There is no second dispatcher route that bypasses hooks.

---

## §6. `save_state` Redesign

### 6.1 Move ownership

`save_state` is no longer a method on `V2Executor` or `RitualRunner`. It becomes:
- A **hook method** (`hooks.persist_state(&state) -> io::Result<()>`) — embedder owns the IO.
- A **V2Executor wrapper** (`persist_state(&state) -> RitualEvent`) — handles retry/escalate, returns event.

V2Executor never touches disk directly for state files.

### 6.2 Retry policy (inside V2Executor wrapper)

The retry loop is structured to be statically exhaustive: on every code path it produces a `RitualEvent`, with no `unreachable!()` / `panic!` surface. A panic inside the V2Executor task would drop in-memory state and emit no event at all — exactly the failure mode this design exists to eliminate (G3: no silent swallow).

```rust
async fn persist_state(&self, state: &RitualState) -> RitualEvent {
    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF: [Duration; 2] = [
        Duration::from_millis(50),
        Duration::from_millis(250),
        // No backoff after the final attempt — we exit either way.
    ];

    let mut last_error: String = String::from("no attempts made");  // unreachable in practice;
                                                                     // the loop runs ≥1 iteration.
    for attempt in 1..=MAX_ATTEMPTS {
        match self.hooks.persist_state(state).await {
            Ok(()) => return RitualEvent::StatePersisted { attempt },
            Err(e) => {
                last_error = e.to_string();
                if attempt < MAX_ATTEMPTS {
                    // Note: per FINDING-15 follow-up, per-attempt notify spam
                    // should be suppressed and replaced with a single summary
                    // notify on final outcome. Tracked as deferred follow-up.
                    self.hooks.notify(&format!(
                        "⚠️ state persist attempt {attempt}/{MAX_ATTEMPTS} failed: {e}; retrying"
                    )).await;
                    tokio::time::sleep(BACKOFF[(attempt as usize) - 1]).await;
                }
            }
        }
    }

    // All MAX_ATTEMPTS attempts exhausted.
    RitualEvent::StatePersistFailed {
        attempt: MAX_ATTEMPTS,
        error: last_error,
    }
}
```

Key properties:
- Every path through the function returns a `RitualEvent`; no `unreachable!()`.
- `last_error` is initialized to a sentinel only because the type system needs it; the `1..=MAX_ATTEMPTS` range is guaranteed non-empty (`MAX_ATTEMPTS = 3 > 0`), so by the time the loop exits the sentinel has been overwritten with a real `io::Error.to_string()`.
- Atomicity is the hook impl's responsibility (see §4 contract on `persist_state`). V2Executor assumes that an `Err` return leaves no on-disk corruption, so retry is safe.

### 6.3 State machine handling of `StatePersistFailed`

> **Design choice (FINDING-5, NG1):** Persist degradation is a **side-channel flag on `RitualState`**, not a new wrapping phase. The `Phase` enum is unchanged, the transition table is unchanged, and no new top-level state is added. This preserves NG1.

#### 6.3.1 Side-channel flag on `RitualState`

```rust
pub struct RitualState {
    // … existing fields unchanged …
    pub phase: Phase,

    /// Set when a `StatePersistFailed` event has been observed mid-phase
    /// and the ritual is continuing in-memory. Cleared on the next
    /// successful `StatePersisted` event. Never set at phase boundaries
    /// (those abort to `Failed` per §6.3.3).
    pub persist_degraded: Option<PersistDegradedInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistDegradedInfo {
    /// The phase the ritual was in when degradation began. Reported in
    /// notifications and post-mortems; not used for control flow.
    pub since_phase: Phase,
    /// Human-readable last error message from the most recent failed attempt.
    pub last_error: String,
    /// Number of consecutive `StatePersistFailed` events without an
    /// intervening `StatePersisted`. Reset to 0 on success (which also
    /// clears `persist_degraded` to `None`). On reaching 5, the next event
    /// transitions the ritual to `Failed` (§6.3.3).
    pub consecutive_failures: u32,
}
```

`persist_degraded` does **not** affect what `Phase` returns from `state.phase()`. The state machine's transition table sees only the existing phase. Only the events that handle persist outcomes inspect / mutate this field.

#### 6.3.2 Phase boundary — formal definition

A `SaveState` action is a **phase boundary** save iff it is the *first* `SaveState` emitted by the state machine *immediately after a phase-transition event* (e.g., `Implementing → Verifying`). All other `SaveState` actions are **mid-phase** saves (periodic checkpoints within a long-running phase like `Implementing` or `Verifying`).

To make this distinguishable at runtime without ambiguity, `RitualAction::SaveState` is extended with a kind tag:

```rust
pub enum RitualAction {
    // … existing variants unchanged …
    SaveState { kind: SaveStateKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStateKind {
    /// Emitted exactly once after each phase-transition event, before the
    /// new phase produces any other actions. A failed Boundary save aborts
    /// the ritual to `Failed` (§6.3.3) — we cannot guarantee the new phase
    /// starts from a known-persisted state.
    Boundary,
    /// Emitted as a periodic checkpoint inside a long-running phase
    /// (e.g. `Implementing` between LLM turns). A failed Periodic save
    /// flips the side-channel flag and lets the ritual continue.
    Periodic,
}
```

The state machine emits `SaveState { kind: Boundary }` on phase transitions and `SaveState { kind: Periodic }` from within long-running phases. This is the *only* state-machine change required by this design — it does not add states or rewrite transitions, just refines the existing `SaveState` action with a tag.

#### 6.3.3 Event handling

| Trigger | `persist_degraded` before | Action |
|---|---|---|
| `StatePersisted { attempt }` | `None` | No-op (normal success). |
| `StatePersisted { attempt }` | `Some(_)` | Clear `persist_degraded = None`. Emit `Notify("✅ persistence recovered after N failed attempts")`. |
| `StatePersistFailed` from a `Boundary` save | any | Transition to `Failed { reason: "could not persist state at phase boundary: <error>" }`. Ritual halts. |
| `StatePersistFailed` from a `Periodic` save | `None` | Set `persist_degraded = Some(PersistDegradedInfo { since_phase: <current>, last_error: e, consecutive_failures: 1 })`. Emit `Notify("⚠️ ritual continuing in-memory only, persist failed: <error>")`. Ritual continues in current phase. |
| `StatePersistFailed` from a `Periodic` save | `Some(info)` and `info.consecutive_failures < 4` | Increment `info.consecutive_failures`. Update `info.last_error`. Continue. |
| `StatePersistFailed` from a `Periodic` save | `Some(info)` and `info.consecutive_failures == 4` | This is the 5th consecutive failure. Transition to `Failed { reason: "5 consecutive persist failures: <error>" }`. Ritual halts. |
| `Cancelled` event while `persist_degraded.is_some()` | (any) | Transitions to `Cancelled` directly. The `persist_degraded` field is preserved on the final state for post-mortem; counter handling is irrelevant after Cancelled. |

This is the key compromise:
- **Disk full mid-implement-phase (Periodic save)** → don't lose 20 minutes of LLM work; flag, retry on next periodic save.
- **Disk full at phase boundary (Boundary save)** → fail loudly; we cannot guarantee the next phase starts from a known state.

#### 6.3.4 Persistence of the flag itself

`persist_degraded` is part of `RitualState` and therefore part of the JSON state file. If a `Periodic` save eventually succeeds while the flag is `Some(_)`, the *successful* write contains `persist_degraded: None` (cleared by the handler before the save). If the process restarts mid-degradation (rare: process restart while in-memory state is ahead of disk), the on-disk state legitimately shows the last successfully persisted state — which is what resume-from-disk should see. No special handling needed.

#### 6.3.5 NG2 compatibility

NG2 says "not changing the on-disk JSON format." Adding the optional `persist_degraded` field is backward-compatible: old state files without the field deserialize with `persist_degraded: None` (serde default), and new state files in the steady-state success path always have `persist_degraded: None` so the field is omitted by `#[serde(skip_serializing_if = "Option::is_none")]`. Old readers can ignore the field if it ever leaks to disk in a `Some(_)` state file (would be parseable as long as they're tolerant; if not, this is the one case where mid-degradation files break older readers — accepted, since the alternative is silent data loss).

### 6.4 Failure observability

Every `StatePersistFailed` event:
1. Logged at ERROR level with the underlying io::Error.
2. Surfaced to user via `hooks.notify` with retry attempt + error.
3. Recorded in the in-memory event log so post-mortem can see the timeline.

Disk-full is no longer silent. Phase-boundary failures abort. Mid-phase failures degrade.

### 6.5 Why not "fail immediately on first persist error"

Considered. Rejected because:
- LLM ritual phases are expensive (minutes of inference). Losing 15 min of `implement` work because `/tmp` briefly hit a quota is bad UX.
- The in-memory state is still correct. Refusing to use it because we can't write it down trades availability for a property we can recover (eventual persistence).

The compromise: **never silently lose data, but distinguish recoverable from unrecoverable.**

## §7. rustclaw `ritual_runner.rs` — Deletion & Migration Plan

Current file: 2954 LOC. After this change: target **~600 LOC** (RustclawHooks impl + thin RitualRunner shell).

### 7.1 Deletion list

Sections of `src/ritual_runner.rs` to **remove entirely**:

| Lines | What | Reason |
|---|---|---|
| ~575–810 | First action `match action {…}` dispatcher | Replaced by V2Executor |
| ~813–1100 | Second action `match action {…}` dispatcher | Replaced by V2Executor |
| ~280–490 | `run_skill` rustclaw-local impl, `run_shell` rustclaw-local | Replaced by V2Executor |
| ~1200–1400 | `run_planning`, `run_harness`, `run_triage` rustclaw-local | Replaced by V2Executor |
| ~1500–1700 | `apply_review`, `update_graph` rustclaw-local | Replaced by V2Executor |
| ~1900–2200 | self-review subloop rustclaw-local | Replaced by V2Executor (§8) |

**Approximate net deletion: ~1700 LOC.** Final number measured during implementation.

#### 7.1.1 Two-phase deletion (T13a → T13b)

Reality check from T12 implementation (2026-04-27): after T12 migrated the two **production** entry points (`tools.rs::start_ritual` and `channels/telegram.rs::handle_ritual_command`) to call `gid_core::ritual::run_ritual`, the dispatcher bodies in `ritual_runner.rs` are **still reachable** through 17 other call sites in `channels/telegram.rs` that depend on the public `RitualRunner` API surface — `advance()`, `send_event()`, `resume_from_phase()`, `make_ritual_runner()` factory — driving `/ritual retry`, `/skip`, `/clarify`, `/reply`, `/cancel`, `/resume-from-phase`, and the orphan-sweep recovery path. A single-shot deletion of the dispatcher bodies would break compilation at all 17 sites and at `make_ritual_runner`.

To preserve bisectability and keep `cargo test --all` green at every commit, T13 is split into two atomic steps:

**T13a — stub dispatcher bodies (compile-preserving)**
- Replace bodies of `RitualRunner::run_skill`, `run_shell`, `run_triage`, `run_planning`, `run_harness`, and `save_state` with `unreachable!("migrated to V2Executor in T12 — should not be reached, see ISS-052")`.
- Keep the function signatures, keep `execute_event_producing` / `execute_event_producing_single` / `advance()` / `send_event()` / `resume_from_phase()` and `make_ritual_runner()` factory **as-is** so the 17 telegram call sites still compile.
- Verify production path goes through `run_ritual` (T12 work) and never enters a stub. Add a release-mode log line on stub entry as a tripwire (defensive — should never fire).
- All 348 existing tests must stay green.

**T13b — delete public API + migrate 17 call sites**
- Delete `advance()`, `send_event()`, `resume_from_phase()`, `execute_event_producing*`, `make_ritual_runner()`.
- Migrate each of the 17 telegram call sites to one of:
  - `run_ritual + RustclawHooks` for sites that actually drive a ritual forward (e.g. `/ritual retry` resumes via `run_ritual` with the persisted state).
  - Thin event-recording shims that read state files and append `UserEvent`s (e.g. `/clarify`, `/reply` may not need a runner at all — they write to the ritual's input channel which `run_ritual` consumes via `hooks.poll_user_input` or similar).
  - Direct state-file mutation for read-only sites (`/ritual status`).
- Delete the now-empty stub bodies introduced in T13a.

The split is the difference between "one bisect point if anything breaks" (T13b broken in isolation = revert one commit) and "tangled refactor commit that must be reverted whole" (T12+T13 monolith).

**Acceptance for T13a in isolation:**
- `wc -l src/ritual_runner.rs` drops only marginally (just function bodies → `unreachable!`).
- `cargo test --all` green.
- `grep -n 'unreachable!' src/ritual_runner.rs` returns ≥6 hits with the ISS-052 message.
- Manual sanity: trigger `/ritual` end-to-end via Telegram, verify no stub log fires.

**Acceptance for T13b** is the original §7.1 / AC1 / AC3 acceptance: file ≤800 LOC, no `match action` against `RitualAction`, all 17 telegram sites migrated, `make_ritual_runner` deleted.

### 7.2 What stays / changes

**Stays as-is:**
- Persistence-path resolution (`~/.gid/runtime/rituals/<id>.json`, project-local fallback) — moves into hook impl.
- Telegram channel handle, notifier wiring — moves into hook impl.
- Project registry lookup (`~/.config/gid/projects.yml`) — moves into hook impl.
- Daemon PID stamping — moves into hook impl.
- `RitualRunner::new` / public CLI surface (`/ritual`, `/ritual status`, `/ritual cancel`).

**Changes:**
- `RitualRunner` becomes a shell that:
  1. constructs `RustclawHooks` (telegram, persister, registry)
  2. builds `V2ExecutorConfig` from env / config.yml
  3. calls `gid_core::ritual::run_ritual(initial_state, config, Arc::new(hooks))`
  4. handles the returned `RitualOutcome` (notify final result, archive state file)

### 7.3 New file: `rustclaw/src/ritual_hooks.rs` (~400 LOC)

```rust
pub struct RustclawHooks {
    telegram: Arc<TelegramClient>,
    persister: Arc<RitualPersister>,
    registry: Arc<ProjectRegistry>,
    cancel_flag: Arc<AtomicBool>,
    daemon_pid: u32,
    engram: Option<Arc<EngramHandle>>,
}

#[async_trait]
impl RitualHooks for RustclawHooks {
    async fn notify(&self, message: &str) {
        if let Err(e) = self.telegram.send(message).await {
            tracing::warn!("telegram notify failed: {e}");
        }
    }

    async fn persist_state(&self, state: &RitualState) -> std::io::Result<()> {
        self.persister.write(state).await
    }

    fn resolve_workspace(&self, work_unit: &WorkUnit) -> Result<PathBuf, WorkspaceError> {
        self.registry.resolve(&work_unit.project)
    }

    fn stamp_metadata(&self, state: &mut RitualState) {
        state.adapter_pid = Some(self.daemon_pid);
        state.adapter = Some("rustclaw".to_string());
    }

    fn on_phase_transition(&self, from: &RitualState, to: &RitualState) {
        if let Some(engram) = &self.engram {
            engram.write_phase_transition(from.phase(), to.phase());
        }
    }

    fn should_cancel(&self) -> Option<CancelReason> {
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            Some(CancelReason {
                source: CancelSource::UserCommand,
                message: "user requested /ritual cancel".to_string(),
            })
        } else {
            None
        }
    }
}
```

### 7.4 Migration ordering inside rustclaw

Single PR, but ordered commits to keep each commit reviewable:

1. **Commit 1:** add `Cargo.toml` dep on new gid-core version (will be a path dep during dev, crate version at release).
2. **Commit 2:** create `ritual_hooks.rs` with `RustclawHooks` impl. Wire to existing helpers (TelegramClient, persister already exist).
3. **Commit 3:** rewrite `RitualRunner::run` to call `gid_core::ritual::run_ritual`. Keep old methods as `#[deprecated]` and unused.
4. **Commit 4:** delete unused dispatcher code, `#[deprecated]` methods, dead helpers. Largest diff, but mechanical (no behavior change beyond what commit 3 already introduced).
5. **Commit 5:** update tests (most rustclaw integration tests need rewiring).
6. **Commit 6:** update `tasks/` docs and any README references to `RitualRunner` internals.

### 7.5 Backward compat

- **CLI surface unchanged.** `rustclaw run`, `/ritual`, `/ritual status`, `/ritual cancel` all behave the same. Acceptance tests on these commands must pass identically.
- **State file format unchanged.** Existing `.gid/runtime/rituals/*.json` files load and resume correctly. (Tested by §9.4.)
- **Telegram message format may change slightly.** Notifications now route through `hooks.notify` from V2Executor's perspective; the raw text is the same (V2Executor passes through), but timing may shift (e.g. `Notify { msg }` now fires immediately at execute() time, not buffered). Document in CHANGELOG.

### 7.6 External callers of `RitualRunner` (FINDING-4)

The deletion plan in §7.1 only addresses the *internals* of `src/ritual_runner.rs`. The actual production entry points to `RitualRunner` live in other rustclaw files. Search command: `grep -rn "RitualRunner\|ritual_runner::" rustclaw/src/ rustclaw/tests/`. Current call sites:

| File:line | Constructor / call | What it does | Migration in this PR |
|---|---|---|---|
| `rustclaw/src/tools.rs:5907` | `RitualRunner::new(…)` | `start_ritual` tool dispatch — orchestrator-spawned ritual | Replace with `gid_core::ritual::run_ritual(initial, config, Arc::new(RustclawHooks::new(…)))`. |
| `rustclaw/src/channels/telegram.rs:1155` | `RitualRunner::with_registries(…)` | `/ritual` command handler | Same as above. Keep `with_registries` shape locally as a thin builder over `RustclawHooks` if registries are still needed. |
| `rustclaw/src/channels/telegram.rs:1433` | `RitualRunner::with_registries(…)` | `/ritual status` command (read-side; may not need full hooks) | Read-only path — may keep a lightweight `RitualRunner` shell that only reads state files. Decide during commit 3. |
| `rustclaw/src/channels/telegram.rs:1502` | `RitualRunner::with_registries(…)` | `/ritual cancel` command | Cancellation is now a hook signal — flip `cancel_flag` and let the running ritual observe via `should_cancel`. Cancel command no longer constructs a new runner. |
| `rustclaw/src/channels/telegram.rs:2555` | `RitualRunner::new(…)` (orphan sweep path) | Ritual orphan sweep | Construct `RustclawHooks` with sweep-mode config, call `run_ritual` with the recovered state. |
| `rustclaw/src/tools.rs:80, 2795, 2804, 5774, 5785` | `crate::ritual_runner::NotifyFn` type alias | Notify function plumbed through tools | The `NotifyFn` shape becomes redundant once notifications go through `hooks.notify`. Either delete the alias or keep it as a compatibility shim that delegates to `hooks`. Decide in commit 4. |
| `rustclaw/src/tools.rs:2942` | `ritual_runner::preload_files_with_budget` | Helper, not a runner construction | Standalone helper; either keep in `ritual_runner.rs` (the file isn't deleted, just shrunk) or move to a dedicated module. No migration concern. |

**Updated commit ordering** (replaces the §7.4 plan): commit 3 ("rewrite `RitualRunner::run`") becomes commit 3a + 3b + 3c, **mirroring the T12 / T13a / T13b graph tasks**:

- **3a (T12, completed 2026-04-27):** Migrate the two production entry points — `tools.rs::start_ritual` and `channels/telegram.rs::handle_ritual_command` — to call `gid_core::ritual::run_ritual` with `RustclawHooks`. Old `RitualRunner` API surface (`advance` / `send_event` / `resume_from_phase` / `make_ritual_runner`) preserved so the remaining 17 telegram call sites still compile. Build + 348 tests green.
- **3b (T13a):** Stub the dispatcher bodies (`run_skill` / `run_shell` / `run_triage` / `run_planning` / `run_harness` / `save_state`) with `unreachable!("migrated to V2Executor in T12 — should not be reached, see ISS-052")`. Public API surface untouched. Build + tests green. This is the bisect point — if anything regresses, revert this single commit.
- **3c (T13b):** Delete `advance()`, `send_event()`, `resume_from_phase()`, `execute_event_producing*`, `make_ritual_runner()`. Migrate all 17 telegram call sites (`/ritual retry`, `/skip`, `/clarify`, `/reply`, `/cancel`, `/resume-from-phase`, orphan sweep, etc.) to either `run_ritual + RustclawHooks` flows or thin event-recording shims. Delete the now-empty stub bodies from 3b. AC3 enforced: zero `match action` against `RitualAction`, file ≤800 LOC.

This three-step split (was: 3a + 3b) lets a reviewer bisect "external production callers migrated correctly" (3a) vs "dispatcher bodies stubbed" (3b) vs "public API removed + 17 secondary callers migrated" (3c) independently. T13a was added during T12 implementation when the 17 secondary call sites became visible.

**Test sites** (`grep -rn "RitualRunner\|ritual_runner" rustclaw/tests/`) are enumerated and migrated in commit 5 alongside the unit-test rewiring.

---

## §8. Self-Review Loop Fix

### 8.1 Current bug (rustclaw ISS-051 §"Self-review loop fixes")

The self-review subloop runs the review skill up to N=4 times. Each run can:
- produce a verdict (`accept`, `reject`, `needs-changes`) → loop exits with verdict
- run out of LLM turns without producing a verdict → **currently treated as accept** (bug)

In `r-950ebf`, all 4 turns hit turn limit. Loop terminated. Ritual marked review phase as passed. **Should have failed.**

### 8.2 Fix

Inside `V2Executor` (since dispatch lives there now), at the self-review subloop:

```rust
async fn run_self_review_subloop(
    &self,
    skill_name: &str,
    context: &str,
    state: &RitualState,
) -> RitualEvent {
    const MAX_TURNS: u32 = 4;
    let mut last_error: Option<String> = None;

    for turn in 1..=MAX_TURNS {
        let outcome = self.run_skill_inner(skill_name, context, state).await;

        match outcome {
            SubloopOutcome::Verdict(verdict) => {
                return RitualEvent::SelfReviewCompleted {
                    skill: skill_name.to_string(),
                    verdict,
                    turns_used: turn,
                };
            }
            SubloopOutcome::TurnLimitNoVerdict { error } => {
                last_error = Some(error);
                if turn < MAX_TURNS {
                    self.hooks.notify(&format!(
                        "self-review turn {turn}/{MAX_TURNS} hit turn limit, retrying"
                    )).await;
                    continue;
                }
            }
            SubloopOutcome::SkillError(e) => {
                return RitualEvent::SkillFailed {
                    phase: skill_name.to_string(),
                    error: e.clone(),
                    reason: Some(SkillFailureReason::ExplicitClaim(e)),
                };
            }
        }
    }

    // Fell off the loop = all MAX_TURNS attempts exhausted without verdict
    RitualEvent::SkillFailed {
        phase: skill_name.to_string(),
        error: last_error.unwrap_or_else(|| "turn limit reached without verdict".to_string()),
        reason: Some(SkillFailureReason::LlmTurnLimitNoVerdict),
    }
}
```

> **Field convention reminder (§5.5.1):** `phase` carries the skill name for skill-shaped failures; `error` is the human-readable summary; `reason` is `Some(_)` for the new gates introduced by this PR and `None` for legacy emit sites.

### 8.3 Three independent gates after this change

For an `implement` skill running in a ritual:

1. **`run_skill` post-condition (file_snapshot):** if Required policy and zero file diff → `SkillFailed::ZeroFileChanges`.
2. **Self-review subloop:** if review verdict is `reject` → `SkillFailed::ReviewRejected`. If turn limit on all attempts → `SkillFailed::LlmTurnLimitNoVerdict`.
3. **Phase boundary persistence:** if `StatePersistFailed` at phase boundary → `Failed`.

`r-950ebf` fails at gate 1 (zero file changes) — never gets to gate 2. Even if it did, gate 2 now fails on turn-limit-no-verdict.

### 8.4 Side benefit: gate 2 testability

Self-review subloop is now in V2Executor, which already has `ScriptedLlm` test infrastructure (`v2_executor.rs:1781+`). Adding a "subloop hits turn limit on every turn" test is ~30 LOC of test setup, vs. the current rustclaw-side path which has no LLM scripting harness at all.

## §9. Testing Strategy

### 9.1 gid-core unit tests (new)

Location: `crates/gid-core/src/ritual/v2_executor.rs` test module + `hooks.rs` test module.

| Test | Scenario | Asserts |
|---|---|---|
| `hook_dispatch_called_once` | Run a 3-action ritual through `run_ritual` | `on_action_start` called 3×, `on_action_finish` called 3× |
| `notify_routed_through_hook` | Action `Notify { msg }` | `hooks.notify` called with exact msg; no other side effects |
| `cancel_polled_between_actions` | `should_cancel` returns Some after action 2 | Action 3 not executed; final state is Cancelled |
| `persist_retry_succeeds_on_attempt_3` | `FailingPersistHooks(fail_after=2)` | Event `StatePersisted { attempt: 3 }`; ritual continues |
| `persist_retry_exhausted` | `FailingPersistHooks(fail_after=999)` on a `Periodic` save | Event `StatePersistFailed { attempt: 3 }`; `state.persist_degraded.is_some()` |
| `persist_failed_at_phase_boundary` | `FailingPersistHooks(fail_after=999)` on a `SaveState { kind: Boundary }` | Final state `Failed`; `persist_degraded` never set (boundary aborts directly) |
| `skill_required_zero_files_fails` | scripted LLM, `implement` skill, no FS changes | `SkillFailed { ZeroFileChanges }` |
| `skill_forbidden_with_files_fails` | scripted LLM, `review-design`, writes a file | `SkillFailed { UnexpectedFileChanges }` |
| `subloop_turn_limit_all_attempts_fails` | scripted LLM that never produces verdict | `SkillFailed { LlmTurnLimitNoVerdict }`; `turns_used = 4` |
| `subloop_recovers_on_turn_3` | LLM produces verdict on turn 3 | `SelfReviewCompleted { turns_used: 3 }` |
| `workspace_unresolved_aborts` | `resolve_workspace` returns `NotFound` | Ritual ends with `WorkspaceUnresolved`; no actions executed |
| `phase_transition_hook_called_on_each_change` | Script a 4-phase ritual through `run_ritual` | `on_phase_transition` called exactly 3× with correct `(from, to)` `Phase` pairs in order |
| `stamp_metadata_called_once_at_start` | Run `run_ritual` with hooks that record calls | `stamp_metadata` called exactly once, before any action dispatches; mutations made by the hook are visible to the first action |
| `phase_boundary_persist_fail_aborts` | `FailingPersistHooks` returning Err on a `SaveState { kind: Boundary }` | Final state `Failed`; reason mentions phase boundary; no `persist_degraded` flag set |
| `mid_phase_persist_fail_degrades` | `FailingPersistHooks` returning Err on a `SaveState { kind: Periodic }` once, then succeeding | After failure: `persist_degraded = Some(_)` with `consecutive_failures: 1`; after next success: `persist_degraded = None`; ritual continues |
| `persist_degraded_5_failures_aborts` | `FailingPersistHooks` returning Err on every `Periodic` save | After 5 consecutive failures: final state `Failed`; reason mentions "5 consecutive persist failures" |

Target: ~16 new tests, ~800 LOC. The five additional tests beyond the original 11 close hook-coverage gaps for `on_phase_transition`, `stamp_metadata`, and the §6.3 boundary/degraded/abort branches respectively (without these tests the §6.3 redesign is untestable from outside V2Executor).

### 9.2 gid-core integration test (replaces ad-hoc)

Single end-to-end test in `crates/gid-core/tests/ritual_e2e.rs`:

```rust
#[tokio::test]
async fn full_ritual_with_noop_hooks() {
    let tempdir = tempfile::tempdir().unwrap();
    let work_unit = WorkUnit::issue("test-project", "ISS-001");
    let hooks = Arc::new(NoopHooks::for_tempdir(tempdir.path()));
    let config = V2ExecutorConfig::for_test();
    let initial = RitualState::start(work_unit, "fix the bug");

    let outcome = run_ritual(initial, config, hooks.clone()).await;

    assert!(outcome.is_done());
    assert!(hooks.notifications.lock().unwrap().len() > 0);
    // verify state file exists in tempdir
    assert!(tempdir.path().join("state.json").exists());
}
```

### 9.3 rustclaw unit tests (new)

Location: `rustclaw/src/ritual_hooks.rs` test module.

| Test | Asserts |
|---|---|
| `hooks_notify_calls_telegram` | mock TelegramClient, verify `send` called |
| `hooks_persist_writes_correct_path` | tempdir persister, verify `.gid/runtime/rituals/<id>.json` |
| `hooks_resolve_workspace_via_registry` | mock registry, verify resolution |
| `hooks_cancel_flag_observed` | flip flag, verify `should_cancel` returns Some once then None |
| `hooks_stamp_metadata_sets_pid_and_adapter` | call, verify state fields populated |

Target: ~5 tests, ~200 LOC.

### 9.4 rustclaw integration tests (replace existing)

Location: `rustclaw/tests/ritual_integration.rs`.

| Test | Scenario | Critical assertion |
|---|---|---|
| `prod_path_invokes_v2executor` | Real `RitualRunner` + mock hooks that record `on_action_start` calls | At least one `on_action_start` observed → proves V2Executor reached |
| `zero_file_implement_fails_in_prod` | Mock LLM that claims success but writes nothing | Final state Failed, reason ZeroFileChanges (this is r-950ebf's regression test) |
| `state_file_format_unchanged` | Run ritual to completion, parse state file with old `serde_json` schema | Parses without error |
| `state_file_resume` | Write a v0.x-format state file, start RitualRunner, verify resume | Loads, continues from saved phase |
| `cli_ritual_cancel_observed` | Start ritual, send `/ritual cancel`, verify cancel within ~2s | Final state Cancelled |
| `telegram_messages_unchanged` | Run scripted ritual, capture all TelegramClient.send calls | Compare to golden snapshot |

Target: ~6 tests. The `zero_file_implement_fails_in_prod` test is the **single most important test** in this design — it is the live regression for r-950ebf.

### 9.5 Manual acceptance run

Before merge:

1. Run a real ritual against engram repo on a trivial issue (pick something with one-line fix).
2. Verify Telegram messages match pre-change format.
3. Verify state file in `.gid/runtime/rituals/` has identical schema.
4. Run a ritual that should fail (intentionally tell LLM to write nothing). Verify it fails at gate 1, not silently passes.
5. Simulate disk full during implement phase: `mount -o ro` the persist dir. Verify ritual sets `persist_degraded` flag, surfaces notification, and on remount the next periodic save clears the flag and the ritual recovers.

---

## §10. Release & Rollout Plan

### 10.1 Version bumps

- **gid-core:** `0.x.y` → `0.(x+1).0` (minor — new public trait `RitualHooks`, new public events, new public function `run_ritual` signature).
- **rustclaw:** patch bump after switching to new gid-core.

### 10.2 Coupling with ISS-053 (artifact)

ISS-053 also touches gid-core and adds new public API. Two options:

**Option A (recommended): same minor release.**
- One gid-core release contains: ISS-052 hooks rework + ISS-053 artifact tools.
- Rustclaw upgrades once.
- CHANGELOG one section.
- Risk: bigger blast radius if release has issues. Mitigation: stage on a branch, run the §9.5 manual acceptance + ISS-053 acceptance against the same RC build.

**Option B: separate minors, ISS-052 first.**
- gid-core 0.(x+1).0 = ISS-052.
- gid-core 0.(x+2).0 = ISS-053 (artifact).
- Rustclaw upgrades twice.
- More release overhead but easier to bisect.

**Decision:** Option A. ISS-052 and ISS-053 don't share API surface. Releasing them together saves an upgrade cycle in rustclaw, which is the only consumer that matters today.

### 10.3 Pre-release checks

- `cargo test --all` clean in gid-core
- `cargo test --all` clean in rustclaw against path-dep gid-core
- §9.5 manual acceptance complete
- Both ISS-052 and ISS-053 sub-issues marked `done` in their respective `.gid/issues/*/issue.md`

### 10.4 Post-release verification

After publishing gid-core to crates.io:

1. rustclaw bumps `Cargo.toml` to crate version (drop path dep)
2. CI green
3. Run a real engram ritual end-to-end

### 10.5 Rollback plan

If post-release issues:
- gid-core: yank the bad version, release a `.1` patch with revert. rustclaw can pin to previous.
- rustclaw: revert the upgrade commit, rebuild. Old `ritual_runner.rs` is in git history.

---

## §11. Risks & Open Questions

### 11.1 Risks

**R1. Hidden coupling discovered during deletion.**
The two rustclaw dispatchers (L575, L813) have drifted. Some subtle behavior may exist in only one of them and not in V2Executor. Mitigation: §7.4 commit ordering keeps old code as `#[deprecated]` for one commit so we can compare behavior. §9.4 captures Telegram message golden snapshot to catch UX regressions.

**R2. Async/sync mismatch in hooks.**
Some adapter operations (Telegram send) are async; some (registry lookup, PID stamping) are sync. The current design mixes both. Risk: someone implements `should_cancel` with blocking IO and stalls V2Executor between every action. Mitigation: doc comment on trait clearly states sync methods must not block; integration test with deliberately slow sync hook to verify V2Executor doesn't degrade.

**R3. `persist_degraded` side-channel flag complexity.**
The flag is on `RitualState` (not a new phase — see §6.3.1) but introduces new branches in event handling for `StatePersistFailed` / `StatePersisted` and a counter that must be correctly reset. Risk: counter-reset bugs (flag never clears, ritual halts after 5 transient FS hiccups even though disk recovered) or counter-leak bugs (flag persists across phase transitions when it shouldn't). Mitigation: §9.1 has explicit tests for both the degraded→recovered path (`mid_phase_persist_fail_degrades`) and the 5-failure abort (`persist_degraded_5_failures_aborts`). The flag is reviewed separately from the phase enum.

**R4. Cross-repo PR coordination.**
Two repos must merge in lockstep. If gid-core releases but rustclaw upgrade lags, rustclaw is stuck on old gid-core. Mitigation: prepare rustclaw upgrade PR against path-dep gid-core; merge within 24h of gid-core release.

**R5. Tests in rustclaw currently rely on testing internal dispatch.**
Many tests poke `RitualRunner::run_skill` directly. These all need rewriting to test through `run_ritual` + mock hooks. Mitigation: §7.4 commit 5 dedicated to test rewiring; test count may temporarily decrease.

**R6. Two public types named `RitualEvent` in gid-core (latent).**
`crates/gid-core/src/ritual/state_machine.rs::RitualEvent` (state-machine event) and `crates/gid-core/src/ritual/notifier.rs::RitualEvent` (notification category) coexist with the same name in sibling modules. This PR adds variants to the state-machine one (§5.5) and could be silently mis-applied to the notifier one. Mitigation: §5.5 explicitly disambiguates; reviewer must check the import path of any patch touching `RitualEvent`. The rename itself is filed as a follow-up issue (out of scope here to avoid expanding the diff).

### 11.2 Open questions

**Q1. Should `notify` be `Result<(), io::Error>` so the caller can react?**
Current design: best-effort, log on failure. Counterargument: silent Telegram failures are also a form of "ritual silently degrading." Proposal: leave as-is (best-effort) but log at WARN; revisit if real users hit the case.

**Q2. Should `run_ritual` own the loop, or should embedders (rustclaw) own it and just call `executor.execute` themselves?**
Pro embedder-owned: more flexibility (e.g., custom logging between actions).
Pro lib-owned: enforces single dispatcher (the entire point of this issue).
**Decision:** lib-owned. Flexibility is a smell here — flexibility is exactly what created the duplicate dispatcher.

**Q3. Engram side-writes — hook or out of band?**
Current design: optional `engram` field in RustclawHooks; `on_phase_transition` writes if present.
Alternative: rustclaw subscribes to `RitualEvent` stream out of band.
**Decision:** hook. Already in design. Out-of-band would require V2Executor to publish events to a channel = new coupling.

**Q4. Should time spent with `persist_degraded.is_some()` count toward ritual timeout?**
Open. Probably yes — a ritual stuck with the flag set for 10 minutes is dead (5 consecutive failures will abort it anyway, but the wall-clock time still drags). Recommend: time counts normally regardless of the flag; user can extend via `/ritual extend`.

**Q5. Do we need a feature flag (`legacy_dispatch: bool`) for gradual rollout?**
Considered. Rejected. Two dispatchers is the bug; shipping a third behind a flag is worse. We do the cutover atomically in one rustclaw PR.

---

## §12. Acceptance Criteria

This design is "done" when:

- **AC1.** `rustclaw/src/ritual_runner.rs` line count reduced by ≥1500 LOC (final state, measured at end of T13b).
- **AC2.** `cargo test --all` passes in both repos against the new gid-core. **Must hold at every commit boundary** — T12, T13a, and T13b each leave the workspace green.
- **AC3.** `grep -rn "match action" rustclaw/src/` returns **zero** matches against `RitualAction` variants. (Single-dispatcher invariant — measured at end of T13b.)
- **AC4.** New gid-core test `skill_required_zero_files_fails` passes (gate runs).
- **AC5.** New rustclaw integration test `zero_file_implement_fails_in_prod` passes (gate runs in production path — r-950ebf regression test).
- **AC6.** New gid-core test `subloop_turn_limit_all_attempts_fails` passes (rustclaw ISS-051 gate).
- **AC7.** New gid-core test `persist_retry_exhausted` passes; `persist_failed_at_phase_boundary` shows ritual transitions to Failed not silent-continue.
- **AC8.** Manual run §9.5 step 4 (zero-file ritual) ends in Failed state, not Done.
- **AC9.** Manual run §9.5 step 5 (RO mount mid-implement) sets `persist_degraded` flag, surfaces a notification, and on remount the flag clears and the ritual recovers.
- **AC10.** `RitualHooks` trait is `pub` and documented; `NoopHooks` and `FailingPersistHooks` are `pub` for downstream test use.
- **AC11.** All four parent issues (gid-rs ISS-051, gid-rs ISS-052, rustclaw ISS-050, rustclaw ISS-051) closeable with reference to this design + the merged PRs.
- **AC12.** CHANGELOG entry in gid-core describing the breaking changes:
    - `V2Executor::new` signature now requires `Arc<dyn RitualHooks>`.
    - `run_ritual` signature replaced (old: `(&str, &V2Executor) -> Result<RitualState>`; new: `(RitualState, V2ExecutorConfig, Arc<dyn RitualHooks>) -> RitualOutcome`).
    - `RitualEvent::SkillFailed` extended with `reason: Option<SkillFailureReason>` field (existing emit sites pass `reason: None`; new gates pass `Some`).
    - `RitualAction::SaveState` extended with `kind: SaveStateKind` field.
    - `RitualState` extended with optional `persist_degraded: Option<PersistDegradedInfo>` side-channel field (serde-defaulted; backward-compatible with old state files).
    - Migration snippet: shows constructing `V2Executor` + `RustclawHooks`-equivalent, then calling new `run_ritual`.
