---
id: "ISS-011"
title: "Walk-up 机制在 explicit project path 不存在时应报错而非静默回退"
status: closed
priority: P1
created: 2026-04-26
closed: 2026-04-25
---
# ISS-011: Walk-up 机制在 explicit project path 不存在时应报错而非静默回退

**Status:** resolved (2026-04-25)
**Priority:** P1

## Resolution

Fixed in rustclaw `src/tools.rs` (commits forthcoming). Two functions hardened:

- `GraphManager::resolve_external` — explicit `project` path that does not exist now bails with `"project path does not exist: ... (no walk-up fallback for explicit paths)"` instead of silently walking up to an ancestor `.gid/`.
- `GraphManager::resolve_gid_dir` — same check on the `project` field of tool input.

**Preserved behavior:** when an explicit path *exists* but has no local `.gid/` (e.g. caller passes `src/foo/`), upward walk still runs. This is the legitimate "find enclosing project from a subdirectory" pattern used by ritual and similar tools, covered by `test_resolve_external_falls_back_to_upward_walk_without_gid`.

**Tests added:**
- `test_resolve_external_nonexistent_path_errors` — basic error case
- `test_resolve_external_nonexistent_does_not_walk_up_to_ancestor` — regression for the original bug case (typo path under an ancestor with `.gid/`)
- `test_resolve_gid_dir_nonexistent_project_errors` — same coverage for `resolve_gid_dir`

**Note on scope:** gid-rs CLI does not expose a `--project` flag (only `--graph`, which already errors on nonexistent paths via `resolve_by_graph_path`). The bug surface was exclusively the rustclaw tool layer.

---

## Original Problem


当用户通过 `--project` 参数或 API 的 `project` 字段**显式指定**一个项目路径时，如果该路径不存在，gid 当前行为是**静默 walk-up**——向上遍历目录树寻找最近的 `.gid/` 目录，然后操作那个（错误的）graph。

这导致用户在完全不知情的情况下读写了**另一个项目的 graph**。

## Reproduction

```bash
# engramai 项目实际路径是 engram-ai-rust，但用户传了 engramai
gid tasks --project /Users/potato/clawd/projects/engramai

# 期望：报错 "project path does not exist"
# 实际：walk-up 找到 /Users/potato/clawd/.gid/graph.db，返回 OpenClaw 全局 graph 的数据
```

## Impact

- **数据污染**：对错误 graph 执行 `gid extract`/`gid add_task` 会写入错误项目
- **误导用户**：返回的 task 列表看起来正常但完全是另一个项目的
- **难以排查**：没有任何 warning 或 log 表明发生了 fallback

## Root Cause

`resolve_project_root()` 或等价函数的逻辑：

1. 检查指定路径是否有 `.gid/`
2. 如果没有 → 向上遍历父目录
3. 找到第一个有 `.gid/` 的目录 → 使用

问题在第 2 步：**当路径是用户显式指定的（非 CWD 推断），walk-up 是错误行为。**

## Proposed Fix

区分两种场景：

| 场景 | 行为 |
|------|------|
| **隐式**（无 `--project`，从 CWD 推断） | Walk-up ✅ 合理，等同于 git 的行为 |
| **显式**（`--project /some/path`） | 路径不存在 → **报错**；路径存在但无 `.gid/` → **报错**（或提示 `gid init`） |

### 实现要点

1. `resolve_project_root()` 增加参数 `explicit: bool`
2. 当 `explicit = true` 时：
   - `!path.exists()` → `Err("Project path does not exist: {path}")`
   - `!path.join(".gid").exists()` → `Err("No .gid/ found at {path}. Run 'gid init' first?")`
3. 当 `explicit = false` 时：保持现有 walk-up 行为
4. CLI 的 `--project` 和 API 的 `project` 字段传入时设 `explicit = true`

## Discovered

2026-04-17 — RustClaw 在操作 engramai 项目时传入了错误路径 `/Users/potato/clawd/projects/engramai`（正确路径是 `engram-ai-rust`），walk-up 机制静默回退到 `/Users/potato/clawd/.gid/graph.db`（OpenClaw 全局 graph），导致看到大量 `infer:component:*` 节点污染，排查花了一段时间才发现是路径错误。

## Related

- git 的行为参考：`git -C /nonexistent status` → 报错 `No such file or directory`，不会 walk-up
