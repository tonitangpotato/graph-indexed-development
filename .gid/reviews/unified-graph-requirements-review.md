# Review: unified-graph/requirements.md (post-refactor completeness check)

**Reviewer**: RustClaw  
**Date**: 2026-04-07  
**Focus**: Verify requirements completeness after WHAT/HOW split  
**Documents reviewed**: requirements.md (34 GOALs, 4 GUARDs), design.md (cross-reference)

---

## 🔴 Critical (blocks implementation)

### FINDING-1: GOAL-6.8 编号乱序 ✅ Applied
**Check #20 (Naming consistency)**: GOAL-6.8 插在 GOAL-6.2 和 GOAL-6.3 之间。编号跳跃打破顺序性，读者会困惑是否有缺失的 GOAL-6.3~6.7。应该重排为连续编号（6.1→6.2→6.3...→6.8）或把 6.8 移到 section 末尾。

**Suggested fix**: 将当前 GOAL-6.8 内容移到 GOAL-6.7 之后（section 末尾），保持原编号不变（因为 design.md 和其他地方已引用）。或者更好的方案：重新编号整个 §6 为 6.1-6.8 连续排列：
- 6.1 层定义
- 6.2 `--layer` 过滤
- 6.3 summary/ready_tasks 隔离（当前 6.8）
- 6.4 visual 层过滤（当前 6.3）
- 6.5 query 层控制（当前 6.4）
- 6.6 写入隔离（当前 6.5）
- 6.7 stats 按层统计（当前 6.6）
- 6.8 性能（当前 6.7）

这需要同步更新 design.md 引用。**建议保持编号不变，只调整位置到末尾。**

**Applied**: Moved GOAL-6.8 to end of §6 (after GOAL-6.7), kept original ID.

---

## 🟡 Important (should fix before implementation)

### FINDING-2: GOAL-1.1 清理后过于模糊 ✅ Applied
原始版本："代码节点使用 `graph::Node` 类型，字段含义清晰区分层归属和节点种类"  
清理后版本："代码节点的层归属和节点种类可清晰区分"

**问题**：清理掉了 HOW（好），但剩下的 WHAT 不够具体——"可清晰区分" 怎么验证？谁来判断"清晰"？缺少验收标准。

**Suggested fix**: 改为："`gid extract` 将代码节点写入 `graph.yml` 而非 `code_graph.json`。每个代码节点携带层归属标识和精确种类标识（如 function/struct/module），两者语义不重叠。验证：从 graph.yml 加载后，能通过层标识过滤出全部代码节点，且种类标识保留 extract 原始粒度。"

**Applied**: Replaced GOAL-1.1 with suggested fix text including verification criteria.

### FINDING-3: GOAL-3.1 和 GOAL-3.4 优先级不一致 ✅ Applied
GOAL-3.4 [P0] 定义手动 code_paths 为"主要桥接机制"，GOAL-3.1 [P1] 定义自动匹配为"fallback"。但 GOAL-3.1 说"extract 完成后自动生成桥接边"——如果 3.4 的手动机制是 P0 主路径，那 3.1 的描述应该更明确它是 fallback，不是独立功能。

**Suggested fix**: GOAL-3.1 开头改为："当 feature 节点未手动指定关联代码路径（见 GOAL-3.4）时，extract 完成后自动生成 feature→code 桥接边作为 best effort fallback。"

**Applied**: Rewrote GOAL-3.1 opening to explicitly reference GOAL-3.4 and clarify fallback role.

### FINDING-4: GOAL-4.1 / GOAL-4.2 近乎重复 ✅ Applied
GOAL-4.1 说 `impact` 能跨层遍历，GOAL-4.2 说 `deps` "同理"。"同理" 不是有效的需求描述——如果 deps 有不同的行为（反向遍历方向），应该明确写出来。如果真的完全对称，合并为一条。

**Suggested fix**: 合并为 "GOAL-4.1 [P0]: `gid query impact <node>` 和 `gid query deps <node>` 能从任意节点类型出发跨层遍历。impact 沿依赖方向展开（task → feature → code），deps 沿反向追踪。验证：对一个 task 节点查 impact/deps，返回结果中包含 code 类型节点。" 删除 GOAL-4.2 或标记为 merged。

**Applied**: Merged GOAL-4.1 and GOAL-4.2 into single GOAL-4.1 with both impact and deps described. Removed GOAL-4.2.

### FINDING-5: GOAL-6.1 缺少验收条件 ✅ Applied
"每个节点有明确的 layer 归属。定义两个逻辑层（code / project）和一个标注维度（semantic 架构层标签）。" 

这是一个定义声明，不是可验证的需求。验证标准是什么？

**Suggested fix**: 追加验证条件："验证：graph.yml 中每个节点可被判定为 code 或 project 层之一，无歧义节点。semantic 标签独立于层判断，不影响 `--layer` 过滤结果。"

**Applied**: Appended verification criteria to GOAL-6.1.

### FINDING-6: GOAL-8.x 是迁移任务而非需求 ✅ Applied
GOAL-8.1~8.5 描述的是"迁移 X 模块到新接口"——这些更像 tasks（implementation work items），不是 requirements（WHAT the system does）。Requirements 应该描述系统的外部行为，不是内部代码结构变更。

**Suggested fix**: 把 §8 的角色明确化——如果保留，改标题为 "8. 内部接口迁移 (internal-migration)" 并加注："以下 GOALs 描述内部模块的接口迁移要求，作为实现统一图后的必要清理工作。" 或者更好的方案：降级为 design.md 中的 migration checklist，从 requirements 移除。

**Applied**: Changed §8 title to "内部接口迁移 (internal-migration)" and added clarifying blockquote.

### FINDING-7: Overview 仍包含 HOW 暗示 ✅ Applied
"extract 直接写入 graph.yml（按 node type 隔离），semantify 自动运行，桥接边自动生成" — 这描述了实现方案，不是问题陈述。Overview 应该只说 WHAT problem 和 WHY we need this.

**Suggested fix**: 改最后一句为："本 feature 实现真正的统一图：代码结构和项目管理共享同一个持久化图，支持跨层查询和分层控制。"

**Applied**: Replaced HOW-oriented last sentence of Overview with WHAT-oriented description.

---

## 🟢 Minor (can fix during implementation)

### FINDING-8: GOAL-1.2 列举具体边类型是否属于 HOW ✅ Applied
"代码边（imports, calls, inherits, defined_in, TestsFor, BelongsTo, Overrides, Implements）" — 这个列表本身是 WHAT（定义了需要支持的边类型），但格式不一致（有的 snake_case 有的 PascalCase）。

**Suggested fix**: 统一为 snake_case 或注明 "以下边类型需要支持"，格式由 design 决定。

**Applied**: Normalized all edge types to snake_case (TestsFor→tests_for, BelongsTo→belongs_to, Overrides→overrides, Implements→implements).

### FINDING-9: GOAL-5.3 混合了两个里程碑 ✅ Applied
"CodeGraph 类型不再作为持久化格式暴露。P2 里程碑：移除 CodeGraph 类型和转换层。" — 一条 GOAL 包含了一个 P1 需求和一个 P2 规划。

**Suggested fix**: 拆分：GOAL-5.3 [P1] 只保留 "CodeGraph 类型不再作为持久化格式暴露"。P2 移除计划移到 Out of Scope 或 design doc。

**Applied**: GOAL-5.3 now only contains P1 requirement. P2 removal plan moved to Out of Scope section.

### FINDING-10: Dependencies section 的注释是 action item ✅ Applied
"本 feature 完成后，sqlite-migration/requirements.md 的 Out of Scope 条目需同步更新" — 这是一个 TODO，不是需求。

**Suggested fix**: 移到 design.md 的 migration checklist 或创建一个 task。

**Applied**: Removed TODO note from Dependencies section. Added to design.md as "Post-Completion Checklist" item.

---

## ✅ Passed Checks

- **Completeness**: 34 GOALs + 4 GUARDs，全部有编号、优先级、描述 ✅
- **No dangling design references**: 清理干净，无 `*(详见 design.md § ...)*` 残留 ✅
- **WHAT/HOW boundary**: 大部分 GOALs 是纯 WHAT ✅ (除 FINDING-2, 7)
- **Priority distribution**: 14× P0, 16× P1, 2× P2, 2× hard GUARD, 2× soft GUARD — 合理 ✅
- **Cross-reference with design**: 26/34 GOALs 在 design.md 有对应实现段落，剩余 8 个不需要 design 展开 ✅
- **Guard coverage**: GUARD-1~4 覆盖数据安全、原子性、序列化、性能 ✅
- **Out of Scope**: 明确排除 SQLite/LSP/多仓库/schema版本 ✅
- **Dependencies**: 正确列出 ISS-009, ISS-006, T2.5 ✅
- **Self-containedness**: Requirements 可独立阅读，不依赖 design doc ✅
- **Naming convention**: 全部使用 GOAL/GUARD，无 CR/INV/REQ 等别名 ✅

---

## 📊 Summary

| Severity | Count |
|----------|-------|
| 🔴 Critical | 1 |
| 🟡 Important | 6 |
| 🟢 Minor | 3 |
| **Total** | **10** |

**Recommendation**: Fix FINDING-1 (ordering) + FINDING-2 (vague GOAL) + FINDING-5 (missing verification) before proceeding. The rest can be addressed during or after implementation.
