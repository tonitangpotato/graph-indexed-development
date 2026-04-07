# Review Round 3 (Final): unified-graph/requirements.md

> 最终审查。前两轮修了 21 个问题。这轮聚焦：逻辑完整性、edge cases、可实现性。

---

### 🔴 Critical

**FINDING-22: `gid tasks` / `summary()` 会被 code 节点污染 — 缺少 "默认过滤" GOAL** — ✅ Applied

`NodeStatus` 的 default 是 `Todo`。代码节点（file, function, class）写入 graph.yml 后，它们的 status 默认也是 `Todo`。

现有行为：
- `graph.summary()` 遍历所有 nodes 按 status 统计 → 5000 个 code 节点全部被计为 `todo`
- `graph.tasks_by_status(&Todo)` → 返回 code 节点
- `graph.ready_tasks()` → code 节点也出现（它们没有 `depends_on` 边，所以被认为 "ready"）
- `gid_tasks` tool → agent 看到 5000 个 "ready" 的 code 节点

GOAL-6.2 说 "支持 `--layer` 过滤参数（默认 `all`）"。**默认 `all` 意味着 `gid tasks` 不加参数就会显示 code 节点。** 这破坏了现有用户体验 — `gid tasks` 应该默认只显示 task 节点。

两种修法：
1. **改默认值**：`gid tasks` 的 `--layer` 默认为 `project`（不是 `all`）。只有 `gid tasks --layer all` 或 `--layer code` 才看到 code 节点。
2. **给 code 节点一个非 Todo 的 status** — 比如 `None` 或新的 `NodeStatus::NA`。但这改类型系统，影响面大。

**Suggested fix**: GOAL-6.2 改为 — "默认过滤值因命令而异：`gid tasks` 默认 `--layer project`，`gid visual` / `gid read` / `gid query` 默认 `--layer all`。" 加一条新 GOAL — "`graph.summary()` 和 `graph.ready_tasks()` 默认只统计 project 层节点（`source != "extract"`），或提供带 layer filter 的版本。"

---

**FINDING-23: 手动创建的边（非 extract、非 auto-bridge）没有 `metadata.source` — 清理逻辑的盲区** — ✅ Applied

GOAL-1.3 的清理逻辑依赖 `metadata["source"]`：
- `source == "extract"` → extract 管理
- `source == "auto-bridge"` → bridge 管理
- 没有 source → ？

用户手动添加的边（`gid add-edge`）和 `gid design --parse` 生成的边没有 `metadata.source`。如果一条手动边从 task 指向 code 节点（比如 `my-task --implements--> func:src/auth.rs:login`），增量 extract 删除 `func:src/auth.rs:login` 时，这条手动边的 `to` 指向一个不存在的节点 — 成为 dangling edge。

Requirements 没有定义这个 edge case：清理 code 节点时，要不要同时清理指向被删节点的非 extract/non-auto-bridge 边？

**Suggested fix**: GUARD-1 加注 — "extract 清理 code 节点时，如果存在其他 source 的边指向被删节点，这些边也应一并删除（级联清理 dangling edges）。此规则不适用于节点本身（只清理边，不清理非 extract 节点）。"

---

### 🟡 Important

**FINDING-24: GOAL-1.1 的 node_type 列表和 ISS-009 的 `BelongsTo` / module nodes 不完全对齐** — ✅ Applied

GOAL-1.1 列了 `file, class, function, module, trait, enum, constant, interface`。ISS-009 design 引入了 module nodes（目录 → `module:path`）和 `BelongsTo` edges。

但 ISS-009 还引入了一些 node_type：
- `impl` — Rust impl blocks
- `method` — 方法（vs 独立 function）

当前 extract parser 实际产出的 NodeKind（在 code_graph/mod.rs）：
```rust
Function, Struct, Impl, Trait, Enum, Variant, Constant, Interface, Module, Method, ...
```

GOAL-1.1 列的 node_type 没有 `struct`（只说了 `class`）、没有 `impl`、没有 `method`、没有 `variant`。

如果这些在转换时被忽略，就会丢数据。如果被映射（Struct→class, Method→function），就需要明确映射表。

**Suggested fix**: GOAL-1.1 改为 — "node_type 与 extract parser 的 NodeKind 一一对应：function, struct, class, impl, trait, enum, variant, constant, interface, module, method。不做 kind 合并或重命名 — 保持 extract 原始粒度。"

---

**FINDING-25: GOAL-3.4 `code_paths` 是 `metadata["code_paths"]` — 但没有定义格式** — ✅ Applied

`code_paths` 是个路径列表，但：
- 路径格式是什么？`src/auth/` 还是 `src/auth` 还是 `file:src/auth/mod.rs`？
- 是精确匹配还是前缀匹配？
- 是匹配 file 节点还是 module 节点还是两者？
- 通配符？`src/auth/**`？

**Suggested fix**: GOAL-3.4 加 — "code_paths 格式为相对路径列表（如 `[\"src/auth\", \"src/storage/sqlite.rs\"]`），使用前缀匹配。匹配所有 code 层节点的 `file_path` 或 `id` 中包含该路径前缀的节点。不支持通配符（简化实现）。"

---

**FINDING-26: GOAL-8.1 把 working_mem 改为基于 Graph — 但 working_mem 的核心数据结构是 CodeGraph 的** — ✅ Applied

`working_mem.rs` 不只是接受 `&CodeGraph` 参数 — 它内部重度使用 `CodeNode`、`NodeKind`、`EdgeRelation` 类型做模式匹配：

```rust
match node.kind {
    NodeKind::Function | NodeKind::Method => ...,
    NodeKind::Struct | NodeKind::Class => ...,
}
```

迁移到 `&Graph` 后，这些变成字符串匹配 `node.node_type.as_deref() == Some("function")`。不仅 verbose，而且失去了编译期 exhaustiveness 检查。

两种选择：
1. 接受字符串匹配（简单但 fragile）
2. 在 graph.rs 里加一个 `NodeType` enum，node_type 从 `Option<String>` 变成 `Option<NodeType>`（干净但影响面大）

Requirements 应该明确选哪条路，不然 implementer 会各自发明。

**Suggested fix**: GOAL-8.1 加注 — "迁移后使用 `node_type` 字符串匹配（`node.node_type.as_deref() == Some("function")`）。Node 类型的枚举化（String → NodeType enum）列为 P2 优化，与 GOAL-5.3 的 CodeGraph 消除同一批完成。"

---

### 🟢 Minor

**FINDING-27: GOAL 编号 1.6 在 1.3 和 1.4 之间 — 违反 "sequential, no gaps" 规则** — ✅ Applied

文档里 Module 1 的顺序是：1.1, 1.2, 1.3, **1.6**, 1.4, 1.5。这是上一轮 apply 时插入的新 GOAL 没有重编号导致的。

**Suggested fix**: 重编号为 1.1 → 1.6 顺序排列。当前的 1.6 内容移到 1.4 位置，原 1.4 变 1.5，原 1.5 变 1.6。

---

**FINDING-28: Module 8 在 Guards 和 Out of Scope 之后 — 文档结构不标准** — ✅ Applied

标准 requirements 格式：Goals（所有 modules）→ Guards → Out of Scope → Dependencies。Module 8 被放在 Out of Scope 之后，读起来像是 afterthought。

**Suggested fix**: 把 Module 8 移到 Module 7 之后、Guards 之前。所有 Goals 在一起。

---

### ✅ Passed Checks

- **概念一致性** ✅ — 两层模型 + semantic 标注维度，定义清晰，无矛盾
- **source 隔离** ✅ — GOAL-1.2/1.3 的 `metadata["source"]` 机制完整
- **桥接策略** ✅ — code_paths P0 + auto-match P1 fallback，优先级正确
- **QueryEngine 复用** ✅ — GOAL-4.1/4.2 正确识别无需代码改动
- **向后兼容** ✅ — GOAL-7.1/7.2 覆盖
- **调用方迁移** ✅ — Module 8 覆盖了所有 6 个依赖 CodeGraph 的模块
- **与 ISS-006/009 兼容** ✅ — 增量逻辑和跨层边设计与现有实现一致
- **与 SQLite migration 兼容** ✅ — 独立并行，FINDING-16 的注释已加
- **atomic write** ✅ — GUARD-2 覆盖
- **GOAL 可验证性** ✅ — 每个 GOAL 都有具体的测试条件

---

### 📊 Summary

| Category | Count |
|---|---|
| 🔴 Critical | 2 (FINDING-22, 23) |
| 🟡 Important | 3 (FINDING-24, 25, 26) |
| 🟢 Minor | 2 (FINDING-27, 28) |
| ✅ Passed | 10 checks |

**整体评价**: 文档经过两轮修复后已经很完整。这轮主要是实现层面的 edge cases — status 污染是最大问题（不修会让 `gid tasks` 不可用），其次是 dangling edge 和 node_type 对齐。修完这 7 个就可以进 design 了。
