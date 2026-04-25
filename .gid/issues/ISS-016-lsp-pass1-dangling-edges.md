# ISS-016: LSP Pass 1 Produces Dangling Edges on `find_closest_node` Miss

**发现日期**: 2026-04-20
**发现者**: potato + RustClaw
**组件**: gid-core, `code_graph/build.rs` (LSP refinement Pass 1)
**优先级**: P2 (cleanup — 不阻塞功能，但污染图质量)
**Status:** closed (2026-04-23)
**类型**: cleanup / correctness
**标签**: lsp, code-graph, dangling-edge

---

## 解决记录 (2026-04-23)

采用 **Option A**（skip update on miss）。两条降级分支不再 push `(idx, None, ...)` update，而是增加 `stats.refinement_skipped` 计数后直接跳过。原 tree-sitter edge 保持不变，由 baseline/Pass 2 处理。

**改动**:
- `crates/gid-core/src/lsp_client.rs` — `LspRefinementStats` 新增 `refinement_skipped: usize` 字段
- `crates/gid-core/src/code_graph/build.rs:514-527` — 两条降级分支改为 skip + stat
- `crates/gid-cli/src/main.rs:2147` — CLI 打印加入 `refinement_skipped`
- 新增单元测试 `test_refinement_stats_has_refinement_skipped_field`

**验证** (TypeScript 项目 xinfluencer/website, typescript-language-server):
- LSP refinement: 16 refined, 0 removed, 12 failed, 0 skipped, **0 refinement_skipped**
- `SELECT COUNT(*) FROM edges WHERE to_node NOT IN (SELECT id FROM nodes)` → **0**
- 全项目 `cargo test --workspace --all-features` 1028/1028 pass

---

## 背景

ISS-015 修复了 `execute_migration_batch` 的 FK 问题后，dangling edges 能正确落库为 warning（符合 GOAL-2.9）。但 LSP refinement Pass 1 **本来就不应该主动产出 dangling edges** — 这是上游数据质量问题，ISS-015 的修复只是让下游 storage 不崩。

## 症状

LSP refinement 后 `graph.db` 中出现 call edges 指向不存在的 node（dangling）。这些 edge 来自 Pass 1 的降级分支，不是真实的调用关系 — 是"没找到精确目标但又不想丢掉原 edge"的妥协产物。

## 现有代码（源头）

```rust
// crates/gid-core/src/code_graph/build.rs:505-524
if let Some(file_index) = def_index.get(&graph_file_path) {
    if let Some(target_id) = find_closest_node(file_index, location.line, 5) {
        edges_to_update.push((idx, Some(target_id), 1.0));           // ✅ 精确匹配
    } else {
        edges_to_update.push((idx, None, edge.confidence.max(0.6))); // ⚠️ target=None
    }
} else {
    edges_to_update.push((idx, None, edge.confidence.max(0.6)));     // ⚠️ target=None
}
```

两条降级分支写入 `target=None` 的 update，交给下游应用到 edge 上。如果原 edge 的 target 已经被其他 Pass 清理/重命名，最终 edge 就 dangling。

## 根因

**"找不到精确目标 → 保留旧 target 并降 confidence"** 这个策略有两个问题：

1. **旧 target 可能已失效** — LSP refinement 是多 Pass，其他 Pass 可能已经动过 node 表。保留旧 target 等于赌它还在，赌输了就 dangling。
2. **`find_closest_node` 窗口固定为 5** — 这个魔数没有依据。LSP 定位到 `location.line` 是**定义行**，5 行窗口对 long function 不够，对 dense file 又太宽。失败不代表"找不到"，可能只是窗口设置差。

## 修复方案（Clean）

### Option A：失败 = 丢弃该 edge 的 refinement（推荐）

Pass 1 找不到精确目标 → **不产出 update**。让原 edge 保持 pre-LSP 的 tree-sitter 匹配结果。LSP 只做"能做的改进"，做不了的交给 baseline。

```rust
if let Some(file_index) = def_index.get(&graph_file_path) {
    if let Some(target_id) = find_closest_node(file_index, location.line, 5) {
        edges_to_update.push((idx, Some(target_id), 1.0));
        stats.refined += 1;
    } else {
        stats.refinement_skipped += 1;  // 新增统计
        // 不产出 update — edge 保持 tree-sitter 原状
    }
} else {
    stats.refinement_skipped += 1;
    // 同上
}
```

**优势**：
- 零 dangling 产出 — storage 层不再靠 FK-off 兜底
- 语义清晰：LSP 是"精确化工具"，不能精确就别硬上
- stats 区分 `refined` vs `refinement_skipped`，可观测性更好

**风险**：
- 原 tree-sitter edge 可能本身就 dangling（独立问题，不在本 issue 范围）
- 需要 Pass 1 之外的 Pass 能处理"没被 LSP 动过"的 edge — 检查现有流水线是否假设所有 edge 都被 Pass 1 touched

### Option B：动态扩大 `find_closest_node` 窗口

第一次 5 行失败 → 扩到 15 行重试。能降低 miss rate，但治标不治本 — 仍会在极端情况产出 dangling。不推荐单独用，可以作为 A 的补充。

### Option C：失败 → 删除该 edge

激进方案：LSP 说"这个 call site 没对应定义"，那就当它不是真的 call。删 edge。

**不推荐**：LSP 有自己的索引盲区（宏展开、条件编译、生成代码），贸然删 edge 会丢真实关系。

---

**推荐组合：Option A + Option B 作为 A 的精确化**。先按 A 重构产出语义，再观察 `refinement_skipped` 比例，高的话再加 B 的窗口扩大。

## 验收标准

1. ✅ LSP refinement 后 `SELECT COUNT(*) FROM edges WHERE to NOT IN (SELECT id FROM nodes)` == 0
2. ✅ `refinement_skipped` 统计被打印出来，供观察
3. ✅ 现有 LSP-相关测试全绿
4. ✅ 在 RustClaw 项目上 `gid extract --lsp` 的 `refined` 计数基本不变（说明 Option A 没误伤精确匹配路径）

## 前置依赖

**ISS-015 必须先修**。否则现状下 dangling edges 直接让 transaction rollback，本 issue 的行为差异观察不到。

## 影响范围

**改动文件**:
- `crates/gid-core/src/code_graph/build.rs` — Pass 1 降级分支
- `crates/gid-core/src/code_graph/stats.rs`（或同名结构所在）— 新增 `refinement_skipped`

**回归风险**: 中。如果下游代码假设"Pass 1 touched 所有 edge"，可能会有隐式依赖。需要审计 Pass 2/3 对 edge 状态的假设。

## 关联

- ISS-015 — 下游 storage FK 修复（本 issue 的前置）
- ISS-004 — LSP client 整体
- `code_graph/build.rs:505-524` — 问题代码位置
- `lsp_client.rs:1138` — `find_closest_node` 实现（5 行窗口魔数）
