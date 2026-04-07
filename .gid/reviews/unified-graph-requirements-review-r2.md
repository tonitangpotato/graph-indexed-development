# Review Round 2: unified-graph/requirements.md

> 基于完整阅读 gid-rs 全部文档后的深度审查。上一轮修复了表面问题（11 findings），这轮关注：
> 架构一致性、与现有系统的冲突、遗漏的影响面、概念完整性。

---

## 阅读的文档
- docs/DESIGN.md — gid-rs 整体架构和愿景
- docs/PRODUCT-ROADMAP.md — 产品路线图（LSP → SQLite → 产品化 → SaaS）
- docs/requirements.md — harness requirements（60+ GOALs）
- .gid/features/STATUS.md — 所有 feature 状态
- .gid/features/core/requirements.md — 核心图引擎
- .gid/features/code-intel/requirements.md — 代码智能
- .gid/features/sqlite-migration/requirements.md — SQLite 迁移（53 GOALs + 10 GUARDs）
- .gid/features/incremental-extract/DESIGN.md — 增量提取设计
- .gid/features/iss-009-cross-layer/design.md — 跨层连接设计
- crates/gid-core/src/unified.rs — 当前运行时合并逻辑
- crates/gid-core/src/query.rs — QueryEngine
- crates/gid-core/src/graph.rs — Node/Edge/Graph 类型
- crates/gid-core/src/working_mem.rs — WorkingMemory（基于 CodeGraph）
- crates/gid-core/src/semantify.rs — 语义层标注
- crates/gid-core/src/harness/scheduler.rs — harness 中的 extract 调用
- crates/gid-core/src/ritual/executor.rs — ritual 中的 extract 调用
- crates/gid-cli/src/main.rs — CLI extract 命令
- rustclaw/src/tools.rs — RustClaw agent tools 中的 gid 调用

---

### 🔴 Critical

**✅ FINDING-12: working_mem.rs 整个模块基于 CodeGraph 类型 — 需要迁移** [Applied]

`working_mem.rs`（~850 行）的核心函数全部接受 `&CodeGraph` 参数：
- `query_gid_context(files_changed, graph: &CodeGraph)`
- `analyze_impact(files_changed, graph: &CodeGraph)`
- `find_test_files(graph: &CodeGraph, ...)`
- `collect_function_info(graph: &CodeGraph, ...)`

统一图之后，调用方不再持有 `CodeGraph`，只有 `Graph`。这意味着：
1. RustClaw 的 `GidWorkingMemoryTool` 目前调 `CodeGraph::extract_from_dir()` → `working_mem::query_gid_context()` — 这整条路径需要改
2. 要么 `working_mem` 改为接受 `&Graph`（过滤 source=extract 的节点），要么保留 `CodeGraph` 作为中间类型让 working_mem 内部用

Requirements 的 GOAL-5.3 说 "CodeGraph 作为 extract 内部中间表示"，但 working_mem 不是 extract 内部 — 它是独立的查询模块。如果 CodeGraph 只是 extract 内部类型，working_mem 就没有输入了。

**Suggested fix**: 加 GOAL — `working_mem.rs` 迁移到基于 `Graph` 类型操作（按 `source == "extract"` / `node_type` 过滤代码节点）。或者保持 CodeGraph 作为公开查询类型（不只是 extract 内部），但这和 "统一图" 理念矛盾。

**Applied**: 新增 GOAL-8.1 — `working_mem.rs` 模块迁移到基于 `&Graph` 类型操作，列出 4 个需要迁移的函数。

---

**✅ FINDING-13: harness scheduler 和 ritual executor 直接调 `CodeGraph::extract_from_dir()` + `build_unified_graph()` — 需要迁移路径** [Applied]

harness/scheduler.rs (line 492):
```rust
let code_graph = CodeGraph::extract_from_dir(&src_dir);
let unified = build_unified_graph(&code_graph, graph);
```

ritual/executor.rs (line 466-468):
```rust
let code_graph = crate::code_graph::CodeGraph::extract_from_dir(&src_dir);
let unified = crate::unified::build_unified_graph(&code_graph, &graph);
```

这两处是 harness 的 "post-layer extract"（GOAL-2.18/2.19）。统一图后，应该直接调新的 extract（写 graph.yml），然后重新 load graph — 不再需要 build_unified_graph。

Requirements GOAL-5.2 说 deprecated `build_unified_graph()`，但没有明确提到 harness 和 ritual 这两个调用方的迁移。

**Suggested fix**: 在 GOAL-5.2 或新 GOAL 中明确列出需要迁移的调用方：
1. `gid-cli/src/main.rs` cmd_extract
2. `harness/scheduler.rs` post_layer_extract
3. `ritual/executor.rs` extract 阶段
4. `rustclaw/src/tools.rs` GidExtractTool, GidSchemaTool, GidComplexityTool, GidWorkingMemoryTool

**Applied**: 更新 GOAL-5.2 列出所有 4 个调用方，并新增 GOAL-8.2/8.3/8.4 详细说明 harness 和 ritual 的迁移路径。

---

**✅ FINDING-14: RustClaw tools.rs 有 6 处直接调 CodeGraph — 是最大的外部消费方** [Applied]

```
tools.rs:3496  CodeGraph::extract_from_dir(dir_path)     // GidExtractTool
tools.rs:3513  build_unified_graph(&code_graph, &existing) // GidExtractTool
tools.rs:3618  CodeGraph::extract_from_dir(dir_path)     // GidSchemaTool
tools.rs:3621  code_graph.get_schema()                    // GidSchemaTool
tools.rs:4092  CodeGraph::extract_from_dir(dir_path)     // GidComplexityTool
tools.rs:4194  CodeGraph::extract_from_dir(dir_path)     // GidWorkingMemoryTool
```

其中 `get_schema()` 和 `assess_complexity_from_graph()` 也是 CodeGraph 上的方法。统一图后需要：
- `get_schema()` 改为从 Graph 的 code 层节点提取
- `assess_complexity_from_graph()` 同上
- 或者在 gid-core 里提供 `Graph → CodeGraph` 的反向提取（但这违反统一方向）

Requirements 没有覆盖 `schema` 和 `complexity` 这两个基于 CodeGraph 的分析功能的迁移。

**Suggested fix**: 加 GOAL 或在 Out of Scope 中明确说明 — schema/complexity 分析模块的 CodeGraph→Graph 迁移是本 feature 的一部分（推荐），还是独立后续任务。

**Applied**: 新增 GOAL-5.4 — `gid schema` 从 graph.yml 的 code 层节点生成输出，`get_schema()` 和 `assess_complexity_from_graph()` 基于 Graph 的 code 层节点工作。新增 GOAL-8.5 明确 RustClaw tools.rs 中 6 处调用的迁移路径。

---

### 🟡 Important

**✅ FINDING-15: GOAL-6.2 的 `--layer` 过滤在 YAML 场景下 = 全量加载后过滤** [Applied]

requirements 说 "过滤在图加载后、输出前应用，不影响底层数据"。但 YAML 模式下，加载就意味着把整个 graph.yml 反序列化到内存。5000+ code 节点 = YAML 文件可能 2-5MB，每次 `gid tasks`（只需 project 层）都要先 parse 整个文件。

GOAL-6.7 说 "不应超过 2x"，但没有定义基线。如果现在 `gid tasks` 对 50 节点图是 10ms，加了 5000 code 节点变成 200ms，是 20x — 违反了 GUARD 但也没人测过。

这不是说 requirements 需要加什么 — 而是应该明确承认这是 YAML 后端的固有限制，SQLite 迁移（已有独立 feature）是根本解法。可以在 Out of Scope 或 GOAL-6.7 的注释里加这个 context。

**Suggested fix**: GOAL-6.7 加注 — "YAML 后端下，过滤在全量加载后应用（序列化/反序列化开销不可避免）。SQLite 后端可通过 WHERE 子句实现真正的按层查询，跳过不需要的节点。YAML 模式下的性能劣化在可接受范围内（<500ms for typical projects），大型项目应迁移到 SQLite。"

**Applied**: 在 GOAL-6.7 添加注释说明 YAML 后端的性能限制及 SQLite 解决方案。

---

**✅ FINDING-16: 与 sqlite-migration requirements 的交叉 — code_graph.json 的 Out of Scope 矛盾** [Applied]

sqlite-migration/requirements.md 最后一行明确写：
> "Code extraction changes — the extractor continues writing code-graph.json; merging extracted data into SQLite is a separate concern handled by gid extract pipeline"

这和 unified-graph 的 GOAL-1.5（"code_graph.json 不再生成"）直接矛盾。不是设计问题，而是文档一致性 — sqlite-migration 的 Out of Scope 条目基于旧假设。

**Suggested fix**: 在 Dependencies 或注释里明确 — "本 feature 完成后，sqlite-migration 的 Out of Scope 中关于 code_graph.json 的条目不再适用"。或者直接去 sqlite-migration/requirements.md 更新那一行。

**Applied**: 在 Dependencies 部分添加注释说明本 feature 完成后需同步更新 sqlite-migration/requirements.md 的 Out of Scope 条目。

---

**✅ FINDING-17: `code_node_to_task_id()` 的 ID 转换是信息损失** [Applied]

unified.rs 中的 `code_node_to_task_id()` 把 `file:src/main.rs` 转成 `code_src_main.rs` — 用下划线替换所有 `/` 和 `:`。这是因为 task graph 的 ID 不允许这些字符吗？不是 — graph.rs 的 `Node.id` 就是 String。

这个转换造成了：
1. 信息损失 — `code:src_main.rs` 看不出原始类型是 file/class/func
2. ID 冲突风险 — `file:src/main.rs` 和 `class:src/main.rs` 转换后相同
3. 和原始 code graph 的 ID（`file:src/main.rs`）不一致

统一图应该保留原始 ID（`file:src/main.rs`），不需要转换。requirements 的 GOAL-1.1 说 "node_type 对应 code kind"，但没有明确说 ID 保持原始格式。

**Suggested fix**: GOAL-1.1 中明确 — 代码节点的 ID 保持 extract 原始格式（`file:path`, `func:path:name`, `module:path`），不做 ID 转换。

**Applied**: 在 GOAL-1.1 添加说明 — 代码节点 ID 保持 extract 原始格式，不做 ID 转换，deprecated `code_node_to_task_id()` 不再使用。

---

**✅ FINDING-18: extract-meta.json 的位置和内容没更新** [Applied]

ISS-006 的 `extract-meta.json` 存储每个文件的 `node_ids: Vec<String>` 用于增量清理。这些 node_ids 目前指向 CodeGraph 的 ID。统一图后，同样的 ID 对应 Graph 中的节点。

如果 GOAL-1.1 决定保留原始 ID（FINDING-17），那 extract-meta.json 无需改动。但如果 ID 方案变了，extract-meta.json 需要同步更新。

另外，extract-meta.json 目前在 `.gid/extract-meta.json`。它追踪 code_graph.json 的元数据。统一后它追踪 graph.yml 中 code 层的元数据 — 位置不变，但语义变了。Requirements 应该提一句。

**Suggested fix**: 在 GOAL-1.3 或新增 GOAL 中说明 — `extract-meta.json` 继续使用，node_ids 对应 graph.yml 中 source=extract 的节点 ID。

**Applied**: 新增 GOAL-1.6 说明 extract-meta.json 继续使用，语义从 "code_graph.json 节点" 变为 "graph.yml code 层节点"。

---

**✅ FINDING-19: `gid schema` 命令完全基于 CodeGraph — 统一图后怎么办？** [Applied]

`gid schema` 目前调 `CodeGraph::extract_from_dir()` 然后调 `code_graph.get_schema()` 返回文件/类/函数的结构摘要。这是 agent 频繁使用的命令（用于理解代码结构）。

统一图后，`gid schema` 应该从 graph.yml 读 code 层节点生成 schema，不需要重新 extract。这会快很多（读文件 vs tree-sitter 解析）。

但 requirements 没有覆盖这个迁移。

**Suggested fix**: 加 GOAL — `gid schema` 从 graph.yml 的 code 层节点生成输出（如果 graph 已包含 code 节点），只在 graph 为空时 fallback 到 extract。

**Applied**: 在 GOAL-5.4 中说明 `gid schema` 从 graph.yml 的 code 层节点生成输出，仅在 graph 无 code 节点时 fallback 到 extract。

---

### 🟢 Minor

**✅ FINDING-20: GOAL-3.1 的 `metadata["path"]` 未被任何其他 feature 使用** [Applied]

GOAL-3.1 说 "feature node 的 `id` 或 `metadata["path"]`"。但 feature 节点目前没有 `metadata["path"]`（只有 `metadata["design_doc"]`）。这个字段从哪来？是假设用户手动设的？还是 `gid design --parse` 会生成？

如果没有来源，这个匹配条件是空谈。

**Suggested fix**: 要么删掉 `metadata["path"]` 引用（只留 ID 匹配），要么在 GOAL-3.1 中说明 `metadata["path"]` 是 `gid design --parse` 应该生成的字段（并加一个 GOAL 或注释在 design --parse 那边）。

**Applied**: 在 GOAL-3.1 中移除 `metadata["path"]` 引用，只保留 ID 匹配（基于前缀匹配）。

---

**✅ FINDING-21: GOAL-6.4 中 `--layer code` 和 `--layer project` 的边分类不完整** [Applied]

"代码边" = imports, calls, inherits 等。"项目边" = depends_on, implements, satisfies 等。但桥接边 `maps_to` 是跨层的 — 它属于哪个 layer？

如果 `--layer code` 不包含 `maps_to`，那从 code 节点出发永远到不了 project 节点（正确行为？）。如果 `--layer project` 不包含 `maps_to`，那从 task 出发到不了 code（也合理？）。只有 `--layer all` 包含 `maps_to`。

这个行为是对的，但需要明确写出来。

**Suggested fix**: GOAL-6.4 加一句 — "`maps_to` 桥接边仅在 `--layer all` 模式下参与遍历，`code` 和 `project` 模式下排除。"

**Applied**: 在 GOAL-6.4 中添加说明 — `maps_to` 桥接边仅在 `--layer all` 模式下参与遍历，`code` 和 `project` 模式下排除跨层边。

---

### 📊 Summary

| Category | Count |
|---|---|
| 🔴 Critical | 3 (FINDING-12,13,14) |
| 🟡 Important | 5 (FINDING-15,16,17,18,19) |
| 🟢 Minor | 2 (FINDING-20,21) |

### 整体评价

**核心设计依然正确** — 一个图、两层、source 隔离。上一轮修复后的文档在概念上是 clean 的。

**这轮发现的本质问题**：requirements 只定义了 "extract 写统一图" + "查询能跨层"，但**没有覆盖统一图带来的连锁迁移影响**。GID 不是只有 extract → graph.yml 这一条路径。至少 6 个模块直接依赖 CodeGraph 类型：

1. `working_mem.rs` — 影响分析（~850 行）
2. `harness/scheduler.rs` — post-layer extract
3. `ritual/executor.rs` — extract 阶段
4. `rustclaw/tools.rs` — 6 处调用
5. `complexity.rs` — 复杂度分析
6. `code_graph/analysis.rs` — impact analysis

不在 requirements 里规划这些迁移 = 实现时才发现连锁反应 = 要么半途而废要么硬编码 workaround = tech debt。

**建议**：把 "调用方迁移" 作为一个 GOAL 组（Module 8），列出所有需要从 CodeGraph 迁移到 Graph 的调用方。这才是 "no tech debt" 的做法。
