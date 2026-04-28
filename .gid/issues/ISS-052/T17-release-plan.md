# T17 Release Plan — gid-core 0.4.0 + gid-dev-cli 0.4.0

> Draft. Apply after CHANGELOG review + rustclaw integration tests (T16) + manual acceptance (T17 §9.5).

## Version-bump diff

### `crates/gid-core/Cargo.toml`

```diff
 [package]
 name = "gid-core"
-version = "0.3.2"
+version = "0.4.0"
 edition = "2021"
```

**Justification:** ISS-052 introduces breaking changes to public API (`run_ritual`
signature, `V2Executor::new`, `RitualEvent::SkillFailed` field, `RitualAction::SaveState`
field, `RitualState` field). Per design §10.1 and SemVer 0.x rules, this is a minor
bump (0.3.2 → 0.4.0). See CHANGELOG `[0.4.0]` *Breaking changes — `ritual` feature*.

### `crates/gid-cli/Cargo.toml`

```diff
 [package]
 name = "gid-dev-cli"
-version = "0.3.1"
+version = "0.4.0"
 edition = "2021"
 ...

 [dependencies]
-gid-core = { path = "../gid-core", version = "0.3.1", features = ["full"] }
+gid-core = { path = "../gid-core", version = "0.4.0", features = ["full"] }
```

**Justification:** Track gid-core's minor so the CLI's published version unambiguously
maps to the gid-core surface it was built against. Also update the path-dep version
requirement so a stale registry index doesn't accidentally resolve to old gid-core.

> Note: gid-dev-cli 0.3.1 was the last published version; gid-core was at 0.3.2.
> The misalignment is harmless but confusing; aligning here is the cleanest fix.

## Pre-publish checklist (design §10.3)

- [ ] `cargo test --workspace --all-features` clean in gid-rs
  - including new `subloop_turn_limit_all_attempts_fails` (T07)
  - including new `persist_retry_exhausted` + `persist_failed_at_phase_boundary` (T03/T04)
  - including new `full_ritual_with_noop_hooks` e2e (T10)
- [ ] `cargo test --all` clean in rustclaw against path-dep gid-core (T16)
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
  - currently passing per ISS-042 closure
- [ ] T16 acceptance: `zero_file_implement_fails_in_prod` regression test green
- [ ] T17 manual acceptance §9.5 complete on engram repo:
  - [ ] real ritual on a trivial engram issue
  - [ ] Telegram message format unchanged (golden snapshot)
  - [ ] state file schema unchanged (round-trip 0.3.x file)
  - [ ] intentional zero-file ritual fails at gate (ISS-038 gate alive in prod)
  - [ ] `mount -o ro` mid-implement → `persist_degraded` set + recovery on remount
- [ ] All ISS-052 sub-issues marked `done` in `.gid/issues/ISS-052/issue.md`
- [ ] CHANGELOG `[0.4.0]` date filled in
- [ ] gid-core README + lib.rs doc-comments referencing `run_ritual` updated

## Publish order

1. `cargo publish -p gid-core` (gid-rs repo)
2. Wait for crates.io index sync (~30s)
3. `cargo publish -p gid-dev-cli` (gid-rs repo) — picks up gid-core 0.4.0 from registry
4. Tag release: `git tag v0.4.0 && git push --tags`
5. Update rustclaw `Cargo.toml` to drop path-dep:

```diff
-gid-core = { path = "/Users/potato/clawd/projects/gid-rs/crates/gid-core", features = ["full"] }
+gid-core = { version = "0.4.0", features = ["full"] }
```

6. `cargo update -p gid-core` in rustclaw
7. `cargo test --all-features` in rustclaw against registry build
8. Bump rustclaw `version = "0.1.0"` → `"0.1.1"` (patch — no rustclaw API breakage)
9. Commit + tag rustclaw

## Rollback (design §10.5)

- gid-core: `cargo yank --vers 0.4.0`, release `0.4.1` with revert.
- rustclaw: revert the upgrade commit; pin to `gid-core = "0.3.2"`. Old `ritual_runner.rs`
  is in git history at SHA `d940e8b^` (T12 entrypoint migration parent).

## Open items before T17 can execute

| Blocker | Owner | Path forward |
|---|---|---|
| T16 rustclaw integration tests not yet written | Coder (specialist) | Spawn after potato review of CHANGELOG/version draft |
| T17 §9.5 manual acceptance — engram ritual run | potato | Live run (this is the "you do this" task) |
| gid-cli version misalignment 0.3.1 vs 0.3.2 | Resolved by this bump (both → 0.4.0) | — |

## What this draft does NOT change yet

- **Does not edit** `crates/gid-core/Cargo.toml` or `crates/gid-cli/Cargo.toml`.
- **Does not** publish anything.
- **Does not** fill in the `[0.4.0]` release date.
- **Does not** delete the deprecated `V2ExecutorConfig.notify` shim — that lands
  in 0.5.0 per CHANGELOG note (a deliberate one-release deprecation window).

---

*Draft authored 2026-04-27 as part of ISS-052 T17 prep, after T14 cleanup landed.
Awaiting review before any version-bump commits.*
