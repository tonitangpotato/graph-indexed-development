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

## ISS-007 [bug] [P0] [open]
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
