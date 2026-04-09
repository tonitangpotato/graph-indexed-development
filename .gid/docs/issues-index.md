# Issues: gid-rs (gid-core + gid-harness)

> 项目使用过程中发现的 bug、改进点和待办事项。
> 格式: ISS-{NNN} [{type}] [{priority}] [{status}]

---

## ISS-001 [bug] [P1] [closed] ✅
**发现日期**: 2026-04-02
**发现者**: potato + RustClaw
**组件**: gid-core, gid_extract / save_gid_graph

**描述**:
`gid extract` 处理外部路径时，总是写入当前 workspace 的 `.gid/graph.yml`。

**解决方案** (已实现):
- `gid extract` 增加了 `--output` 参数指定输出路径
- 不指定 `--output` 时默认打印到 stdout（不再隐式写文件）
- 验证日期: 2026-04-05

---

## ISS-002 [improvement] [P0] [closed] ✅
**发现日期**: 2026-04-05
**更新日期**: 2026-04-05
**发现者**: potato + RustClaw
**组件**: gid-core, code_graph.rs / resolve_rust_call_edge

**描述**:
Call edge 解析是纯 name-only matching，没有 receiver type 信息，导致大量 false positive。例如 `TelegramBot` 调用 `self.client.send()` 会错误连接到 `CdpClient::send()`、`DistributedBus::send()` 等所有同名方法。RustClaw 项目 12,039 条 call edge 中约 402 条是这类误报。

**方案演进**:
- ~~原方案: import-scoped filtering + receiver type heuristics~~ ← 仍然是自建半个类型推断
- **新方案: LSP client** — 直接调用语言编译器的 LSP server（rust-analyzer, tsserver, pyright 等），获取 100% 精确的 goto-definition 和 find-references

**LSP Client 方案**:
- 建图时（`gid extract`）启动 LSP client，连接本地/远程 LSP server
- 对每个 call site 调 `textDocument/definition` 拿精确的定义位置
- 精度 ~99%（编译器级别），代码量 ~百行 LSP client vs GitNexus 几万行 TypeEnv
- Hybrid fallback: 有 LSP 用 LSP，没有退化到 tree-sitter + name matching（标注 confidence: low）

**部署模式**:
- 本地: 检测已有 LSP server → 直接连接
- 远程 (GID as a Service): Docker container 预装 LSP server + 沙箱

**对比 GitNexus**: 他们自建 14 Phase TypeEnv 模拟编译器。我们直接调真编译器的 LSP。精度赢，维护成本赢，新语言扩展速度赢。

**相关**:
- PRODUCT-ROADMAP.md Phase 1
- DESIGN.md 架构图中的 LSP client 层

---

## ISS-003 [improvement] [P2] [open]
**发现日期**: 2026-04-05
**发现者**: potato
**组件**: gid-core, semantify

**描述**:
当前 `semantify` 用的是基于文件路径的启发式规则分配架构层（interface/application/domain/infrastructure）。GitNexus 使用 **Leiden community detection** 算法基于图的 edge 密度分布自动发现代码模块边界，效果更好。

**改进方向**:
- 研究 Leiden 聚类算法，替代或补充当前启发式 semantify
- 算法基于 edge 密度自动聚类，不依赖路径命名约定
- 可用于自动发现功能模块边界、生成 SKILL.md（GitNexus 的 `--skills` 功能）

**参考**:
- GitNexus 的 Leiden 实现（569 commits 的 monorepo，21.7k stars）
- Leiden 算法论文: Traag, V.A., Waltman, L. & van Eck, N.J. (2019)

---

## ISS-004 [improvement] [P1] [closed] ✅
**发现日期**: 2026-04-05
**发现者**: RustClaw
**组件**: gid-core, lsp_client.rs

**描述**:
LSP client 当前只实现了 `textDocument/definition`（正向：这个调用指向哪个定义）。缺少两个关键能力：

1. **`textDocument/references`** — 反向查询：谁调用了这个函数/方法？对 `gid query impact` 至关重要
2. **`textDocument/implementation`** — trait → concrete impl 解析：`trait Handler` 的 `.handle()` 应该连接到所有实现它的 struct

**价值**:
- `references`: impact analysis 从 "猜测" 变成 "精确"。改一个函数，精确知道哪些 caller 受影响
- `implementation`: 解决 trait method 的间接调用问题，当前这类 edge 要么 miss 要么连到错误的 impl

**实现计划** (按优先级):
1. `references` 支持 → impact query 精度飞升
2. `implementation` 支持 → trait 调用解析

---

## ISS-005 [improvement] [P2] [closed]
**发现日期**: 2026-04-05
**发现者**: RustClaw
**组件**: gid-core, code_graph.rs + lsp_client.rs

**描述**:
LSP refinement 中 1,225 条 call edges "failed"（~9.7%），原因是 call site 定位失败：tree-sitter 提取 call edge 时没记录精确的 `(line, col)` 位置，LSP client 需要后期在 caller 函数体内搜索 callee 名字来定位，对 macro 展开、闭包、链式调用等场景搜不到。

**修法**:
tree-sitter 提取 call edge 时直接记录 `call_site_line` / `call_site_col`，跳过后期搜索步骤。

---

## ISS-007 [bug] [P0] [closed]
**发现日期**: 2026-04-06
**发现者**: potato + RustClaw
**组件**: gid-core, code_graph/extract.rs → `collect_source_files()` + unified.rs → `build_unified_graph()`

**描述**:
`gid extract` 对大项目（如 CC，~1,031 源文件）生成大量重复节点。实际 ~850 文件被膨胀到 1,884 file nodes。

**Root Cause**:
`collect_source_files()` 第 117-122 行的 `module_map` partial path 机制：

```rust
let parts: Vec<&str> = module_path.split('.').collect();
for start in 1..parts.len() {
    let partial = parts[start..].join(".");
    module_map.entry(partial).or_insert_with(|| file_id.clone());
}
```

当项目有 `src/Tool.ts` 和 `src/components/Tool.ts` 时：
1. 第一个文件注册 `module_map["Tool"] = "file:src/Tool.ts"` ✅
2. import resolution 遇到 `import Tool from './Tool'` 可能解析成 partial path `"Tool"`
3. `resolve_references()` 将 `module_ref:Tool` 解析到 `"file:src/Tool.ts"` — 但如果 import 来自不同子目录，实际应指向 `"file:src/components/Tool.ts"`

更严重的是 `build_unified_graph()` (unified.rs) 阶段：
- `code_node_to_task_id("file:src/Tool.ts")` → `"code_src_Tool.ts"`
- `code_node_to_task_id("file:Tool.ts")` → `"code_Tool.ts"`（幽灵节点，物理文件不存在）
- 两个不同的 graph node 指向同一个物理文件（或根本不存在的文件）

**影响**:
- 节点数翻倍（1,031 → 1,884 files）
- Edge 指向幽灵节点，图完整性破坏
- 28MB YAML graph 文件（正常应 ~14MB）
- 所有下游分析（impact query, complexity, semantify）的准确性受损

**修复方案**:
在 `extract_from_dir()` 返回 `CodeGraph` 前，加一个 normalization pass：
1. 收集所有 file node 的 canonical ID（`file:{rel_path}`，rel_path 来自 `collect_source_files`）
2. 遍历所有 edges，如果 `from`/`to` 引用了 non-canonical file ID，映射到 canonical ID
3. 删除没有对应 canonical file node 的幽灵 edge targets
4. 在 `build_unified_graph()` 中 dedup 相同物理路径的 code nodes

**验证**: 对 CC 项目重新 extract，确认 file nodes 回到 ~850，无幽灵节点。

---

## ISS-006 [improvement] [P2] [closed] ✅
**发现日期**: 2026-04-05
**发现者**: RustClaw
**组件**: gid-core, code_graph.rs

**描述**:
当前 `gid extract --lsp` 每次全量重建 code graph。改一个文件也要重新解析整个项目 + 重跑所有 LSP 查询。

**改进方向**:
增量更新 — 检测文件变更（mtime/hash），只对变更文件重新提取 + LSP 查询，合并到现有 graph。

---

## ISS-008 [feature] [P2] [open]
**Shared Function Detection — 语义级功能重叠检测**

两层检测：
1. Design-time: graph component 描述相似 → 建议共享模块
2. Code-time: import similarity + type overlap + caller domain → 检测候选函数对

利用 GID 已有的 call graph + LSP 类型信息 + semantify layer。
现有工具（PMD CPD, jscpd）只做语法克隆，这个做语义层。

- **来源**: IDEA-20260406-05 (RustClaw IDEAS.md)
- **触发 case**: `ritual_runner::run_skill` vs `SpawnSpecialistTool` 70% 功能重复
- **建议 Phase**: Phase 3（产品化核心）
- **关联**: ISS-002 (LSP call edges), ISS-003 (Leiden 社区检测)

---

## ISS-009 [bug] [P0] [open]
**Graph 缺少多层连接 — 代码层与任务层完全隔离**

**发现日期**: 2026-04-06
**发现者**: potato + RustClaw
**组件**: gid-core — code_graph/extract.rs, code_graph/types.rs, design.rs, unified.rs, query.rs, semantify.rs

### 问题描述

GID graph 应该是多层结构，层内有细分层级，层间有 edges 连接：

1. **架构层** — project / feature / component / module（手动或从 design doc 生成 + extract 聚合）
2. **任务层** — task 节点（`gid_design --parse` 生成）
3. **代码层** — file / class / function（`gid_extract` 生成）

层间通过 edges 连接：
- feature → component（subtask_of）
- task → component（implements）
- file → module（belongs_to）
- function → file（defined_in）✅ 这个已有

### 现状分析（代码审读 2026-04-06）

#### 已有的类型体系

**CodeGraph (code_graph/types.rs):**
- `NodeKind` enum: File, Class, Function, **Module**, Constant, Interface, Enum, TypeAlias, Trait
- `EdgeRelation` enum: Imports, Inherits, DefinedIn, Calls, **TestsFor**, Overrides, Implements
- Module 和 TestsFor 类型**已定义但从未被 extract 使用**

**Graph (graph.rs):**
- `node_type`: 自由字符串 — "file", "class", "function", "module", "task", "feature", "component"
- `edge.relation`: 自由字符串 — "depends_on", "calls", "imports" 等
- 不受 enum 约束，任何字符串都合法

#### 已有但未串联的连接机制

1. **`unified.rs::build_unified_graph()`** — 合并 CodeGraph + Graph，但只是**并排放到一个 Graph 里**，不创建跨层 edge
2. **`unified.rs::link_tasks_to_code()`** — **存在！** 扫描 task 的 title/description，用文件名做文本匹配，生成 `relates_to` edge。但：
   - 只匹配 file name/path，不看 class/function
   - 用 `relates_to`（弱关系），不是 `implements`（强关系）
   - **从来没被自动调用** — 需要手动触发，没有集成到 extract 或 design 流程里
3. **`unified.rs::merge_relevant_code()`** — 按关键词把相关代码节点加到 task graph，也从未自动调用
4. **`semantify.rs`** — 有 `SemanticProposal::GroupIntoModule` 和 `AddFeature`，可以创建 module/feature 层节点。但需要 LLM 调用（generate prompt → LLM → parse response），不是确定性的

#### 代码层缺陷

**`extract` 不生成 Module 节点：**
- `NodeKind::Module` 在 types.rs 已定义
- `extract_from_dir()` / `extract_incremental()` **从不生成** Module 类型节点
- 目录结构信息丢失 — 文件之间没有按目录聚合

**`extract` 不生成 TestsFor edges：**
- `EdgeRelation::TestsFor` 在 types.rs 已定义
- test 文件（`test_*.rs`, `*_test.py` 等）**只生成普通 file node**，不关联被测源文件
- 结果：改一个源文件，查不到哪些 test 受影响

#### 任务层缺陷

**`design.rs::parse_design_yaml()` 不关联代码节点：**
- 解析 LLM 生成的 YAML 创建 task/feature/component 节点
- 这些节点的 metadata 可能包含文件引用（如 `files: ["src/auth.rs"]`），但**不自动生成 edge**
- task 和代码层之间零连接

#### 查询层缺陷

**`query.rs` 只走 `depends_on`：**
- `impact()` — 只追踪 `relation == "depends_on"` 的反向 edge
- `deps()` — 只追踪 `relation == "depends_on"` 的正向 edge
- **忽略** `calls`, `defined_in`, `imports`, `belongs_to`, `implements`, `relates_to` 等所有其他关系
- 即使跨层 edge 存在，impact/deps 查询也**查不到**

### 实际数据

**RustClaw graph:**
- 1921 nodes: 1482 function + 383 class + 56 file — **0 个 module/task/feature/component 节点**
- 13201 edges: 全是代码层内部的 calls/defined_in/overrides/inherits
- `gid_query_impact` 跨层查询 → 空结果
- `gid_query_deps` 跨层查询 → 空结果

**gid-rs graph:**
- 11 nodes: 全是 doc 类型 — **0 个代码节点**
- 反面极端：只有上层没有下层

### Root Cause Summary

| 组件 | 问题 | 严重程度 |
|---|---|---|
| extract.rs | 不生成 Module 节点（`NodeKind::Module` 已定义未使用） | 🔴 |
| extract.rs | 不生成 TestsFor edges（`EdgeRelation::TestsFor` 已定义未使用） | 🔴 |
| extract.rs | 不生成目录→文件的 belongs_to edges | 🔴 |
| unified.rs | `build_unified_graph()` 只并排合并，不创建跨层 edge | 🔴 |
| unified.rs | `link_tasks_to_code()` 存在但未自动集成到任何流程 | 🟡 |
| design.rs | `parse_design_yaml()` 不从 metadata 生成跨层 edge | 🔴 |
| query.rs | impact/deps 只走 depends_on，忽略所有其他 relation 类型 | 🔴 |

### 修复方案

**Phase 1: Extract 生成完整多层图**
1. 按目录结构生成 Module 节点（每个含源文件的目录 → 一个 module node）
2. file → module: `belongs_to` edge
3. module → parent_module: `belongs_to` edge（嵌套目录）
4. 识别 test 文件，生成 `tests_for` edge 到被测源文件
5. Rust: 用 `mod.rs` / `lib.rs` 识别 module boundary；Python: 用 `__init__.py`；TS: 用 `index.ts`

**Phase 2: Design/Unified 自动跨层连接**
1. `parse_design_yaml()` 解析 task 的 metadata（files, components 字段）→ 自动生成 `implements` edge
2. `build_unified_graph()` 合并后自动调用 `link_tasks_to_code()`
3. `link_tasks_to_code()` 增强：除了文件名匹配，也匹配 module/class
4. 用 `implements` 替代 `relates_to`（强关系 vs 弱关系）

**Phase 3: Query 支持多关系类型遍历**
1. `impact()` 和 `deps()` 接受 `relations: &[&str]` 参数
2. 默认遍历所有 structural relations（depends_on, calls, defined_in, belongs_to, implements）
3. 可过滤：只查 task 层（depends_on）、只查代码层（calls, defined_in）、跨层（implements, belongs_to）

### 影响

这是 GID 作为 "graph-indexed development" 工具的**核心能力缺陷**：
- 没有多层连接，graph 是多个不相关的子图共存在一个文件里
- impact query、dependency tracking、design-to-code traceability 全部失效
- **已定义的类型（Module, TestsFor）从未被使用** — 说明设计时想到了，实现时没做完

**优先级**: P0 — 比 LSP (ISS-002) 更基础。LSP 提高代码层精度，但如果代码层和任务层连不起来，精度高了也没用

---

## ISS-010 [improvement] [P1] [closed]
**Triage Size 驱动 Review 深度分级**

**发现日期**: 2026-04-07
**发现者**: potato + RustClaw
**组件**: gid-core, ritual/state_machine.rs + ritual/v2_executor.rs + review skills (RustClaw skills/)

### 问题描述

Ritual v2 的 triage 结果（small/medium/large）应该影响 review 的**深度**，而不仅仅是 model 选择和是否跳过。

### 现状

已实现（部分）：
1. ✅ **Skip 逻辑** — small + incremental update → 跳过 review
2. ✅ **Model/iterations 选择** — `review_config_for_triage_size()`: small→sonnet/30, medium→opus/50, large→opus/100
3. ❌ **`_max_iterations` 未实际传递** — 变量带下划线，`run_skill()` 没有 max_iterations 参数

未实现：
4. ❌ **Review skill 本身不分级** — 无论 size，都跑同一个 review-design (27 checks) 或 review-requirements skill
5. ❌ **无轻量 review 模式** — small task 不需要跑 "State machine invariants" 或 "Cross-cutting concerns" 等重型 check

### 方案

三级 review 深度，由 triage size 选择：

| Triage Size | Review Depth | Checks | Model |
|---|---|---|---|
| small | **Quick** — structural + naming 只 | Phase 1 + Phase 4 (8 checks) | Sonnet |
| medium | **Standard** — logic + architecture | Phase 1-5 (20 checks) | Opus |
| large | **Full** — 全部 27 checks + path traces | Phase 1-7 (27 checks) | Opus |

实现方式：
1. **Review skill 参数化** — `RunSkill` 的 context 注入 review depth 指令（如 `[REVIEW_DEPTH: quick]`），skill 据此选择执行哪些 phase
2. **或拆分 skill** — `review-design-quick`, `review-design-standard`, `review-design-full` 三个 skill 文件
3. **max_iterations 真正传递** — `run_skill()` 加 max_iterations 参数或在 config 里限制

方式 1 更灵活（一个 skill 文件，参数控制），方式 2 更显式（各自独立）。推荐方式 1。

### 同时修复
- `_max_iterations` 变量实际传递到 `run_skill()` 调用
- requirements review 也做同样的分级（当前只对 design review 有条件跳过）

### 关联
- Ritual v2 triage 逻辑（state_machine.rs Triaging phase）
- review-design skill (RustClaw skills/review-design/SKILL.md)
- review-requirements skill (RustClaw skills/review-requirements/SKILL.md)
