# Requirements: Unified Graph

## Overview

GID 的核心设计是 **一个图包含所有层** — 代码结构、语义标注、项目管理（feature/task/component）全部在同一个 graph.yml 中。当前实现偏离了这个设计：代码节点存在 `code_graph.json`，项目节点存在 `graph.yml`，运行时通过 `unified.rs` 做临时合并，结果不持久化。这导致查询断层 — 无法从一个 task 一路追踪到受影响的源码文件，`impact` / `deps` 只能在各自的图内工作。

本 feature 实现真正的统一图：代码结构和项目管理共享同一个持久化图，支持跨层查询和分层控制。

## Priority Levels

- **P0**: Core — 统一图的基本能力
- **P1**: Important — 自动化和质量保证
- **P2**: Enhancement — 便利性和可观测性

## Guard Severity

- **hard**: Violation = 数据丢失或图损坏
- **soft**: Violation = 功能降级但不破坏

## Goals

### 1. Extract 输出统一 (extract-target)

- **GOAL-1.1** [P0]: `gid extract` 将代码节点写入 `graph.yml` 而非 `code_graph.json`。每个代码节点携带层归属标识和精确种类标识（如 function/struct/module），两者语义不重叠。验证：从 graph.yml 加载后，能通过层标识过滤出全部代码节点，且种类标识保留 extract 原始粒度。

- **GOAL-1.2** [P0]: 代码边（imports, calls, inherits, defined_in, tests_for, belongs_to, overrides, implements）写入 `graph.yml` 的 `edges` 列表。所有 edge relation 统一格式，与现有 project 边风格一致。增量操作能按来源识别并选择性更新代码边。

- **GOAL-1.3** [P0]: 增量 extract（`extract_incremental`）按层隔离操作 — 只增删改代码层节点及其关联边，不触碰 task/feature/component 节点。验证：对含有 5 个 task 节点 + 10 个 code 节点的 graph.yml 跑增量 extract，task 节点数量和内容不变。

- **GOAL-1.4** [P1]: `extract-meta.json` 继续使用，其 `node_ids` 对应 graph.yml 中代码层节点 ID。位置保持 `.gid/extract-meta.json` 不变。

- **GOAL-1.5** [P1]: 全量 extract（`--force`）同样只清除代码层节点，不做全图清空。

- **GOAL-1.6** [P1]: `code_graph.json` 不再生成。旧的 `code_graph.json` 文件不自动删除（避免数据丢失），但 extract 不再读写它。

### 2. Semantify 自动集成 (auto-semantify)

- **GOAL-2.1** [P0]: `gid extract` 完成后自动运行 semantify，给代码节点打架构层标签。无需手动跑 `gid semantify`。

- **GOAL-2.2** [P1]: `gid extract --no-semantify` 跳过自动 semantify（用于调试或性能敏感场景）。

### 3. 桥接边自动生成 (bridge-edges)

- **GOAL-3.1** [P1]: 当 feature 节点未手动指定关联代码路径（见 GOAL-3.4）时，extract 完成后自动生成 feature→code 桥接边作为 best effort fallback。由于真实项目中 feature ID 和路径往往不一致，自动匹配仅作为补充机制。

- **GOAL-3.2** [P1]: 桥接边携带可信度标注和来源标识，以区分手动创建的边和自动生成的边。

- **GOAL-3.3** [P1]: 增量 extract 时，桥接边随代码节点一起更新 — 删除的代码节点对应的桥接边也删除，新增的代码节点尝试匹配现有 feature。

- **GOAL-3.4** [P0]: 支持用户在 feature 节点手动指定关联代码路径列表，作为主要桥接机制，优先于自动匹配。自动匹配（GOAL-3.1）仅在未指定时作为 fallback。

### 4. 跨层查询 (cross-layer-query)

- **GOAL-4.1** [P0]: `gid query impact <node>` 和 `gid query deps <node>` 能从任意节点类型出发跨层遍历。impact 沿依赖方向展开（task → feature → code），deps 沿反向追踪。验证：对一个 task 节点查 impact/deps，返回结果中包含 code 类型节点。

- **GOAL-4.3** [P1]: `gid query impact/deps` 支持按节点类型过滤结果（只看 code 节点、只看 task 节点等）。

### 5. 废弃 code_graph.json 路径 (deprecate-code-graph-json)

- **GOAL-5.1** [P0]: `gid_extract` 工具函数（agent 调用入口）返回的结果直接基于 graph.yml，不再引用 code_graph.json。

- **GOAL-5.2** [P1]: `build_unified_graph()` 函数标记为 deprecated，一个版本后移除。现有调用方迁移到直接读 graph.yml。

- **GOAL-5.3** [P1]: `CodeGraph` 类型不再作为持久化格式暴露。

- **GOAL-5.4** [P1]: `gid schema` 优先从 graph.yml 的代码层节点生成输出，仅在图中无代码节点时 fallback 到 extract。

### 6. 分层控制 (layer-control)

- **GOAL-6.1** [P0]: 每个节点有明确的 layer 归属。定义两个逻辑层（code / project）和一个标注维度（semantic 架构层标签）。验证：graph.yml 中每个节点可被判定为 code 或 project 层之一，无歧义节点。semantic 标签独立于层判断，不影响 `--layer` 过滤结果。

- **GOAL-6.2** [P0]: 所有读取图的 CLI 命令支持 `--layer <code|project|all>` 过滤参数。默认值因命令而异：面向任务管理的命令默认 project 层，面向可视化/查询的命令默认 all。

- **GOAL-6.3** [P0]: `gid visual --layer code` 只渲染代码节点及其边，`--layer project` 只渲染 task/feature 节点及其边，`--layer all` 渲染完整图含跨层桥接边。

- **GOAL-6.4** [P1]: `gid query impact/deps` 的 `--layer` 参数控制遍历范围 — `--layer code` 只沿代码边遍历，`--layer project` 只沿项目边遍历，`--layer all` 跨层遍历（通过桥接边穿透）。桥接边仅在 `--layer all` 模式下参与遍历。

- **GOAL-6.5** [P1]: 所有写入路径显式设置 `source` 字段 — extract 设 `"extract"`，design --parse / add-task / ritual generate-graph 设 `"project"`，桥接设 `"auto-bridge"`。不允许写入 `source == None` 的节点。层过滤使用白名单匹配，不使用 `None` 兜底。

- **GOAL-6.6** [P2]: `gid stats` 按层显示节点/边统计 — code 层 N 个节点 M 条边，project 层 N 个节点 M 条边，桥接边 N 条。

- **GOAL-6.7** [P1]: 常用命令在统一图上的读取延迟不应因 code 节点增加而超过 2x。

- **GOAL-6.8** [P0]: `graph.summary()` 和 `graph.ready_tasks()` 默认只统计 project 层节点。提供包含 code 节点的变体接口。验证：向 graph 写入 100 个 code 节点后，`summary()` 的 todo 计数不包含 code 节点，`ready_tasks()` 返回结果中无 code 节点。

### 7. 向后兼容 (backward-compat)

- **GOAL-7.1** [P0]: 现有 graph.yml（只含 task/feature 节点）能被新版本正常读取，无需迁移。

- **GOAL-7.2** [P1]: 首次 extract 时，如果存在旧的 `code_graph.json` 且 graph.yml 中没有代码层节点，自动迁移其内容到 graph.yml（one-time migration）。

### 8. 内部接口迁移 (internal-migration)

> 以下 GOALs 描述内部模块的接口迁移要求，作为实现统一图后的必要清理工作。

- **GOAL-8.1** [P0]: `working_mem.rs` 模块不再依赖 `CodeGraph` 持久化格式进行图操作。

- **GOAL-8.2** [P0]: `gid-cli/src/main.rs` cmd_extract 迁移到新 extract 流程（写 graph.yml）+ 重新加载 graph。

- **GOAL-8.3** [P0]: `harness/scheduler.rs` post_layer_extract 迁移到直接从 graph.yml 加载统一图。

- **GOAL-8.4** [P0]: `ritual/executor.rs` extract 阶段迁移到直接从 graph.yml 加载统一图。

- **GOAL-8.5** [P1]: `rustclaw/src/tools.rs` 中的 CodeGraph 调用迁移到基于 graph.yml 的接口。

### 9. Planned Code Nodes (planned-code-nodes)

- **GOAL-9.1** [P1]: `generate-graph` 从设计文档中识别主要代码结构（struct, trait, module, public function），并在 graph.yml 中创建对应的 planned code nodes。Planned nodes 使用 `node_type: "code"`、`status: planned`、`source: "project"`，`node_kind` 保持精确种类标识。验证：对一个描述 "AuthService struct with login() method" 的设计文档运行 generate-graph，graph.yml 中包含 `struct:AuthService` 和 `fn:auth::login` 节点，status 均为 planned。

- **GOAL-9.2** [P1]: `update-graph` 在追加新功能时，保留已有的 planned code nodes，并为新功能描述的代码结构创建新的 planned code nodes。验证：对已有 planned nodes 的 graph 运行 update-graph 追加新 feature，原有 planned nodes 不丢失，新 feature 对应的 planned nodes 被创建。

- **GOAL-9.3** [P2]: Planned code nodes 支持生命周期转换 — `extract` 产出真实代码节点后，planned nodes 的 `status` 可从 `planned` 更新为 `done`（或被 extract node supersede）。未匹配的 planned nodes（无对应 extract node）保持 `status: planned`，作为缺失实现的信号。验证：graph 同时包含 `struct:AuthService`（planned, source: project）和 `func:src/auth/service.rs:AuthService`（extract, source: extract），verify 步骤能识别匹配关系。

### 10. Agent Tool API (tool-api)

> RustClaw agent tools（`rustclaw/src/tools.rs`）暴露 GID 操作给 LLM，当前 tool schema 不足以操作统一图。以下 GOALs 描述 tool 层面的前置变更，可在统一图核心工作之前或并行完成。

- **GOAL-10.1** [P0]: `gid_add_task` 接受 `node_type`、`source`、`node_kind`、`file_path`、`metadata`（JSON object）作为可选参数。LLM 可创建 feature 节点、planned code 节点、component 节点 — 不仅限于 task。工具描述更新以反映其通用性（可创建任意节点类型）。验证：调用 `gid_add_task` 传入 `node_type: "code"`、`node_kind: "struct"`、`status: "planned"` 创建一个 planned code node 并写入 graph.yml。

- **GOAL-10.2** [P0]: `gid_add_edge` 的 `relation` 参数从 4 值 enum 改为 string 类型，描述中列出所有常用 relation 值：depends_on, blocks, subtask_of, relates_to, implements, contains, tests_for, calls, imports, defined_in, belongs_to, maps_to, overrides, inherits。验证：调用 `gid_add_edge` 传入 `relation: "implements"` 创建正确的边。

- **GOAL-10.3** [P1]: `gid_update_task` 接受 `tags`、`metadata`、`priority`、`node_type`、`node_kind` 作为可选更新参数。验证：调用时传入 `tags: ["gate:human"]` 更新节点的 tags 数组。

- **GOAL-10.4** [P0]: `gid_tasks` 默认按 `node_type` 过滤 — 未指定过滤条件时，只显示 `node_type` 为 `None`（legacy）、`"task"`、`"feature"` 或 `"component"` 的节点。接受可选 `node_type` 参数用于显式过滤。summary 统计同样只计算 project 层节点（与 GOAL-6.8 一致）。注意：planned code nodes（`node_type: "code"`, `source: "project"`）不在默认视图中显示，需要 `node_type: "code"` 或 `"all"` 显式查看。验证：graph 含 10 个 task + 100 个 code 节点 → `gid_tasks` 显示 10 个，不是 110 个。

- **GOAL-10.5** [P1]: `gid_query_impact` 和 `gid_query_deps` 接受可选 `relations` 参数（edge relation 字符串列表），过滤遍历的边类型。默认遍历所有 relation（当前行为）。底层调用 gid-core `QueryEngine` 已有的 `engine.impact_filtered(node_id, relations)` / `engine.deps_filtered(node_id, transitive, relations)` 方法（注意 `deps_filtered` 多一个 `transitive` 参数）。验证：`gid_query_impact(id, relations=["tests_for"])` 只沿 tests_for 边遍历。

- **GOAL-10.6** [P1]: 移除 `gid_execute` 工具。`gid_plan` 增加可选 `detail: bool` 参数 — 为 true 时包含关键路径分析和预估 turns（当前 gid_execute 的内容）。验证：`gid_plan(detail=true)` 输出包含 critical path；不存在 `gid_execute` 工具。

- **GOAL-10.7** [P1]: `gid_refactor` 支持 `delete` 操作，删除节点及其关联的所有边。调用 `Graph::remove_node()`。输出明确展示删除了什么（节点 ID + 关联边数量）。验证：`gid_refactor(operation="delete", id="old-node")` 删除该节点及其所有入边/出边。

- **GOAL-10.8** [P2]: 新增 `gid_search` 工具，支持按关键词（title 匹配）、tag、node_type、status 的组合搜索节点。返回匹配节点的简要上下文。验证：`gid_search(keyword="auth", node_type="task")` 返回 title 中包含 "auth" 的 task 节点。

- **GOAL-10.9** [P2]: 新增 `gid_get_node` 工具，获取单个节点的完整详情，包括所有字段及其入边/出边。验证：返回节点全部字段 + inbound/outbound edges 列表。

## Guards

- **GUARD-1** [hard]: extract 操作绝不能删除或修改非代码层节点。违反 = task/feature 数据丢失。清理代码节点时不能产生悬空边（dangling edges）。
- **GUARD-2** [hard]: graph.yml 写入必须是原子的，不能出现半写状态。
- **GUARD-3** [soft]: graph.yml 中代码节点不序列化冗余字段，减少文件大小。
- **GUARD-4** [soft]: extract 性能不应因写入 graph.yml 而劣化超过 20%（对比写 code_graph.json）。

## Out of Scope

- SQLite 存储后端（已有 T2.5 SqliteStorage，独立 feature）
- LSP 集成（精确解析，独立 feature）
- 多仓库联合图（未来 feature）
- graph.yml 的 schema 版本号/迁移框架（由 sqlite-migration feature 覆盖）
- CodeGraph 类型和转换层的完全移除（P2 里程碑，待 GOAL-5.3 完成后评估）

## Dependencies

- ISS-009 Cross-Layer（已完成）— module nodes, BelongsTo, TestsFor 在 extract 中已实现
- ISS-006 Incremental Extract（已完成）— 增量逻辑已在 extract 中
- T2.5 SqliteStorage（已实现）— 统一图可选持久化到 SQLite，但不是本 feature 的前置
