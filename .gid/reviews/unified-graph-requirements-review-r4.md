# Review Round 4 (Final): unified-graph/requirements.md

> 最终 purpose 审查。前三轮修了 28 个问题。这轮从整体看：需求文档是否真正 serve 了 "统一图" 的目的？有没有遗漏的核心问题？

---

### 🔴 Critical

**FINDING-29: `node_type` vs `node_kind` 歧义 — 需求文档全程使用 `node_type` 存 code kind，但 Node struct 有两个字段**

现有 `graph::Node` 有：
- `node_type: Option<String>` — 注释: "task, file, component, feature, layer, etc."
- `node_kind: Option<String>` — 注释: "Code-level kind: Function, Struct, Impl, Trait, Enum, etc."

现有 `unified.rs` 对这两个字段的使用：
- `node_type`: 设为 "file", "class", "function", "module"（粗粒度分类，做了合并如 Class+Interface+Enum+Trait→"class"）
- `node_kind`: 设为 `format!("{:?}", code_node.kind)` = "Function", "Struct", "Trait" 等（原始 NodeKind 的 Debug 输出）

需求文档 GOAL-1.1 说 `node_type` 存 11 种 kind（function, struct, class, impl...），"不做 kind 合并"。

**问题**：
1. 这实际上是 `node_kind` 的语义（原始 code kind），不是 `node_type` 的语义（粗粒度类型分类）
2. 需求文档没有提到 `node_kind` 字段 — 是废弃它？还是保留？还是换用它？
3. GOAL-8.1 说 `node.node_type.as_deref() == Some("function")` — 但如果 struct 和 function 都叫自己的真名，query 代码里也要列 11 种才能过滤出 "所有代码节点"
4. GOAL-6.1 说通过 `node_type + source` 推导层归属 — 但如果 node_type=function，怎么知道它是 code 层？必须靠 `source == "extract"` 才行

**Suggested fix**: 文档需要明确 `node_type` 和 `node_kind` 的分工：
- 方案 A：**node_type 粗分类 + node_kind 细分类**。node_type 保持 {file, class, function, module, task, feature, component}（粗粒度，用于层判断），node_kind 存精确类型 {struct, trait, enum, impl, ...}（细粒度，用于精准过滤）。层判断用 `source == "extract"` 而非 node_type。
- 方案 B：**合并到 node_type**。废弃 node_kind，node_type 存 11 种精确类型。层判断完全靠 `source` 字段。代价：node_type 不再能区分 "粗类型"。
- 方案 C：**合并到 node_kind**。node_type 留给 project 层（task/feature/component），code 节点的 node_type 设为 None 或 "code"，细类型全走 node_kind。层判断最清晰：`node_type == Some("code")` 或 `source == "extract"`。

无论选哪个，需要在 GOAL-1.1 中明确。

---

**FINDING-30: code 节点的 NodeStatus 未指定 — 这是 R1 修过的问题但修得不完整**

R1 FINDING-5 指出 code 节点不应该有 `Todo` 状态（会污染任务列表）。但修复后的需求文档没有在任何 GOAL 中说明 code 节点应该设什么 status。

现有 `unified.rs` 设为 `NodeStatus::Done`。

但 `NodeStatus` 默认是 `Todo`（serde `#[serde(default)]`）。如果 extract 写入时忘了设 status（或设了 None → 反序列化 fallback 到 default），code 节点就变成 Todo，再次污染 `ready_tasks()`。

GOAL-6.8 通过在 `summary()` / `ready_tasks()` 中过滤 `source != "extract"` 来解决输出层，**但没解决数据层**。一个 code 节点在 graph.yml 里到底是什么 status？

**Suggested fix**: 在 GOAL-1.1 中加一句："代码节点的 `status` 字段设为 `Done`（代码存在 = 完成）。这个值仅为满足 NodeStatus required field 语义，不参与 project 层任务状态统计。" 或者引入 `NodeStatus::None` 变体，但改 enum 影响面大。

---

### 🟡 Important

**FINDING-31: `source` 字段是唯一的层判别标准，但 GOAL-6.1 说 "通过 node_type + source 推导"**

GOAL-6.1:
> 通过 `node_type` + `source` 推导

实际上 `source == "extract"` 就足够判断 code 层了。加 node_type 进来判断反而模糊 — 如果有人手动创建一个 node_type=file 的 project 节点怎么办？

而 GOAL-6.8、GOAL-1.3、GOAL-5.4 全部只用 `source == "extract"` 过滤。文档自身不一致。

**Suggested fix**: GOAL-6.1 改为 "通过 `source` 字段判断层归属：`source == \"extract\"` → code 层，其他 → project 层。`node_type` 用于层内细分类，不用于跨层判断。"

---

**FINDING-32: 桥接边是 feature→code 单向的 — 但跨层查询需要双向遍历**

GOAL-3.1 说生成 `feature --maps_to--> module:src/auth` 桥接边。方向是 feature→code。

GOAL-4.1 说从 task 出发查 impact，应该能到达 code 节点。路径是 task → (depends_on/implements) → feature → (maps_to) → code module → code file → function。这个方向 OK。

但反过来呢？从 code function 出发查 impact（"我改了这个函数，哪些 task 受影响？"），路径需要反向穿越 maps_to：function → file → module → (maps_to 反向) → feature → task。

当前 QueryEngine 的 `impact()` 是否支持反向边遍历？如果它只沿 `edges_from(node)` 走（即 from=node 的边），那 code→feature 方向没有边。

**Suggested fix**: 两个选择：
- A. 在 GOAL-3.1 中说明桥接边是双向的 — 同时生成 `feature→code` 和 `code→feature` 反向边
- B. 在 GOAL-4.1/4.2 中说明 QueryEngine 需要支持反向边遍历 — 对每个节点同时查 `edges_from(n)` 和 `edges_to(n)`
- 确认当前 QueryEngine 实现再选。如果已经支持双向遍历，在 GOAL-4.1 中注明 "依赖 QueryEngine 的双向遍历能力" 即可。

---

**FINDING-33: `gid tasks` 默认 `--layer project` 但 ready_tasks() 需要 GOAL-6.8 修改 — 两个机制在做同一件事**

GOAL-6.2 说 `gid tasks` 默认 `--layer project`（CLI 层过滤）。
GOAL-6.8 说 `summary()` / `ready_tasks()` 默认排除 code 节点（API 层过滤）。

这是 belt-and-suspenders（双保险）还是冗余？

如果 `gid tasks` 在 CLI 层就过滤了 project only，那 `ready_tasks()` 的过滤变成了 dead code path（CLI 永远不会传 code 节点进来）。

反过来，如果 `ready_tasks()` 在 API 层过滤了，CLI 的 `--layer project` 默认值对 tasks 命令就是多余的。

**问题**：其他直接调用 `graph.ready_tasks()` 的代码（harness、ritual executor）不走 CLI 层。所以 GOAL-6.8 是必要的 — 这些内部调用方不应该看到 code 节点的 ready tasks。

**Suggested fix**: 保留两者，但在 GOAL-6.8 中说明 rationale："API 层过滤保护内部调用方（harness、ritual），CLI 层过滤保护用户命令。两者互为补充，不是冗余。" 让 design 阶段清楚为什么需要两层。

---

### 🟢 Minor

**FINDING-34: GOAL-5.2 列出 4 个迁移调用方，GOAL-8.2~8.5 也列出了同样的 4 个 — 完全重复**

GOAL-5.2 说 "需要迁移的调用方包括" 然后列了 CLI, harness, ritual, rustclaw。
Module 8 的 GOAL-8.2~8.5 分别为每个调用方写了详细迁移 GOAL。

GOAL-5.2 变成了 GOAL-8.x 的摘要，不是独立要求。

**Suggested fix**: GOAL-5.2 删除调用方列表，改为引用 Module 8："具体迁移路径见 GOAL-8.2~8.5"。避免维护两份相同列表。

---

**FINDING-35: 缺少 "code 节点量级" 的预期 — 设计需要知道这个来做性能决策**

requirements 里 GUARD-4 说 "性能不劣化超过 20%"，GOAL-6.7 说 "<500ms 可接受"。但没有说一个典型项目的 code 节点量级。

一个中型 Rust 项目（gid-rs 自身）有多少 code 节点？extract 后 graph.yml 会膨胀到什么大小？这直接影响设计决策（是否需要 lazy loading、是否需要 binary format、YAML 是否撑得住）。

**Suggested fix**: 在 Overview 或 GOAL-6.7 附近加一行估算："典型项目规模预期 — 小型 <500 code 节点 / 中型 500-5000 / 大型 5000+。gid-rs 自身约 ~800 code 节点。YAML 后端目标支持到 5000 节点 <500ms 读取，超过此规模建议迁移 SQLite。"

---

### ✅ Passed Checks (big picture)

1. **核心 purpose 清晰** ✅ — "两个图合成一个" 的动机和收益表达准确
2. **extract 隔离** ✅ — GOAL-1.3 + GUARD-1 充分保护 project 节点
3. **边的所有权模型** ✅ — extract/auto-bridge/manual 三种来源明确
4. **迁移路径完整** ✅ — Module 8 覆盖了所有 6 个调用方
5. **向后兼容** ✅ — GOAL-7.1/7.2 合理
6. **跨层查询验收** ✅ — GOAL-4.1/4.2 作为集成测试条件而非代码变更
7. **分层过滤** ✅ — CLI 层 + API 层双保险
8. **bridge 机制** ✅ — 手动 code_paths 优先于自动匹配

### Summary
- Total findings: 7 (2🔴 2🟡+1 clarification 2🟢)
- **FINDING-29** 是最重要的 — node_type/node_kind 歧义会影响整个实现
- **FINDING-30** 是数据完整性问题 — status 不指定可能导致 regression
- **FINDING-32** 是功能正确性 — 反向跨层查询可能静默失败
- 其他是文档一致性/可读性
- Recommendation: 修完 FINDING-29~33 后可以进 design
