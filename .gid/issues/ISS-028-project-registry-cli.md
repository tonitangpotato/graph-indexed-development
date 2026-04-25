# ISS-028: gid CLI `project` subcommand + `~/.config/gid/projects.yml`

**Status:** open
**Created**: 2026-04-23
**Reporter**: potato
**Severity**: medium
**Parent feature**: feature-project-registry
**Resolves**: ISS-020 (project path discovery friction)

---

## Problem

Every cross-project session wastes 3-6 tool calls discovering project paths. No stable `name → path` mapping exists. See `rustclaw/.gid/issues/ISS-020-project-path-discovery-friction.md` for the full symptom writeup.

## Decision (agreed with potato 2026-04-23)

**gid is the owner** of the "which projects exist on this machine" registry. Not rustclaw, not agentctl. Rationale:
- `.gid/` is gid's directory → gid owns "who has a .gid/"
- rustclaw / agentctl are *consumers* of this info
- XDG-compliant location (`~/.config/gid/`) → Linux portable, dotfiles-friendly
- Beats the meta-workspace hack (would hard-bind to `/Users/potato/clawd/`)

## Deliverable

### 1. Registry file

Location: `$XDG_CONFIG_HOME/gid/projects.yml`, falling back to `~/.config/gid/projects.yml`.

Schema (v1):

```yaml
version: 1
projects:
  - name: engram
    path: /Users/potato/clawd/projects/engram
    aliases: [engram-ai, ea]
    default_branch: main
    tags: []          # optional
    archived: false   # optional
    notes: ""         # optional
```

### 2. CLI subcommand

```
gid project list
gid project resolve <ident>      # prints path, exit 1 if not found
gid project add <name> <path>    # validates .gid/ exists under path
gid project remove <name>
gid project where                # prints registry file path
```

### 3. Resolution rules

- `<ident>` matches `name` first, then `aliases` (both case-insensitive)
- Issue references use `project:issue` format (e.g. `engram:ISS-022`) — this subcommand only resolves the project portion
- Alias collision across projects → error listing all candidates (no silent "first match wins")

## Acceptance

- Unit tests: load/save, add/remove, resolve (name + alias), not-found errors, missing-`version`-field migration
- `gid project resolve engram` prints `/Users/potato/clawd/projects/engram` on a seeded registry
- Zero dependency on gid-core graph logic — pure YAML I/O + CRUD
- Initial seed: manual one-time import from `rustclaw/MEMORY.md` "Canonical Project Roots" section

## Scope

- **In**: CLI, registry file, resolution logic, unit tests
- **Out**: ritual integration (ISS-029), rustclaw tool changes (ISS-030)

## Dependencies

None. Can proceed in parallel with ISS-029.
