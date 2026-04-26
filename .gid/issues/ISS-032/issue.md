---
id: "ISS-032"
title: "`gid validate` Detects Issues But Cannot Repair — No `gid repair` / `gid clean-orphans` Command"
status: closed
priority: P2
created: 2026-04-26
---
# ISS-032: `gid validate` Detects Issues But Cannot Repair — No `gid repair` / `gid clean-orphans` Command

**发现日期**: 2026-04-23
**发现者**: potato + RustClaw
**组件**: gid-cli, `commands/validate.rs`; gid-core (new repair module needed)
**优先级**: P1
**Status:** closed
**类型**: feature-gap
**标签**: cli, validation, ux, tooling

---

## ✅ Resolution (2026-04-26)

Implemented `gid repair` command. See bottom of file for closure note.

---

## 症状

`gid validate` 忠实地报告图健康问题：

```
✗ 7 issues found:
  Orphan nodes (no edges): iss-fts-corruption, iss-002, file:lib.rs
  Duplicate node IDs: method:compiler/types.rs:TopicId.from, method:compiler/types.rs:ConflictId.from
  Duplicate edge: method:compiler/types.rs:TopicId.from → class:compiler/types.rs:TopicId (defined_in)
```

然后……没了。没有 `--fix`，没有 `gid repair`，没有 `gid clean-orphans`。用户只有三条路：

1. 手写 SQL 直接改 `.gid/graph.db` — 违反 AGENTS.md 规则，风险大
2. 一条一条 `gid remove-node <id>` / `gid remove-edge <from> <to>` — 对几十/几百条 orphan 不现实
3. 删掉 graph 重跑 extract — 丢失所有手写的 task/feature 节点、history snapshot 等非代码数据

**这是 UX gap：gid 把诊断和治疗的工具分在了诊断侧，治疗侧空缺。**

## 真实踩坑场景（2026-04-23）

RustClaw 在 engram monorepo 上手滑跑了一次无 LSP 的 `gid extract`，污染主图：

- Before: 25 nodes / 28 edges（project layer only）
- After pollution: 2731 nodes / 11898 edges（2706 tree-sitter code nodes + 大量 orphan-prone edges）

回滚要两步：
1. `DELETE FROM nodes WHERE node_type='code'` — 删 code 节点
2. `DELETE FROM edges WHERE from_node NOT IN (SELECT id FROM nodes) OR to_node NOT IN (...)` — 清孤儿边

第 2 步本应是 `gid clean-orphans` 或 `gid validate --fix --orphans` 一条命令。现状是必须直接写 sqlite，绕过 gid 自己的数据模型。

## 根因（Root Cause）

设计上 validate 被定位为 **纯诊断（read-only）**：

```
// commands/validate.rs 大意
fn run(...) -> Result<()> {
    let issues = detect_issues(&graph);
    print_issues(&issues);
    if !issues.is_empty() { exit(1); }
}
```

"修"这个语义被假设为用户的手动动作。对早期小图这成立，对已经积累数据的图（merge、refactor、failed extract 后）不成立——用户需要一个**受控的修复路径**，否则只能用 raw SQL 绕过整个抽象层。

## 副问题（一起考虑）

1. **CASCADE 依赖 FK 开启**：SQLite schema 里 edges 表对 nodes 的外键引用如果启用 `ON DELETE CASCADE`，删 node 会自动清 orphan edge。见 ISS-033（相关问题，单独开）。即便 CASCADE 开启，已经存在的 orphan 仍需清理一次。
2. **Duplicate nodes/edges 不是一次写入造成**：多半来自重复 extract（不同 backend 或不同 --no-lsp 标志）。`gid_extract` 应当先做一致性检查（见建议"防御性 pre-extract"）。
3. **Orphan issue node 是另一回事**：`iss-002` 这种项目层节点没边 —— 不是数据损坏，是"还没建立关联"。`repair` 应该能区分"code-layer orphan edge（应删）"vs"issue/task orphan node（可能是正常待建连）"。

## 修复方案（Clean & Elegant）

### 1. 新增 `gid repair` 子命令（主方案）

```
gid repair                          # 交互式，列出可修复问题，让用户选
gid repair --orphans                # 只清 orphan edges（最安全，无歧义）
gid repair --duplicates             # 合并重复 node/edge（需要 merge 策略）
gid repair --all --dry-run          # 预览所有修复动作
gid repair --all --yes              # 执行所有（non-interactive，CI 用）
```

关键设计：
- **所有 repair 动作先备份**：自动 `cp graph.db graph.db.before-repair-<timestamp>`
- **每类问题独立 flag**：orphan/duplicate/cycle/dangling-ref 分开，避免"我只想清 orphan 结果它合并了我的 duplicate 节点"
- **Dry-run 必须默认可用**：`--dry-run` 输出"将会删除 N 条 orphan edge、合并 M 对 duplicate"，不动 DB
- **合并策略需显式**：duplicate 合并涉及保留哪个节点、如何重写引用它的边——默认应拒绝并让用户选（`--merge-strategy=keep-first|keep-latest|keep-with-edges`）

### 2. `gid validate --fix` 作为 thin wrapper

`validate --fix` 等价于 `validate` + 对可自动修的（orphan edges，no-controversy duplicates）调 repair 的对应能力。这样两个命令都在，职责清晰：
- `validate`：只看不动
- `repair`：动手但每步可控
- `validate --fix`：最简单场景的快捷方式

### 3. Repair 接口下沉到 gid-core

不能只在 CLI 层做。repair 能力应当在 `gid-core::repair` 模块里暴露为公共 API，这样：
- RustClaw / 其他嵌入 gid 的工具可以直接调（不必 shell out）
- 未来的 `gid_repair` MCP/framework tool 可以包装
- 测试可以在 core 层写，不必依赖 CLI

建议签名（草案）：
```rust
pub struct RepairPlan {
    pub orphan_edges: Vec<(NodeId, NodeId, EdgeKind)>,
    pub duplicate_nodes: Vec<Vec<NodeId>>,
    pub duplicate_edges: Vec<(NodeId, NodeId, EdgeKind)>,
    // ...
}

pub fn plan_repair(graph: &Graph) -> RepairPlan;
pub fn execute_repair(graph: &mut Graph, plan: &RepairPlan, opts: RepairOpts) -> RepairReport;
```

## 验收标准

- [ ] `gid repair --help` 存在，列出 `--orphans`, `--duplicates`, `--dry-run`, `--yes`, `--backup`
- [ ] `gid repair --orphans --dry-run` 输出待清理的 orphan edge 数量，不改 DB
- [ ] `gid repair --orphans` 自动备份 `.gid/graph.db.before-repair-<ts>`，清掉 orphan edges，报告 changes
- [ ] 在 RustClaw engram 案例上可复现：跑一次无 LSP extract → validate 报 orphan → `gid repair --orphans` 一键清理，不需要 sqlite3
- [ ] `gid validate --fix`（如果加）等价于 `repair --orphans --yes`（只做 safe 子集）
- [ ] 单元测试覆盖：orphan 清理、duplicate 合并各种策略、dry-run 不改数据、backup 路径存在

## 影响范围

- gid-cli: 新增 `commands/repair.rs`，修改 `validate.rs`（可选加 `--fix`）
- gid-core: 新增 `repair` 模块，公共 API
- 文档：README "Graph Maintenance" 一节；AGENTS.md 里 RustClaw 的 GID 规则（当前 rule 6 允许 sqlite3 做 orphan 清理的 "exception" 可以删掉）
- 向后兼容：纯新增，无破坏

## 关联

- ISS-033: PRAGMA foreign_keys 默认未开，CASCADE 不生效（姐妹 issue）
- ISS-015: FK enforcement 在 migration batch 内是 no-op（已修）
- 触发案例：RustClaw memory/2026-04-23.md "GID usage rules added to AGENTS.md (23:20)"

---

## ✅ Closure Note (2026-04-26)

**Implemented**: `gid repair` command with full feature set per spec.

**Files changed:**
- `crates/gid-core/src/repair.rs` (NEW, 380 LoC + 12 tests) — pure logic: `RepairOptions`, `RepairPlan`, `RepairReport`, `plan_repair()`, `apply_repair()`
- `crates/gid-core/src/lib.rs` — module + re-exports
- `crates/gid-cli/src/main.rs` — `Commands::Repair { ... }` variant + `cmd_repair_ctx()` + `backup_graph_file()`

**Capabilities delivered:**
- `gid repair --all --dry-run` — preview plan
- `gid repair --all --yes` — CI mode (no prompt)
- `gid repair --orphan-edges` etc. — selective fixes
- Default interactive: shows plan, prompts `[y/N]` before applying
- Auto-backup to `graph.{db,yml}.backup-<unix-ts>` before apply (skip with `--no-backup`)
- `--json` output for both dry-run and apply modes
- Both YAML and SQLite backends supported (uses `ctx.save()` + `std::fs::copy()` for backup)

**Safety design (副问题 #3 resolved):**
Orphan node removal is restricted to `SAFE_ORPHAN_NODE_TYPES` = code/file/function/method/class/module/trait/struct/enum. User-authored types (task/issue/feature/component) are SKIPPED with a transparency note in the plan. Tasks without edges often = "not yet linked", not "stale data".

**Test coverage**: 12 unit tests in `repair::tests` covering each repair category individually + combined; all pass. Total gid-core test count: 1095 → 1107.

**Manual verification** (both backends):
- YAML: synthetic graph with 5 issue categories → plan shows 5 changes + 1 skipped → apply → re-validate clean except for preserved orphan task ✓
- SQLite: same flow on graph.db → backup file created at `graph.db.backup-<ts>` ✓
- JSON mode: both dry-run and apply produce structured output ✓
- Guard: no flags → clear error message ✓

**What was NOT done (intentionally out of scope):**
- Cycle breaking — requires choosing which edge to drop, which is a domain decision; not safely auto-repairable
- Orphan task/issue auto-removal — would risk data loss; user must use `gid refactor delete` explicitly

**Branch**: `iss-001-002-revive` (continuing the revive batch)
