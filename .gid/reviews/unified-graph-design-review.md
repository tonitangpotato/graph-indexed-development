# Review: unified-graph/design.md (post-completion)

**Reviewer**: RustClaw  
**Date**: 2026-04-07  
**Documents reviewed**: design.md (299 lines), requirements.md (33 GOALs, 4 GUARDs), graph.rs (source code)

---

## 🔴 Critical (blocks implementation)

### FINDING-1: Edge metadata 类型不匹配 ✅ Applied
Design 多处写 `edge.metadata["source"] == "extract"` / `== "auto-bridge"`，暗示 metadata 是 `HashMap<String, String>`。

**实际类型**：`Edge.metadata` 是 `Option<serde_json::Value>`（graph.rs line ~78）。访问方式应为：
```rust
edge.metadata.as_ref()
    .and_then(|m| m.get("source"))
    .and_then(|v| v.as_str())
    == Some("extract")
```

这不是写法风格问题 — 如果实现者按 design 字面写 `edge.metadata["source"]`，会编译失败。

**Suggested fix**: 在 ADR-2 或 Schema Design 开头加一段 "Edge Metadata 访问约定"，明确 `Edge.metadata: Option<serde_json::Value>` 的实际类型，提供访问 helper 函数签名：
```rust
fn edge_source(edge: &Edge) -> Option<&str> {
    edge.metadata.as_ref()?.get("source")?.as_str()
}
```
后续各处引用改为 `edge_source(edge) == Some("extract")`。或者更好的方案：在 Edge 上加 `source()` 方法。

**Applied**: Added "Edge Metadata 访问约定" subsection under ADR-2 with helper function signature and usage convention.

### FINDING-2: design --parse 的 source 值未确定 ✅ Applied
§6.5 写入路径隔离表格说 `source: "design"` **或无 source 字段**。这是两种不同的行为，实现者需要做一个选择。如果用 `None`，则 project 层有两种 source 状态（`None` 和 `Some("design")`）；如果统一用 `"design"`，则旧节点和新节点 source 不同。

**Suggested fix**: 明确决策：design --parse 写入的节点 **不设置 source**（`None`），与手动创建节点一致。理由：(1) `None` 天然被判定为 project 层（ADR-1 + 向后兼容 §7.1 已定义）; (2) 不引入新的 source 值; (3) 和 `gid add-task` 行为一致。删除"或设为 `"design"`"的歧义描述。

**Applied**: Changed §6.5 table to `无 source 字段（None）`, updated isolation mechanism text to remove ambiguity.

---

## 🟡 Important (should fix before implementation)

### FINDING-3: 全量 extract 步骤 4 逻辑有误 ✅ Applied
§1.5 步骤 4 说 "额外删除 `edge.metadata["source"] == "extract"` **且** `edge.metadata["source"] == "auto-bridge"` 的边"。

一条边的 source 不可能同时等于 "extract" 和 "auto-bridge"。这里应该是 **或**（OR）。

**Suggested fix**: 改为 "额外删除 `edge_source == "extract"` **或** `edge_source == "auto-bridge"` 的边"。

**Applied**: Changed "且" to "或" in §1.5 step 4, also updated to use `edge_source` convention per FINDING-1.

### FINDING-4: 增量 extract 步骤 ③ 与 GOAL-1.5 步骤 4 的清理范围不一致 ✅ Applied
增量流程（§1.3）说 "删除旧 `auto-bridge` 边 → 生成新的"，只提 auto-bridge。
全量流程（§1.5）说 "删除 extract + auto-bridge 的边"。

但增量流程步骤 ① 也说 "删除 `source=extract` 的边"。所以实际上增量和全量都清理两种边——但行文不一致，增量步骤 ③ 只提了 auto-bridge。

**Suggested fix**: 增量流程改为更明确的三步：
```
① 删除 modified/deleted 文件的旧 code 节点（by extract-meta.json node_ids）+ 这些节点关联的 `source=extract` 边
② 写入新 code 节点 + `source=extract` 边
③ 删除所有 `source=auto-bridge` 边 → 重新匹配生成桥接边
```

**Applied**: Rewrote incremental extract steps with explicit scope for each step.

### FINDING-5: GUARD-4 性能目标 "20%" 缺少测量方法 ✅ Applied
§Performance 说 "extract 写入 graph.yml 的性能不应劣化超过 20%（对比写 code_graph.json）"。但没有指定怎么测量：什么项目规模？冷启动还是热启动？包不包括 semantify 时间？

**Suggested fix**: 明确 benchmark 条件：
- 项目规模：500 文件 / ~2000 code 节点
- 测量：extract wall time（不含 semantify）
- 对比基线：当前 extract → code_graph.json 的时间
- 方法：`hyperfine` 或 `criterion` benchmark

**Applied**: Added benchmark conditions (project scale, measurement scope, baseline, method) under GUARD-4 performance target.

### FINDING-6: §4.3 节点类型过滤的 `--type` flag 未在 Layer Filtering 中定义 CLI 语法 ✅ Applied
§4.3 说 `gid query impact/deps --type <filter>`，但 §6 Layer Filtering 只定义了 `--layer` flag。`--type` 和 `--layer` 的交互没有说明：
- `gid query impact --layer code --type function` — 两个过滤器同时生效？
- `--type` 只在 `--layer all` 时有意义？还是也能在 `--layer project` 时过滤 task/feature？

**Suggested fix**: 在 §4.3 或 Layer Filtering 中加一段 `--type` 和 `--layer` 的交互规则：
- `--type` 在任何 `--layer` 模式下都生效
- `--layer` 先过滤可见节点集，`--type` 再从可见节点中过滤
- 即：`--layer project --type task` = 只显示 task 节点；`--layer code --type function` = 只显示 function 节点

**Applied**: Added "`--type` 与 `--layer` 交互规则" subsection under §4.3 节点类型过滤.

### FINDING-7: 桥接边 "全量重建" 策略的性能未量化 ✅ Applied
§3.3 说桥接边采用全量重建，"性能开销可接受"。但没有给出估算。如果一个项目有 50 个 feature 节点和 2000 个 code 节点，全量重建要做 50×2000 = 100K 次前缀匹配。这 OK 吗？

**Suggested fix**: 加一行估算："`N_feature × N_code` 次字符串前缀比较，典型项目 `50 × 2000 = 100K` 次，<10ms。即使 `500 × 20000 = 10M` 次也在 100ms 量级，远低于 YAML I/O 开销。"

**Applied**: Added performance estimate paragraph after 桥接边 "实现要点" section.

### FINDING-8: ADR-5 的 "无需修改 QueryEngine" 假设需要验证 ✅ Applied
ADR-5 说 impact/deps 遍历自动跨层，因为桥接边存在。但 QueryEngine 的遍历是否有 `relation` 白名单？如果只遍历 `depends_on` 边，`maps_to` 桥接边不会被遍历。

**Suggested fix**: 加验证条件："`QueryEngine` 的 BFS/DFS 遍历当前是否对 `relation` 做过滤？如果是，需要将 `maps_to` 加入可遍历关系列表。需要检查 `graph.rs` 中 `impact()` 和 `deps()` 的实现。" 如果确认不过滤 relation，标注 "已验证：遍历不过滤 relation 类型"。

**Applied**: Added verification conditions paragraph to ADR-5.

---

## 🟢 Minor (can fix during implementation)

### FINDING-9: "P2 里程碑" 分散在多处 ✅ Applied
ADR-3 提到 P2 里程碑（消除 CodeGraph），§8 Caller Migration 也提到 Node 类型枚举化是 P2。这些 P2 项分散在 design doc 各处，没有汇总。

**Suggested fix**: 在 Post-Completion Checklist 下面加 "## P2 Roadmap" section，汇总所有 P2 项：
- 消除 CodeGraph 中间类型
- Node 类型枚举化（String → NodeType enum）
- SQLite 后端层过滤优化

**Applied**: Added "## P2 Roadmap" section after Post-Completion Checklist with all 4 P2 items consolidated.

### FINDING-10: code_paths 不支持通配符的理由不充分
§3 说 "不支持通配符（简化实现）"。但 glob pattern 在 Rust 中有成熟的 crate（`glob`、`globset`），实现成本不高。如果真的决定不支持，应该说明为什么前缀匹配够用。

**Suggested fix**: 改为 "不支持通配符。前缀匹配已覆盖主要场景（按目录关联），通配符需求可用多个 code_paths 条目替代（如 `["src/auth", "src/middleware/auth.rs"]`）。如果后续发现不够，通配符作为 P2 增强。"

### FINDING-11: Deprecation Strategy 缺少时间线
§5 说 `build_unified_graph()` 标记 deprecated 并在"下一个大版本"移除，但没有定义什么是 "下一个大版本" 或留多久。

**Suggested fix**: 加一行："deprecated 标记在合并后的下一个 minor 版本发布（v0.X+1），一个 minor 版本后（v0.X+2）移除。"

### FINDING-12: Post-Completion Checklist 只有一项
sqlite-migration/requirements.md 更新。应该还有：
- 更新 README/docs 中关于 code_graph.json 的描述
- 更新 gid-cli `--help` 文档中的 extract 行为描述
- 通知 RustClaw 侧更新 GidExtractTool 的 tool description

**Suggested fix**: 补充 checklist 项。

---

## ✅ Passed Checks

- **GOAL coverage**: 33/33 GOALs + 4/4 GUARDs 全覆盖 ✅
- **No stale refs**: design 中无 GOAL-4.2（已清理）✅
- **ADR quality**: 5 个 ADR 每个都有决策理由和替代方案分析 ✅
- **Schema 与源码一致**: `Node.source: Option<String>` + `serde(default)` 匹配 design 描述 ✅
- **层判断逻辑自洽**: 所有地方用 `source == "extract"` 判断 code 层，`None` fallback 到 project 层 ✅
- **Migration plan 完整**: P0/P1 调用方均列出文件位置和迁移方式 ✅
- **向后兼容**: 旧 graph.yml 处理完整，不需要迁移脚本 ✅
- **桥接边设计**: 优先级明确（手动 > 自动），生命周期管理通过全量重建简化 ✅
- **Deprecation path**: 清晰的 3 步路径（新接口 → deprecated → 移除）✅

---

## 📊 Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 2 |
| 🟡 Important | 6 |
| 🟢 Minor | 4 |
| **Total** | **12** |

**Recommendation**: FINDING-1（Edge metadata 类型）和 FINDING-2（source 值决策）must fix — 实现者会直接撞上这两个问题。FINDING-3~8 应在实现前修复。FINDING-9~12 可以在实现过程中处理。
