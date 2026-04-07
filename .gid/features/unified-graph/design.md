# Design: Unified Graph

## Overview

统一图的实现策略：extract 产出 `graph::Node` / `graph::Edge` 直接写入 graph.yml，通过 `source` 和 `node_type` 字段实现层隔离，桥接边通过路径匹配自动生成。现有 `code_graph.json` + `unified.rs` 合并路径废弃，调用方逐步迁移到直接操作 `Graph`。

## Architecture Decisions

### ADR-1: 层判断使用 `source` 而非 `node_type`

层判断靠 `source` 字段（白名单匹配），不靠 `node_type`。原因：`node_type` 是粗粒度分类字段，未来可能有非 extract 来源的 code 类型节点。`source` 明确标识数据来源，语义更精确。

**三个合法 source 值**：`"extract"`（code 层）、`"project"`（project 层）、`"auto-bridge"`（桥接边）。所有写入路径必须显式设置 source。`source == None` 仅在旧 graph.yml 向后兼容期存在，T4.1 迁移后消除。

### ADR-2: 边的所有权由 `metadata["source"]` 决定

每条边通过 `edge.metadata["source"]` 标识所属方：
- `"extract"` — 随 extract 生命周期管理
- `"auto-bridge"` — 在桥接步骤重建

增量 extract 时只清理自己 source 的边，不触碰其他来源的边。这是实现 GOAL-1.2（基于来源的精准清理）和 GOAL-1.3（按层隔离操作）的核心机制。

#### Edge Metadata 访问约定

`Edge.metadata` 的实际类型是 `Option<serde_json::Value>`（非 `HashMap<String, String>`）。Design 中简写的 `edge.metadata["source"]` 是伪代码，实际访问需通过 JSON 路径解引用。提供 helper 函数：

```rust
fn edge_source(edge: &Edge) -> Option<&str> {
    edge.metadata.as_ref()?.get("source")?.as_str()
}
```

后续各处 `edge.metadata["source"] == "extract"` 的伪代码，实现时统一使用 `edge_source(edge) == Some("extract")`。或者更优方案：在 `Edge` 上加 `pub fn source(&self) -> Option<&str>` 方法。

### ADR-3: CodeGraph 作为过渡中间表示

`CodeGraph` 类型保留为 extract 内部中间表示。extract 内部构建 CodeGraph → 转换为 Graph 后写入 graph.yml，但不再作为持久化格式暴露。具体而言：`code_graph.json` 不再被写入或读取，`CodeGraph` 仅作为 extract 解析器到 `graph::Node` 转换的内存中间结构存在。调用方不应依赖 `CodeGraph` 的序列化/反序列化能力。*(covers GOAL-5.3)*

**P2 里程碑**：extract 解析器直接产出 `graph::Node`，移除 CodeGraph 类型和转换层，消除双重类型系统的 tech debt。Node 类型枚举化（String → NodeType enum）同一批完成。

### ADR-4: YAML 后端的性能取舍

YAML 后端下，`--layer` 过滤在全量加载后应用（序列化/反序列化开销不可避免）。SQLite 后端可通过 WHERE 子句实现真正按层查询。YAML 模式下典型项目 <500ms 可接受，大型项目应迁移到 SQLite。

### ADR-5: 跨层查询无需修改 QueryEngine

一旦 code 节点写入 graph.yml 且桥接边存在，现有 `QueryEngine` 的 `impact` / `deps` 遍历自动跨层生效，无需代码变更。GOAL-4.1（含 deps query）为集成测试验收条件，不是实现任务。

**验证条件**：需确认 `QueryEngine` 的 BFS/DFS 遍历当前不对 `relation` 做过滤。如果 `impact()` / `deps()` 实现中存在 relation 白名单（如只遍历 `depends_on`），需要将 `maps_to` 加入可遍历关系列表。实现前检查 `graph.rs` 中 `impact()` 和 `deps()` 的实现。如果确认不过滤 relation 类型，标注 "已验证：遍历不过滤 relation 类型"。

## Schema Design

### 代码节点类型映射（GOAL-1.1 实现）

代码节点使用 `graph::Node` 类型写入 graph.yml，字段映射：

| 字段 | 值 | 说明 |
|------|-----|------|
| `source` | `"extract"` | 层判断依据（ADR-1） |
| `node_type` | `"code"` | 粗粒度跨层分类 |
| `node_kind` | NodeKind 原始值 | function, struct, class, impl, trait, enum, variant, constant, interface, module, method — **不做 kind 合并或重命名，保持 extract 原始粒度** |
| `status` | `Done` | 代码存在 = 完成。满足 NodeStatus required field 语义，不参与 project 层任务状态统计 |

**字段分工**：`node_type` 用于跨层分类（code/task/feature/component），`node_kind` 用于层内精确过滤（struct vs trait vs enum）。

### 代码节点 ID 格式

保持 extract 原始格式：`file:path`, `func:path:name`, `module:path`。不做 ID 转换。deprecated `code_node_to_task_id()` 不再使用。

### extract-meta.json 语义对齐（GOAL-1.4 实现）

`extract-meta.json` 的 `node_ids` 对应 graph.yml 中的代码层节点 ID（语义从 "code_graph.json 节点" 变为 "graph.yml code 层节点"）。具体变更：

- **之前**：`FileState.node_ids` 存储 `CodeGraph` 节点 ID，用于增量 extract 时 diff 和清理 `code_graph.json` 中的旧节点
- **之后**：`FileState.node_ids`（定义在 `code_graph::types::FileState`）存储 graph.yml 中 `source == "extract"` 的节点 ID，用于增量 extract 时从 graph.yml 中精准删除该文件的旧代码节点
- **ID 格式不变**：仍为 `file:path`, `func:path:name`, `module:path` 等 extract 原始格式
- **清理流程**：增量 extract 读取 `extract-meta.json` → 对 modified/deleted 文件，从 graph.yml 中删除 `node_ids` 列表中的节点 → 写入新节点 → 更新 `extract-meta.json`

`extract-meta.json` 文件位置保持 `.gid/extract-meta.json` 不变。

*(ref: 现有 unified.rs 的 CodeNode→Node 转换逻辑, extract parser 的 NodeKind enum)*

### 代码边映射（GOAL-1.2 实现）

代码边使用 `graph::Edge` 类型写入 graph.yml：

- edge relation 统一使用 snake_case 格式（imports, calls, inherits, defined_in, tests_for, belongs_to, overrides, implements），与现有 project 边一致
- CodeEdge 的 `weight`、`confidence`、`call_site_line`、`call_site_column` 字段保留在 Edge 的 `metadata` JSON 中
- 代码边的来源标识：`edge.metadata["source"] = "extract"`（支持基于来源的精准清理）

*(ref: 现有 unified.rs edge 转换)*

### 桥接边 Schema（GOAL-3.2 实现）

- relation: `maps_to`
- `confidence`: 精确匹配 1.0，前缀匹配 0.8，模糊匹配 0.5
- `edge.metadata["source"] = "auto-bridge"`（区分手动创建的边和自动生成的边）

### 序列化优化（GUARD-3 实现）

代码节点使用 `skip_serializing_if` 避免序列化未设置的 Option 字段，减少 graph.yml 文件大小。

### 逻辑层定义（GOAL-6.1 实现）

两个逻辑层 + 一个标注维度：
- **code 层**：`source == "extract"` 的节点（file, class, function, module, trait, enum, constant, interface）
- **project 层**：`source == "project"` 的节点（task, feature, component）。向后兼容期也包含 `source == None` 的遗留节点（T4.1 迁移后移除）
- **semantic 标注维度**：架构层标签（interface/application/domain/infrastructure），作为 code 节点的 `metadata["layer"]` 属性，非独立逻辑层

`--layer` 参数只有 `code|project|all`，semantic 不作为独立的层选项。

### 节点类型过滤（GOAL-4.3 实现）

`gid query impact/deps --type <filter>` 按 `node_type` 字段过滤结果。Node 类型匹配使用 `node.node_type.as_deref() == Some("function")` 等字符串匹配（P2 枚举化前）。

#### `--type` 与 `--layer` 交互规则

- `--type` 在任何 `--layer` 模式下都生效
- `--layer` 先过滤可见节点集，`--type` 再从可见节点中过滤
- 即：`--layer project --type task` = 只显示 task 节点；`--layer code --type function` = 只显示 function 节点
- `--type` 不改变 `--layer` 的默认值

## Planned Code Nodes

### 概念（设计阶段的代码结构预期）

设计文档描述了预期的代码结构（modules, structs, traits, key functions），但当前 `generate-graph` / `update-graph` 只产出项目管理节点（task, feature, component），不产出代码结构节点。这导致设计→实现之间存在信息断层 — 设计说 "AuthService struct with login() method"，但图中只有 task 节点，没有对应的代码结构预期。

**Planned code nodes** 解决这个问题：在代码编写之前，设计阶段就在图中创建代码结构节点，使用 `status: planned`、`source: "project"` 标识。这些节点的作用：

- **验证目标**：实现完成后，`extract` 产出真实代码节点，verify 步骤比较 planned vs actual
- **依赖锚点**：task 可以直接 `implements → struct:AuthService`，而非模糊地关联到 feature
- **架构文档化**：图展示预期架构结构，不仅仅是任务列表

### 节点命名约定

Planned code nodes 使用 `{kind}:{path_or_name}` 格式，与 extract 产出的代码节点 ID 命名空间对齐：

| 格式 | 示例 | 说明 |
|------|------|------|
| `module:{path}` | `module:src/auth` | 模块 |
| `struct:{name}` | `struct:AuthService` | 结构体 |
| `trait:{name}` | `trait:Storage` | Trait |
| `fn:{module}::{name}` | `fn:auth::login` | 关键公开函数 |
| `enum:{name}` | `enum:StorageBackend` | 枚举 |

**注意**：planned node 的 ID 与 extract 产出的 ID 格式可能不完全一致（extract 使用完整文件路径如 `func:src/auth/service.rs:login`），verify 阶段通过模糊匹配处理差异。

### 节点 Schema

Planned code nodes 使用与其他 project 层节点相同的 `graph::Node` 类型，字段映射：

| 字段 | 值 | 说明 |
|------|-----|------|
| `source` | `"project"` | 设计阶段产出，属于 project 层写入路径 |
| `node_type` | `"code"` | 粗粒度分类 — 表示这是代码结构节点 |
| `node_kind` | `"struct"` / `"trait"` / `"module"` / `"function"` 等 | 精确种类标识 |
| `status` | `planned` | **关键区分字段** — 与 task 的 `todo`/`in_progress`/`done` 区别，与 extract 节点的 `Done` 区别 |

### 生命周期

```
Design 阶段                  Implement 阶段              Verify 阶段
─────────────                ──────────────              ──────────
generate-graph / update-graph    开发者写代码               gid extract
创建 planned code nodes          ...                      产出真实 code nodes
status: planned                  ...                      (source: "extract", status: Done)
source: "project"                ...                      ↓
                                                          比较 planned vs actual
                                                          ├─ matched → planned node status: done
                                                          │           或被 extract node supersede
                                                          └─ unmatched → 缺失实现信号
```

- **创建时机**：`generate-graph`、`update-graph`、`design --parse` 阶段，LLM 根据设计文档产出
- **status: planned**：表示 "设计预期但尚未实现"
- **extract 后比对**：extract 产出的真实代码节点（`source: "extract"`）与 planned 节点匹配
  - **匹配成功**：planned node 的 `status` 更新为 `done`，或标记为被 extract node supersede
  - **匹配失败**（planned 存在但无对应 extract node）：说明预期的代码结构尚未实现，可作为验证信号

### generate-graph prompt 变更

在现有 `generate-graph` prompt 中追加 planned code node 的规则和示例：

**新增规则**：
> 对于设计文档中描述的主要 struct、trait、module 和 public function，创建 planned code structure nodes。这些节点使用 `status: planned`、`source: project`，`node_type: code`，`node_kind` 为具体种类。

**YAML 示例**（追加到 prompt 的 example 部分）：

```yaml
nodes:
  # 项目管理节点（现有）
  - id: feature-auth
    node_type: feature
    title: "Authentication Module"
    status: todo
    source: project

  - id: task-impl-auth-service
    node_type: task
    title: "Implement AuthService"
    status: todo
    source: project

  # Planned code structure nodes（新增）
  - id: "module:src/auth"
    node_type: code
    node_kind: module
    title: "Auth module"
    status: planned
    source: project

  - id: "struct:AuthService"
    node_type: code
    node_kind: struct
    title: "AuthService — handles authentication logic"
    status: planned
    source: project

  - id: "trait:TokenValidator"
    node_type: code
    node_kind: trait
    title: "TokenValidator trait for JWT validation"
    status: planned
    source: project

  - id: "fn:auth::login"
    node_type: code
    node_kind: function
    title: "Login endpoint handler"
    status: planned
    source: project

edges:
  # task → planned code node 的实现关系
  - from: task-impl-auth-service
    to: "struct:AuthService"
    relation: implements

  # planned code node 之间的结构关系
  - from: "struct:AuthService"
    to: "module:src/auth"
    relation: belongs_to

  # planned code node → feature 的归属
  - from: "struct:AuthService"
    to: feature-auth
    relation: belongs_to
```

**Prompt 中的判断标准**：不需要为每个私有函数都创建 planned node。规则：
- ✅ 主要 struct（核心业务逻辑的载体）
- ✅ 公开 trait（接口契约）
- ✅ 模块（src/ 下的目录结构）
- ✅ 关键 public function（API 入口、核心算法）
- ❌ 私有 helper 函数、内部实现细节
- ❌ 测试函数、工具宏

### update-graph prompt 变更

`update-graph` prompt 同样需要扩展：

- **保留现有 planned code nodes**：update-graph 在追加新功能时，不应删除已有的 planned code nodes
- **为新功能创建 planned code nodes**：新增的 feature/task 对应的代码结构也应产出 planned nodes
- **允许的节点类型扩展**：除 task/feature/component 外，允许 `node_type: code` + `status: planned` 的节点

### 无需新工具

Planned code nodes 不需要新的 GID tool，原因：

- **相同写入路径**：LLM 生成 YAML → 写入 graph.yml，与 task/feature 节点完全一致
- **相同 Node struct**：通过 `node_type`、`status`、`source` 字段区分，无需新的数据类型
- **现有工具覆盖**：`gid_add_task` 在语义上可以添加任意 project 层节点（P2 可 alias 为 `gid_add_node` 以避免命名混淆，但不是当前需求）

### 验证机制（P2）

未来 `gid verify-planned` 命令可比较 planned nodes vs extract 产出的真实 code nodes：

- 按 `node_kind` + name/path 模糊匹配
- 输出：matched / unmatched / unexpected（extract 有但无 planned 对应）
- 当前不在 scope 内，但数据模型已支持 — planned nodes（`source: "project"`, `status: planned`, `node_type: "code"`）与 extract nodes（`source: "extract"`, `status: Done`, `node_type: "code"`）通过字段组合天然区分

## Extract Pipeline

### 增量 extract 流程（GOAL-1.3 实现）

① 删除 modified/deleted 文件的旧 code 节点（by extract-meta.json node_ids）+ 这些节点关联的 `source=extract` 边
② 写入新 code 节点 + `source=extract` 边
③ 删除所有 `source=auto-bridge` 边 → 重新匹配生成桥接边

### 全量 extract (--force) 删除范围 *(covers GOAL-1.5)*

全量 extract（`--force`）只清除当前项目的代码层节点，不影响 project 层和 semantic 层。删除逻辑：

1. **识别 code 层节点**：过滤 `node.source.as_deref() == Some("extract")` 的节点
2. **限定当前项目**：进一步过滤 `node.file_path` 以当前项目根路径为前缀的节点（或在单项目模式下跳过此步）
3. **批量删除**：调用 `Graph::remove_node()` 删除匹配的节点（该方法自动清理关联边）
4. **清理残余边**：额外删除 `edge_source == "extract"` **或** `edge_source == "auto-bridge"` 的边（确保没有遗留的代码边和桥接边）
5. **重建**：全量解析所有文件，写入新的代码节点和边，重跑桥接生成

**不做全图清空** — project 层节点（task, feature, component）和手动创建的边完全不受影响。`extract-meta.json` 在 `--force` 时被忽略并重新生成。

### 级联清理规则（GUARD-1 实现）

extract 清理 code 节点时，如果存在其他 source（非 extract、非 auto-bridge）的边指向被删节点，这些边也应一并删除（级联清理 dangling edges）。此规则仅适用于边，不适用于节点本身 — 只清理指向已删除 code 节点的悬空边，不清理非 extract 节点。

### 原子写入（GUARD-2 实现）

graph.yml 写入采用原子策略：写临时文件 → rename，不能出现半写状态。

### Semantify 集成（GOAL-2.1 实现）

extract 完成后自动运行 semantify，给代码节点打架构层标签，标签存入 `metadata["layer"]`。`--no-semantify` 标志可跳过（GOAL-2.2）。

## Bridge Edge Generation

### 匹配算法（GOAL-3.1 / GOAL-3.4 实现）

1. **优先级 1 — code_paths 显式指定**（GOAL-3.4）：feature 节点的 `metadata["code_paths"]` 手动指定关联路径列表
2. **优先级 2 — 自动前缀匹配（fallback）**（GOAL-3.1）：仅在 `code_paths` 未设置时触发。feature node 的 `id` 与 module/file 节点的路径做前缀匹配。例如 feature `auth` 匹配 `module:src/auth`

### code_paths 格式

相对路径的 JSON 数组（如 `["src/auth", "src/storage/sqlite.rs"]`），使用前缀匹配 — 匹配所有 code 层节点的 `file_path`（metadata）或 `id` 中包含该路径前缀的节点。不支持通配符。前缀匹配已覆盖主要场景（按目录关联），通配符需求可用多个 code_paths 条目替代（如 `["src/auth", "src/middleware/auth.rs"]`）。如果后续发现不够，通配符作为 P2 增强。

### 自动匹配的局限性

真实项目中 feature ID 和路径往往不一致（如 `sqlite-migration` → `src/storage/sqlite.rs`），自动匹配仅作为补充机制。

### 桥接边生命周期管理 *(covers GOAL-3.3)*

增量 extract 更新代码节点时，关联的桥接边同步更新：

1. **代码节点删除时**：步骤 ③ 的桥接重建会先删除所有 `metadata["source"] == "auto-bridge"` 的边，因此指向已删除代码节点的桥接边自动消失。无需单独做级联删除 — 桥接边的完整重建策略天然覆盖此场景。
2. **代码节点新增时**：步骤 ③ 的桥接重建根据匹配规则（GOAL-3.1 / GOAL-3.4）为新增的代码节点生成桥接边。
3. **代码节点修改时**（文件内容变更但节点 ID 不变）：节点本身被替换，桥接边在重建时重新评估匹配。若匹配条件仍满足，桥接边被重新生成（ID 和 confidence 可能不变）。

**实现要点**：桥接边采用「全量重建」而非「增量维护」策略 — 每次 extract 后删除全部 `auto-bridge` 边再重新生成。这简化了实现，且桥接边数量通常远小于代码边（feature 节点数 × 匹配的 code 节点数），性能开销可接受。

**性能估算**：全量重建需 `N_feature × N_code` 次字符串前缀比较，典型项目 `50 × 2000 = 100K` 次，<10ms。即使 `500 × 20000 = 10M` 次也在 100ms 量级，远低于 YAML I/O 开销。

### 遍历规则（GOAL-6.4 实现）

`maps_to` 桥接边仅在 `--layer all` 模式下参与遍历，`code` 和 `project` 模式下排除跨层边。

## Layer Filtering

### `--layer` 过滤实现（GOAL-6.2 实现）

过滤在图全量加载后、输出前应用，不影响底层数据。YAML 后端下无法避免全量反序列化（ADR-4）。

### `--layer` 默认值

按命令差异化，避免不必要的 code 节点加载进输出：
- `gid tasks` → 默认 `--layer project`
- `gid visual` / `gid read` / `gid query impact` / `gid query deps` → 默认 `--layer all`

### gid visual --layer 渲染规则 *(covers GOAL-6.3)*

各 `--layer` 模式下的渲染范围：

| 模式 | 渲染的节点 | 渲染的边 | 桥接边处理 |
|------|-----------|---------|------------|
| `--layer project` | `source == "project"` 的节点（过渡期含 `source` 为空的遗留节点） | 两端均为 project 层节点的边 | **隐藏** — 桥接边的 code 端不可见，显示桥接边无意义 |
| `--layer code` | `source == "extract"` 的节点 | 两端均为 code 层节点的边（`metadata["source"] == "extract"`） | **隐藏** — 桥接边的 project 端不可见 |
| `--layer all` | 全部节点 | 全部边，包括代码边、项目边和桥接边 | **显示** — 桥接边作为跨层连接线渲染，使用虚线样式区分 |

**边过滤实现**：加载全图后，根据可见节点集合过滤边 — 仅保留 `from` 和 `to` 均在可见节点集合中的边。这自然排除了跨层边（包括桥接边），无需按边类型单独判断。

### 写入路径隔离 *(covers GOAL-6.5)*

确保 extract 和 design --parse 的写入路径互不干扰：

| 操作 | 写入的节点 | source 标识 | 清理范围 |
|------|-----------|-------------|----------|
| `gid extract` | 代码层节点 | `source: "extract"` | 只删除/替换 `source == "extract"` 的节点 |
| `gid design --parse` / `gid add-task` / ritual `generate-graph` | project 层节点 | `source: "project"` | 只操作 `source == "project"` 的节点 |

**隔离机制**：
- extract 在清理阶段仅 `retain` `source != "extract"` 的节点，绝不触碰 project 层节点（GUARD-1）
- design --parse、add-task、ritual generate-graph 写入时设置 `source: "project"`，显式标识 project 层归属
- 所有层判断使用白名单匹配（`"extract"` / `"project"` / `"auto-bridge"`），不使用 `None` 兜底。未知 source 值不被归入任何层，在过滤时被排除（fail-safe）
- 两者操作的 node ID 命名空间天然不重叠：extract 使用 `file:`, `func:`, `module:` 等前缀，design 使用用户定义的 task/feature ID

**向后兼容**：现有 graph.yml 中 `source` 为空（`None`）的节点，在 T4.1 迁移时统一补为 `source: "project"`。迁移前，`project_nodes()` helper 同时匹配 `source == "project"` 和 `source == None`（过渡期兼容）。迁移后移除 `None` 兼容分支。

### summary() / ready_tasks() 行为（GOAL-6.8 实现）

`graph.summary()` 和 `graph.ready_tasks()` 内部按 `source` 过滤，默认只统计 project 层节点。提供包含 code 节点的变体接口（如接受 layer 参数的重载）。

### gid stats 分层统计 *(covers GOAL-6.6)*

`gid stats` 按层展示统计，输出格式：

```
Project layer:  12 nodes, 18 edges
Code layer:     147 nodes, 312 edges
Bridge edges:   9
Total:          159 nodes, 339 edges
```

**统计实现**：
- **Project 层节点**：`node.source.as_deref() == Some("project")` 的节点（过渡期也包含 `source` 为空的遗留节点）
- **Code 层节点**：`node.source.as_deref() == Some("extract")` 的节点
- **Project 层边**：`edge.metadata` 中无 `source` 字段，或 `source` 不是 `"extract"` 和 `"auto-bridge"` 的边
- **Code 层边**：`edge.metadata` 中 `source == "extract"` 的边
- **Bridge 边**：`edge.metadata` 中 `source == "auto-bridge"` 的边
- **Total**：所有节点和边的总数（project 边 + code 边 + bridge 边 = total 边）

## Caller Migration Plan

### P0 调用方（必须迁移）

| 调用方 | 位置 | 迁移方式 |
|--------|------|----------|
| `working_mem.rs`（GOAL-8.1） | `query_gid_context`, `analyze_impact`, `find_test_files`, `collect_function_info` | 参数从 `&CodeGraph` 改为 `&Graph`，使用 `source == "extract"` / `node_type` 过滤。Node 类型匹配使用 `node.node_type.as_deref() == Some("function")` 字符串匹配 |
| `gid-cli/src/main.rs`（GOAL-8.2） | cmd_extract | 从 `CodeGraph::extract_from_dir()` + `build_unified_graph()` 改为新 extract（写 graph.yml）+ 重新加载 graph |
| `harness/scheduler.rs`（GOAL-8.3） | post_layer_extract (line 492) | 从 `CodeGraph::extract_from_dir()` + `build_unified_graph()` 改为直接从 graph.yml 加载统一图 |
| `ritual/executor.rs`（GOAL-8.4） | extract 阶段 (line 466-468) | 同上 |

### P1 调用方

| 调用方 | 位置 | 迁移方式 |
|--------|------|----------|
| `rustclaw/src/tools.rs` GidExtractTool（GOAL-8.5） | line 3496, 3513 | 使用新 extract + 从 graph.yml 加载 |
| `rustclaw/src/tools.rs` GidSchemaTool | line 3618, 3621 | 调用新 schema 接口（GOAL-5.4） |
| `rustclaw/src/tools.rs` GidComplexityTool | line 4092 | 从 graph.yml 的 code 层节点评估复杂度 |
| `rustclaw/src/tools.rs` GidWorkingMemoryTool | line 4194 | 调用迁移后的 working_mem |

### Node 类型枚举化

Node 类型的枚举化（String → NodeType enum）列为 **P2 优化**，与 CodeGraph 消除（ADR-3 P2 里程碑）同一批完成。

### gid schema 迁移（GOAL-5.4 实现）

`gid schema` 从 graph.yml 的 code 层节点生成输出（如果 graph 已包含 code 节点），仅在 graph 无 code 节点时 fallback 到 extract。`get_schema()` 和 `assess_complexity_from_graph()` 基于 Graph 的 code 层节点工作（过滤 `source == "extract"` / `node_type`）。

## Deprecation Strategy

### build_unified_graph()（GOAL-5.2 实现）

标记为 deprecated，一个版本后移除。现有调用方迁移到直接读 graph.yml（见 Caller Migration Plan）。deprecated 标记在合并后的下一个 minor 版本发布（v0.X+1），一个 minor 版本后（v0.X+2）移除。

### code_graph.json（GOAL-1.6 / GOAL-5.1 实现）

- extract 不再生成 `code_graph.json`
- 旧文件不自动删除（避免数据丢失），但 extract 不再读写它
- `gid_extract` 工具函数返回结果直接基于 graph.yml，不再引用 code_graph.json

### code_node_to_task_id()

不再使用，随 CodeGraph 消除一并移除。

### 一次性迁移（GOAL-7.2 实现）

首次 extract 时，如果存在旧的 `code_graph.json` **且** graph.yml 中没有 `source == "extract"` 的节点，自动迁移其内容到 graph.yml。迁移状态由 graph.yml 本身表达（包含 code 节点 = 已迁移），无需额外标记文件。

### 向后兼容：旧版 graph.yml 处理 *(covers GOAL-7.1)*

打开旧版 graph.yml（不含代码节点、节点无 `source` 字段）时的行为：

1. **`source` 字段缺失处理**：`Node.source` 是 `Option<String>`（定义在 `graph::Node`），反序列化时 `#[serde(default)]` 自动将缺失的 `source` 设为 `None`。
2. **层判断逻辑**：过渡期，`source == None` 的节点视为 **project 层**（向后兼容）。所有层过滤 helper 同时匹配 `Some("project")` 和 `None`。
3. **T4.1 迁移时回填**：一次性迁移（T4.1）将所有 `source == None` 的节点补为 `source: "project"`。迁移后移除 `None` 兼容分支，层判断简化为纯白名单（`"extract"` / `"project"` / `"auto-bridge"`）。
4. **extract 清理安全**：extract 删除 `source == "extract"` 的节点，旧节点 `source == None` 不受影响。
5. **stats/summary 兼容**：`source == None` 的节点计入 project 层统计（与 `source == "project"` 同等对待）。

**过渡策略**：T4.1 之前旧 graph.yml 开箱即用，T4.1 之后所有节点都有显式 source 值。

## Performance Considerations

### YAML vs SQLite

| 维度 | YAML 后端 | SQLite 后端 |
|------|-----------|-------------|
| 层过滤 | 全量加载后内存过滤 | WHERE 子句按层查询 |
| 典型延迟 | <500ms（可接受） | 更低 |
| 适用场景 | 典型项目 | 大型项目 |

### 性能目标

- 常用命令（`gid tasks`、`gid visual --layer project`）在统一图上的读取延迟不应因 code 节点增加而超过 2x（GOAL-6.7）
- extract 写入 graph.yml 的性能不应劣化超过 20%（对比写 code_graph.json）（GUARD-4）
  - **Benchmark 条件**：项目规模 500 文件 / ~2000 code 节点
  - **测量范围**：extract wall time（不含 semantify）
  - **对比基线**：当前 extract → code_graph.json 的时间
  - **测量方法**：`hyperfine` 或 `criterion` benchmark

## Post-Completion Checklist

- [ ] sqlite-migration/requirements.md 的 Out of Scope 中关于 "extractor continues writing code-graph.json" 的条目不再适用，需同步更新
- [ ] 更新 README/docs 中关于 code_graph.json 的描述
- [ ] 更新 gid-cli `--help` 文档中的 extract 行为描述
- [ ] 通知 RustClaw 侧更新 GidExtractTool 的 tool description

## P2 Roadmap

以下 P2 项分散在 design 各处，汇总如下：

- **消除 CodeGraph 中间类型**（ADR-3）：extract 解析器直接产出 `graph::Node`，移除 CodeGraph 类型和转换层
- **Node 类型枚举化**（Caller Migration Plan）：`String` → `NodeType` enum，与 CodeGraph 消除同一批完成
- **SQLite 后端层过滤优化**（ADR-4）：利用 WHERE 子句实现按层查询，替代 YAML 全量加载后内存过滤
- **code_paths 通配符支持**（Bridge Edge Generation）：如果前缀匹配不够，添加 glob pattern 支持
- **Planned code nodes 验证**（Planned Code Nodes）：`gid verify-planned` 命令比较 planned nodes vs extract 产出的真实 code nodes，输出 matched/unmatched/unexpected（GOAL-9.3）

## Agent Tool API Changes

> RustClaw agent tools 全部定义在 `rustclaw/src/tools.rs`。以下变更为统一图的 tool 层前置工作，可独立于统一图核心实现完成。所有变更限于该单一文件（除非注明）。

### GOAL-10.1: gid_add_task schema 扩展

当前 `GidAddTaskTool` 的 JSON schema 只有 `id`, `title`, `description`, `depends_on`, `status`, `tags`, `priority` 参数。扩展为：

| 新参数 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `node_type` | `Option<String>` | `None` | 节点类型：task, feature, component, code |
| `source` | `Option<String>` | `"project"` | 来源标识，project 层节点默认 "project" |
| `node_kind` | `Option<String>` | `None` | 精确种类：struct, trait, module, function 等 |
| `file_path` | `Option<String>` | `None` | 关联文件路径 |
| `metadata` | `Option<serde_json::Value>` | `None` | 任意 JSON 对象，存入 Node.metadata |

所有新参数为 optional — 现有调用（只传 id/title/status）继续正常工作，向后兼容。

**别名方案**：注册两个工具名（`gid_add_task` 和 `gid_add_node`）指向同一个 `GidAddTaskTool` 实现。两者共享完全相同的 schema 和执行逻辑。`gid_add_task` 的描述更新为 "Add a node to the project graph (task, feature, component, or planned code node)"，`gid_add_node` 描述为 "Add a node to the project graph (alias for gid_add_task)"。实现方式：在 tools 注册列表中 push 两次，name 不同但 impl 相同。

**⚠️ Token 成本注意**：双注册会在 LLM schema 中出现两个完全相同的 tool definition，增加 ~200 tokens/request。如果实测发现 schema 膨胀影响上下文容量，可退回为单注册 `gid_add_task`（仅更新描述说明通用性），不注册 alias。优先单注册方案 — alias 的 UX 收益不大。

**写入逻辑**：构造 `Node` 时，将新参数映射到 `Node` struct 对应字段。`source` 若未指定默认设为 `"project"`（遵循 GOAL-6.5 所有写入路径显式设置 source）。

### GOAL-10.2: gid_add_edge relation 扩展

当前 `relation` 参数使用 `enum: ["depends_on", "blocks", "subtask_of", "relates_to"]`，硬编码 4 个值。`Edge.relation` 在 gid-core 中是 `String` 类型，代码图使用 8 种 relation（imports, inherits, defined_in, calls, tests_for, overrides, implements, belongs_to），统一图还需要 `maps_to`（桥接边）和 `contains`。

**方案**：将 `relation` 从 enum 类型改为 string 类型。在 `description` 字段中列出所有常用值作为 LLM 提示：

```json
{
  "relation": {
    "type": "string",
    "description": "Edge relation type. Common values: depends_on, blocks, subtask_of, relates_to, implements, contains, tests_for, calls, imports, defined_in, belongs_to, maps_to, overrides, inherits"
  }
}
```

这比保留 enum 更好：enum 会导致 LLM 拒绝使用不在列表中的值，而 string + description 既提供提示又不阻止合法值。gid-core 对 relation 值不做验证（任意 String 都可存储），所以 tool 层也无需验证。

### GOAL-10.3: gid_update_task schema 扩展

同 GOAL-10.1 的 pattern — 在 `GidUpdateTaskTool` 的 JSON schema 中增加可选参数：

| 新参数 | 类型 | 说明 |
|--------|------|------|
| `tags` | `Option<Vec<String>>` | 替换节点的 tags 数组 |
| `metadata` | `Option<serde_json::Value>` | 合并到节点的 metadata（非替换，而是 JSON merge） |
| `priority` | `Option<u32>` | 更新优先级 |
| `node_type` | `Option<String>` | 更新节点类型 |
| `node_kind` | `Option<String>` | 更新精确种类 |

更新逻辑：只修改传入的字段（`None` 表示不修改该字段）。`metadata` 使用 **浅层 JSON merge** 语义 — 顶层 key：新 key 追加，已有 key 覆盖，不传入的 key 保留。嵌套对象整体替换，不做深层 merge。这保持行为简单可预测。

### GOAL-10.4: gid_tasks node_type 过滤

`GidTasksTool` 当前调用 `graph.nodes` 遍历所有节点并格式化输出。统一图后会包含大量 code 节点，淹没 task 信息。

**变更**：
1. 添加可选 `node_type` 参数到 schema
2. 默认行为（`node_type` 未指定时）：只显示 `node_type` 为 `None`、`"task"`、`"feature"` 或 `"component"` 的节点。实现：`node.node_type.as_deref().map_or(true, |t| ["task", "feature", "component"].contains(&t))`
3. 显式指定时：`node_type: "code"` 只显示 code 节点，`node_type: "all"` 显示所有节点
4. summary 统计使用 `graph.project_nodes()`（T0.2 的 helper）或等效过滤逻辑，只统计 project 层节点

### GOAL-10.5: query tools relations 过滤

`GidQueryImpactTool` 和 `GidQueryDepsTool` 当前调用 `graph.impact()` / `graph.deps()`。gid-core 的 `QueryEngine` 已提供 `impact_filtered(node_id, relations)` 和 `deps_filtered(node_id, transitive, relations)` 变体。注意两者签名不同：
- `impact_filtered(&self, node_id: &str, relations: Option<&[&str]>) -> Vec<&Node>`
- `deps_filtered(&self, node_id: &str, transitive: bool, relations: Option<&[&str]>) -> Vec<&Node>`

`deps_filtered` 多一个 `transitive` 参数（当前 `gid_query_deps` 已有此参数，直接透传即可）。

**变更**：
1. 在两个工具的 schema 中添加 `relations` 可选参数（`type: array, items: { type: string }`）
2. 当 `relations` 传入时：impact 调用 `engine.impact_filtered(id, Some(&relations_refs))`，deps 调用 `engine.deps_filtered(id, transitive, Some(&relations_refs))`；未传入时调用原方法（等价于遍历所有 relation）
3. 描述中列出常用 relation 值供 LLM 参考

### GOAL-10.6: 移除 gid_execute

`GidExecuteTool` 调用 `create_plan()` 后追加 "⚠️ Full execution not available" — 从未实际执行任何操作，name 误导 LLM。

**变更**：
1. 删除 `GidExecuteTool` struct 及其 `Tool` impl
2. 从 tools 注册列表中移除 `gid_execute`
3. 在 `GidPlanTool` schema 中添加 `detail: Option<bool>` 参数
4. 当 `detail == true` 时，plan 输出追加关键路径分析和预估 turns（从 gid_execute 的实现逻辑中提取）

### GOAL-10.7: gid_refactor 增加 delete 操作

当前 `GidRefactorTool` 的 `operation` enum 为 `["rename", "merge", "update_title"]`。

**变更**：
1. 在 enum 中添加 `"delete"`
2. `delete` 操作：读取 `id` 参数，先计算关联边数 `graph.edges.iter().filter(|e| e.from == id || e.to == id).count()`，然后调用 `graph.remove_node(id)` — 该方法自动清理该节点的所有关联边
3. 输出格式：`"Deleted node '{id}' and N associated edges"`（N = 预计数的边数）
4. 不需要确认机制 — graph 操作可通过 git 恢复，LLM 是受信任的操作者

### P2 新增工具

**gid_search**（GOAL-10.8）和 **gid_get_node**（GOAL-10.9）为全新的 tool struct，不修改现有工具。实现模式与现有工具一致：定义 struct → impl `Tool` trait → 注册到 tools 列表。

- `GidSearchTool`：接受 `keyword`, `tag`, `node_type`, `status` 可选参数组合。遍历 `graph.nodes` 做内存过滤，返回匹配节点的简要信息（id, title, status, node_type）。未来 SQLite 后端可委派给 `storage.search()` FTS5 查询以提升性能（P2）。
- `GidGetNodeTool`：接受 `id` 参数。从 graph 查找节点，遍历 edges 收集该节点的 inbound/outbound 边列表，格式化输出全部字段。
