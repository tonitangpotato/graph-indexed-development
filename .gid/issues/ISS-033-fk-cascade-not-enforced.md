# ISS-033: SQLite `PRAGMA foreign_keys` Not Enabled Per-Connection → CASCADE Delete Silently Does Not Cascade

**发现日期**: 2026-04-23
**发现者**: potato + RustClaw
**组件**: gid-core, `storage/sqlite.rs` (connection setup)
**优先级**: P1
**Status:** open
**类型**: bug
**标签**: sqlite, storage, data-integrity, root-fix

---

## 症状

批量删除 code nodes 后，edges 表里留下大量指向已删节点的 "orphan edges"：

```sql
-- 删掉所有 code 节点后
DELETE FROM nodes WHERE node_type='code';
-- 期望：edges 表里所有引用 code node 的边也被删掉（CASCADE）
-- 实际：edges 表不变，全是指向 ghost node 的孤儿边
SELECT COUNT(*) FROM edges WHERE from_node NOT IN (SELECT id FROM nodes);
-- → 11870
```

即使 schema 上写了 `ON DELETE CASCADE`（或者用户以为写了），实际 delete 不 cascade。

## 根因（Root Cause）

SQLite 的外键约束**必须每个 connection 都显式打开**：

```sql
PRAGMA foreign_keys = ON;
```

这条 PRAGMA：
1. **不是 schema 级别的持久设置**——重开 connection 就复位成默认（OFF）
2. **默认值是 OFF**（历史包袱，为了兼容 pre-3.6.19 行为）
3. **必须在事务外执行**（见 ISS-015）
4. **影响所有**引用完整性相关行为：`ON DELETE CASCADE`, `ON DELETE SET NULL`, FK constraint checking

如果 gid 在打开 DB connection 后没有**立即**执行 `PRAGMA foreign_keys = ON`，那么：
- CASCADE 不生效
- FK 违反不报错（可以插入 orphan edge）
- 依赖 FK 的数据完整性保证全部失效

## 证据

**实验 1**：engram monorepo 主图，2026-04-23 23:17：

```bash
$ sqlite3 .gid/graph.db "PRAGMA foreign_keys;"
0                                  # ← OFF，默认值，gid 没打开
$ sqlite3 .gid/graph.db "DELETE FROM nodes WHERE node_type='code'; SELECT COUNT(*) FROM edges;"
12453                              # ← 删了 2706 个 node，边数纹丝不动
```

**实验 2**：显式开 FK 后再试：

```bash
$ sqlite3 .gid/graph.db "PRAGMA foreign_keys=ON; DELETE FROM nodes WHERE node_type='code';"
# → 此时行为取决于 schema 是否声明了 ON DELETE CASCADE
#   若有 CASCADE：edges 自动清理
#   若无 CASCADE：DELETE 本身失败（FK violation），事务 rollback
```

两种结果都比"静默留 orphan"好——前者是期望行为，后者会立刻让用户知道 schema 需要调整。现状（FK OFF）是**最糟的第三种**：既没清理也没报错，数据悄悄腐化。

## 触发链路

任何涉及批量删除或 refactor 的 gid 操作都可能留 orphan：

1. `gid remove-node` 大量调用（人工 refactor）
2. `gid extract` 的 re-extract 路径（如果实现用 DELETE + INSERT 而非 UPSERT）
3. 用户手动清理（如 RustClaw 今晚的主图回滚）
4. 未来 `gid repair` / `gid merge` 等操作

每一处都要自己在应用层手动清 orphan，非常脆弱。

## 副问题（需要一起确认）

1. **Schema 是否实际声明了 `ON DELETE CASCADE`？**
   - 需要 audit `storage/sqlite.rs` 里的 `CREATE TABLE edges (...)` 语句
   - 如果没有：只开 PRAGMA 不够，还要 ALTER TABLE 或 migration
   - 如果有：只开 PRAGMA 就够了
2. **ISS-015 修完后 migration batch 外的普通连接是什么状态？**
   - ISS-015 修的是 migration 事务内的 no-op PRAGMA
   - 普通查询 / 普通 extract 的 connection setup 路径是否也开了 FK？需要 grep `PRAGMA foreign_keys` 看覆盖面
3. **LSP pass 1 产出的 dangling edges（ISS-016）**与本 issue 是否同源？
   - ISS-016 的 dangling 边是 LSP symbol 解析失败留下的，不是 node 删除造成的
   - 但都暴露了"FK 未强制 → 应用层责任散落"这个根本问题
   - 如果 FK 开启，LSP pass 1 插入 dangling edge 时就会直接失败，问题在写入点暴露而不是读取点

## 修复方案（Root Fix）

### 1. Connection 建立时立即开 FK（主修复）

`SqliteStorage::open` / `::connect` / 任何获取 rusqlite `Connection` 的地方，第一件事：

```rust
fn configure_connection(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch("
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;          -- 已有？audit
        PRAGMA synchronous = NORMAL;        -- 已有？audit
    ")?;
    Ok(())
}
```

**必须在每个新建 connection 上执行，一次。** SQLite 的 PRAGMA 是 per-connection state，不能指望 schema 记住。

### 2. Audit schema 的 CASCADE 声明

检查 edges 表的 FK 声明：

```sql
-- 期望看到：
CREATE TABLE edges (
    from_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    to_node   TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    ...
);
```

如果没有 `ON DELETE CASCADE`：
- 方案 A（推荐）：用 migration 重建 edges 表加上 CASCADE
- 方案 B：不加 CASCADE，但开 FK —— 此时 DELETE node 会因 FK 违反而失败，应用层必须先删 edges（显式、可审计，也能接受，只是更冗长）

决定方案前先 audit，不要拍脑袋。

### 3. 加入启动健康检查（防御性）

在 `SqliteStorage::open` 成功后，立刻 verify：

```rust
let fk_on: i32 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
if fk_on != 1 {
    return Err(StorageError::ConfigError("PRAGMA foreign_keys failed to enable".into()));
}
```

这样任何 connection 配置失败都会在打开时立刻报错，不会悄悄以 FK OFF 状态运行。

### 4. 清理 migration batch 里的 PRAGMA OFF/ON 组合（如需要）

ISS-015 修复后，migration batch 的 PRAGMA 已经移到事务外。现在全局 FK 默认 ON 之后，migration batch 里可能需要临时 OFF（比如 bulk import 时）。要确保：
- 临时 OFF 在事务外执行
- OFF 仅限 migration batch 作用域
- 结束后立刻 ON
- 如果中间 crash，下次 connection 重开会恢复 ON（因为是 per-connection state，不持久化）——这是天然的 fail-safe

## 验收标准

- [ ] `SqliteStorage::open` 后，`PRAGMA foreign_keys` 返回 1
- [ ] 测试：删除 node 时，CASCADE 生效，相关 edges 自动删除（如果 schema 声明了 CASCADE）
- [ ] 测试：插入 from_node 指向不存在 node 的 edge 会失败（FK constraint violation）
- [ ] Schema audit 报告：edges 表是否有 `ON DELETE CASCADE`，如没则有 migration 补上
- [ ] 健康检查：任何 connection 打开后立刻 verify FK=ON，否则报错
- [ ] 回归测试：在 RustClaw 今晚的污染场景上——extract 跑错一次后，`gid remove-node` 主动清 code layer，自动 cascade 清 edges，不留 orphan
- [ ] 文档：README / storage module doc 里明确"gid 依赖 SQLite FK enforcement"

## 影响范围

- gid-core: `storage/sqlite.rs` connection setup；可能有 schema migration
- 行为变化：
  - **破坏性（好的意义上）**：之前静默存入的 dangling edge 写入路径现在会失败——这会暴露 ISS-016 等未解决的 LSP dangling 问题
  - 需要先修（或至少审视）ISS-016 再开 FK，否则 LSP extract 直接报错
- 依赖：ISS-015 已修（PRAGMA 能在事务外正确执行），是本 issue 的前置
- ISS-032 依赖：一旦 FK + CASCADE 工作，orphan edge 问题从源头减少，`gid repair --orphans` 主要用于清理历史遗留 + 手动 SQL 造成的 orphan

## 关联

- **依赖**: ISS-015（已修）——前置，FK PRAGMA 的事务外执行能力
- **可能触发**: ISS-016（LSP pass 1 dangling edges）——开 FK 后会让这个问题从"静默"变"显式失败"，修本 issue 前应先评估 ISS-016 状态
- **搭配**: ISS-032（`gid repair` 命令）——FK+CASCADE 工作后，repair 主要负责"已经存在的历史 orphan" 和 "duplicate 合并"，边界更清晰
- **触发案例**: RustClaw memory/2026-04-23.md "GID usage rules added to AGENTS.md (23:20)"
