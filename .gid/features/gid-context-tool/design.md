# Design: `gid_context` Tool — Agent-Facing Context Assembly

## 1. Overview

gid-core 已经完整实现了 context assembly pipeline（`harness::context` 模块，7 个测试），支持：
- 多 target node BFS 遍历
- 5 级 edge-relation 相关性打分
- Token budget 分类裁剪（target 永不截断，transitive deps 优先丢弃）
- 源码从磁盘读取（file_path + line range）
- Markdown/JSON/YAML 输出

**但这个能力目前没有暴露给 agent。** Agent 无法直接调用 `assemble_context()` — 它是 harness 内部函数，只被 ritual executor 间接调用。

本设计的目标：将 `assemble_context()` 包装为 `gid_context` tool，让 agent 能按 node ID 获取精准的实现上下文，而不是盲读整个文件。

### 1.1 为什么这是最重要的 GID tool

其他 GID tool（`gid_tasks`, `gid_query_deps`, `gid_visual` 等）告诉 agent **graph 结构**。`gid_context` 告诉 agent **实际内容** — 源码、design section、requirements goals、dependency interfaces。这是 agent 从"知道有什么"到"知道怎么做"的桥梁。

### 1.2 两个入口点

gid-core 提供了两个 context assembly 函数：

| 函数 | 场景 | 输入 | 输出 |
|------|------|------|------|
| `assemble_context()` | 通用：给定任意 node ID，BFS 遍历图 + 读源码 | `ContextQuery` (targets, budget, depth, filters, format) | `AssembledContext` (targets, deps, callers, tests + stats) |
| `assemble_task_context()` | 任务专用：给定 task ID，解析 design_ref + GOALs + guards | task_id + gid_root | `TaskContext` (task_info, goals_text, design_excerpt, deps, guards) |

**两个都要暴露。** `assemble_context()` 用于代码探索（"给我这个函数的上下文"），`assemble_task_context()` 用于任务执行（"给我实现这个 task 需要的所有信息"）。

## 2. Tool 参数设计

### 2.1 `gid_context` — 通用 Context Assembly

```json
{
  "name": "gid_context",
  "description": "Assemble token-budget-aware context for target nodes. Traverses the graph to collect dependencies, callers, and tests with relevance scoring. Returns structured context that fits within the token budget.",
  "input_schema": {
    "type": "object",
    "required": ["targets"],
    "properties": {
      "targets": {
        "type": "array",
        "items": { "type": "string" },
        "description": "One or more node IDs to assemble context for (files, functions, classes, tasks)"
      },
      "token_budget": {
        "type": "integer",
        "description": "Maximum token budget for output (default: 8000). Targets are never truncated; transitive deps are dropped first when over budget."
      },
      "depth": {
        "type": "integer",
        "description": "Maximum traversal depth in hops (default: 2). depth=1 returns only direct dependencies."
      },
      "include": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Filter patterns: file globs (e.g. '*.rs') or type filters (e.g. 'type:function'). Empty = include all."
      },
      "exclude": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Node IDs to exclude from results."
      },
      "format": {
        "type": "string",
        "enum": ["markdown", "json", "yaml"],
        "description": "Output format (default: markdown). Markdown is optimized for LLM consumption."
      }
    }
  }
}
```

### 2.2 `gid_task_context` — Task-Specific Context

```json
{
  "name": "gid_task_context",
  "description": "Assemble implementation context for a task node. Resolves design_ref to extract the relevant design section, maps satisfies GOALs to requirement text, collects guards and dependency interfaces. Returns everything a developer needs to implement the task.",
  "input_schema": {
    "type": "object",
    "required": ["task_id"],
    "properties": {
      "task_id": {
        "type": "string",
        "description": "Task node ID in the graph"
      }
    }
  }
}
```

参数故意极简 — `assemble_task_context()` 的设计理念就是"给一个 task ID，自动解析一切"。gid_root 从 workspace 自动推导。

## 3. 实现架构

### 3.1 在 RustClaw `src/tools.rs` 中的位置

两个新 struct：

```rust
struct GidContextTool {
    graph: SharedGraph,
    path: SharedPath,
}

struct GidTaskContextTool {
    graph: SharedGraph,
    path: SharedPath,
}
```

`SharedPath` 是 graph.yml 的路径（如 `.gid/graph.yml`）。从中推导：
- `gid_root` = `path.parent()` → `.gid/`
- `project_root` = `gid_root.parent()` → workspace root（用于 `assemble_context()` 读源码）

### 3.2 `GidContextTool::execute()` 流程

```
input JSON → ContextQuery → assemble_context(&graph, &query) → format_context(&result, format) → ToolResult
```

详细步骤：

1. **解析参数**：从 JSON input 构建 `ContextQuery`
   - `targets`: 必填，`Vec<String>`
   - `token_budget`: 默认 8000
   - `depth`: 默认 2
   - `include` → `ContextFilters.include_patterns`
   - `exclude` → `ContextFilters.exclude_ids`
   - `format`: 默认 Markdown
   - `project_root`: 从 `SharedPath` 自动推导（graph.yml 的 grandparent）

2. **读图**：`let graph = self.graph.read().await;`

3. **调用 library 函数**：`assemble_context(&graph, &query)?`
   - 这是 gid-core 的公开 API，返回 `AssembledContext`

4. **格式化输出**：`format_context(&result, query.format)`
   - gid-core 已实现 `format_context()` 函数，直接复用

5. **附加 stats header**：在输出开头加入遍历统计
   ```
   📊 Context: {nodes_included}/{nodes_visited} nodes, {budget_used}/{budget_total} tokens, {elapsed_ms}ms
   ---
   {formatted_context}
   ```

6. **返回** `ToolResult { output, is_error: false }`

### 3.3 `GidTaskContextTool::execute()` 流程

```
input JSON → task_id → assemble_task_context(&graph, task_id, gid_root) → TaskContext::render_prompt() → ToolResult
```

详细步骤：

1. **解析参数**：提取 `task_id` 字符串

2. **推导路径**：
   ```rust
   let graph_path = PathBuf::from(self.path.as_str());
   let gid_root = graph_path.parent().unwrap_or(Path::new(".gid"));
   ```

3. **读图**：`let graph = self.graph.read().await;`

4. **调用 library 函数**：`assemble_task_context(&graph, &task_id, &gid_root)?`

5. **格式化**：`context.render_prompt()` — gid-core 已实现，产出结构化 markdown：
   - `## Task: {title}\n{description}`
   - `## Design Reference\n{excerpt}`
   - `## Requirements (GOALs to satisfy)\n{goals}`
   - `## Guards (Invariants)\n{guards}`
   - `## Dependency Interfaces\n{interfaces}`

6. **返回** `ToolResult`

### 3.4 注册

在 `register_gid_tools()` 中，紧跟其他 tool 注册之后：

```rust
self.register(Box::new(GidContextTool::new(graph.clone(), path.clone())));
self.register(Box::new(GidTaskContextTool::new(graph.clone(), path.clone())));
```

## 4. 错误处理

| 错误场景 | 处理方式 |
|----------|---------|
| targets 为空 | `ToolResult { is_error: true, output: "targets is required..." }` |
| target node ID 不存在 | `assemble_context()` 返回 `Err` → 转为 error ToolResult |
| task_id 不存在 | `assemble_task_context()` 返回 `Err` → 转为 error ToolResult |
| design.md / requirements.md 不存在 | `assemble_task_context()` 内部 graceful degrade — 对应字段为空，不报错 |
| graph.yml 无法读取 | RwLock 读取失败 → `Err` → error ToolResult |
| 源文件不存在 | 对应 node 的 `source_code` 为 None，`source_note` 说明原因 |

## 5. 与现有工具的关系

```
                    ┌─────────────────┐
                    │  gid_tasks      │  "有哪些任务？"
                    └─────────────────┘
                            │
                            ▼
                    ┌─────────────────┐
                    │  gid_query_deps │  "任务依赖什么？"
                    └─────────────────┘
                            │
                            ▼
         ┌──────────────────┴──────────────────┐
         │                                     │
┌────────┴────────┐                 ┌──────────┴──────────┐
│  gid_context    │                 │  gid_task_context   │
│  通用：code node │                 │  任务专用：task node  │
│  → 源码 + deps   │                 │  → design + GOALs    │
│  → callers       │                 │  → guards + deps     │
│  → tests         │                 │  → 完整 prompt       │
└─────────────────┘                 └─────────────────────┘
```

- `gid_query_deps` → 只返回 node IDs 和 edge 关系，不读源码
- `gid_context` → 返回完整内容（源码 + metadata），带 token budget
- `gid_task_context` → 为 task 组装实现 prompt（design section + GOALs + guards）
- `gid_working_memory` → 给定变更文件列表，分析影响（反方向：从文件到 graph）

## 6. 不需要做的事

- **不需要新的 gid-core 代码** — `assemble_context()` 和 `assemble_task_context()` 已完整实现
- **不需要改 context.rs** — 所有 API surface 已经存在，RustClaw 只需调用
- **不需要 CLI 包装** — 这是 tool 注册，不是命令行
- **不需要测试 context assembly 本身** — gid-core 已有 7 个测试覆盖

## 7. 需要做的事

1. 在 `src/tools.rs` 中添加 `GidContextTool` struct + `impl Tool`（~80 行）
2. 在 `src/tools.rs` 中添加 `GidTaskContextTool` struct + `impl Tool`（~50 行）
3. 在 `register_gid_tools()` 中注册两个新 tool（2 行）
4. 在 `rustclaw.yaml` 的 system prompt 中注册 tool description（自动，因为 tool 会自报 schema）
5. 更新 `docs/gid-tools-audit.md` 反映 24 → 26 tools
6. 测试：在 Telegram 中调用 `gid_context` 和 `gid_task_context`，验证输出

**估计工作量**：~130 行 Rust 代码，30 分钟。
