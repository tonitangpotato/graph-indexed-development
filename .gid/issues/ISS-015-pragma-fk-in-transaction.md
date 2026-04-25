# ISS-015: PRAGMA foreign_keys Inside Transaction is a No-op → Migration Batch Fails on Dangling Edges

**发现日期**: 2026-04-20
**发现者**: potato + RustClaw
**组件**: gid-core, `storage/sqlite.rs` (`execute_migration_batch`)
**优先级**: P0 (blocker)
**Status:** closed
**类型**: bug
**Fix commit**: 941c132 — `fix(storage): ISS-015 — move PRAGMA foreign_keys out of transaction + distinguish constraint types`
**Closed**: 2026-04-25 (status sync — fix shipped earlier, index was stale)
**标签**: sqlite, storage, lsp, migration, root-fix

---

## 症状

`gid extract --lsp` 在 LSP refinement 阶段失败。Transaction rollback，整批 LSP 细化结果丢失，最终 extract 拿不到精确 call edge。

错误路径（error classifier 报出的）统一是 `StorageError::ForeignKeyViolation` — 但这掩盖了真实原因（SQLite 只返回通用的 `ConstraintViolation`，见下文"副问题"）。

## 根因（Root Cause）

SQLite 的 `PRAGMA foreign_keys` **只能在事务外修改**。在事务内写 `PRAGMA foreign_keys = OFF` 是静默 no-op — SQLite 不返回错误，但 FK enforcement 不会被关掉。

现有代码把这条 PRAGMA 写在事务内：

```rust
// crates/gid-core/src/storage/sqlite.rs:178-212
pub fn execute_migration_batch(&self, ops: &[BatchOp]) -> Result<(), StorageError> {
    let mut conn = self.conn.borrow_mut();
    let tx = conn.transaction()?;              // ← tx 开始

    // Disable FK enforcement for migration
    tx.execute_batch("PRAGMA foreign_keys = OFF")?;  // ← no-op！FK 仍然 ON

    for op in ops { /* ... */ }

    // Re-enable FK enforcement
    tx.execute_batch("PRAGMA foreign_keys = ON")?;   // ← no-op

    tx.commit()?;
    Ok(())
}
```

**这已经被源码注释和 test 标记为已知 bug 但未修复**：
- `sqlite.rs:1956-1968` — `test_migration_batch_fk_disabled_bug` 断言 FK violation 会发生，锁死了错误行为
- `migration.rs:514-523` — 注释说明意图是禁 FK，实现没做到

## 触发链路（为什么 extract --lsp 会踩到）

LSP refinement 的 Pass 1 要把粗糙的 call edge 重定向到精确目标：

```rust
// crates/gid-core/src/code_graph/build.rs:505-524
if let Some(file_index) = def_index.get(&graph_file_path) {
    if let Some(target_id) = find_closest_node(file_index, location.line, 5) {
        edges_to_update.push((idx, Some(target_id), 1.0));  // ← 成功
    } else {
        edges_to_update.push((idx, None, edge.confidence.max(0.6)));  // ← ⚠️ 匹配失败，target=None
    }
} else {
    edges_to_update.push((idx, None, edge.confidence.max(0.6)));       // ← ⚠️ 文件不在 def_index
}
```

`find_closest_node` 在 5 行窗口内找不到候选节点时返回 `None`。这种情况下边被标记为保留原 target — 但原 target 可能已经不在 node 表里（被其他 Pass 清理或重命名），形成 **dangling edge**。

这批 edges 经 `execute_migration_batch` 写入，设计上依赖 `PRAGMA foreign_keys = OFF` 允许 dangling edge 先落库（GOAL-2.9 要求 dangling 是 warning 不是 error）。但 PRAGMA 无效 → FK 拦截 → `INSERT` 失败 → transaction rollback → **整批 refinement 丢失**。

## 副问题（需要一起修）

`crates/gid-core/src/storage/sqlite.rs:34-41` 把所有 `ConstraintViolation` 一刀切映射成 `StorageError::ForeignKeyViolation`：

```rust
rusqlite::Error::SqliteFailure(e, _)
    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
{
    StorageError::ForeignKeyViolation { ... }  // ← 把 UNIQUE/CHECK/NOT NULL 也归为 FK
}
```

SQLite 有 6 种 constraint（FK, UNIQUE, CHECK, NOT NULL, PRIMARY KEY, TRIGGER）。extended error code 能区分（`SQLITE_CONSTRAINT_FOREIGNKEY` = 787 等），但当前代码把所有 787/275/531/1299/1555/1811 全报成 FK。调试时极度误导 — 一个 UNIQUE 冲突看起来像 FK bug，实际不是。

## 修复方案（Root Fix，Clean & Elegant）

### 1. PRAGMA 移到事务外（主修复）

不能在 `Transaction` 实例存在时改 PRAGMA。正确做法：在拿事务**之前**改，commit 之后恢复。

```rust
pub fn execute_migration_batch(&self, ops: &[BatchOp]) -> Result<(), StorageError> {
    let mut conn = self.conn.borrow_mut();

    // ⚠️ MUST be outside any transaction — PRAGMA foreign_keys
    // is silently ignored inside transactions.
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;

    // Use a RAII guard to guarantee FK re-enable even on panic/early return.
    let _fk_guard = FkGuard::new(&conn);

    let tx = conn.transaction()?;
    for op in ops {
        match op { /* ... */ }
    }
    tx.commit()?;

    // FkGuard::drop re-enables FK
    Ok(())
}

/// RAII guard: ensures `PRAGMA foreign_keys = ON` runs on scope exit.
struct FkGuard<'a> { conn: &'a Connection }
impl<'a> FkGuard<'a> {
    fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
impl<'a> Drop for FkGuard<'a> {
    fn drop(&mut self) {
        // Best-effort: if this fails, log but don't panic in drop.
        if let Err(e) = self.conn.execute_batch("PRAGMA foreign_keys = ON") {
            tracing::error!("failed to re-enable FK: {}", e);
        }
    }
}
```

**为什么 RAII guard**：保证 `?` early-return、panic、任何路径退出都会恢复 FK。不这么做等于留一颗定时炸弹 — 一次错误路径漏了 re-enable，后续所有 write 都绕过 FK 检查直到进程重启。

**为什么不用 `conn.pragma_update`**：rusqlite 的 `pragma_update` 对 `foreign_keys` 的处理没有比 `execute_batch` 更安全，且 `execute_batch` 在原代码中已经是约定。保持一致。

### 2. 修正错误分类（附带修复）

用 extended error code 区分 constraint 种类：

```rust
// storage/error.rs — 新增 variants
pub enum StorageError {
    ForeignKeyViolation { op, detail, source },
    UniqueConstraintViolation { op, detail, source },
    CheckConstraintViolation { op, detail, source },
    NotNullViolation { op, detail, source },
    ConstraintViolation { op, detail, source },  // fallback
    // ... existing variants
}

// storage/sqlite.rs — 用 extended code 分流
rusqlite::Error::SqliteFailure(e, _) => {
    use rusqlite::ffi;
    match e.extended_code {
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY => StorageError::ForeignKeyViolation { .. },
        ffi::SQLITE_CONSTRAINT_UNIQUE     => StorageError::UniqueConstraintViolation { .. },
        ffi::SQLITE_CONSTRAINT_CHECK      => StorageError::CheckConstraintViolation { .. },
        ffi::SQLITE_CONSTRAINT_NOTNULL    => StorageError::NotNullViolation { .. },
        _ if e.code == ErrorCode::ConstraintViolation
            => StorageError::ConstraintViolation { .. },
        _ => StorageError::Sqlite { .. },
    }
}
```

### 3. 翻转并更新那个 "BUG" 测试

`test_migration_batch_fk_disabled_bug` 当前断言 **bug 存在**。修复后要翻转：

```rust
#[test]
fn test_migration_batch_accepts_dangling_edges() {
    // GOAL-2.9: migration batch MUST accept dangling edges (as warnings).
    let s = temp_storage();
    let ops = vec![
        BatchOp::PutNode(Node::new("a", "A")),
        BatchOp::AddEdge(Edge::new("a", "nonexistent", "depends_on")),
    ];
    s.execute_migration_batch(&ops)
        .expect("migration batch should accept dangling edges (FK off)");
    assert_eq!(s.get_node_count().unwrap(), 1);
    assert_eq!(s.get_edge_count().unwrap(), 1);
}

#[test]
fn test_fk_reenabled_after_migration_batch() {
    // Regression: after migration batch commits, FK must be back ON.
    let s = temp_storage();
    s.execute_migration_batch(&[BatchOp::PutNode(Node::new("a", "A"))]).unwrap();

    // Normal write with dangling edge must now fail.
    let result = s.execute_batch(&[BatchOp::AddEdge(Edge::new("a", "ghost", "depends_on"))]);
    assert!(result.is_err(), "FK must be re-enabled after migration batch");
}

#[test]
fn test_fk_reenabled_after_migration_batch_error() {
    // Regression: if migration batch fails mid-flight, FK must still be re-enabled
    // (RAII guard guarantee).
    // ... setup that forces an error inside the batch ...
    // ... then verify normal FK-on behavior is restored ...
}
```

第三个测试是关键 — 覆盖 RAII guard 在 error path 上的语义。没有这个测试，guard 的正确性只是"相信"。

### 4. 清理 LSP Pass 1 的 dangling 产出（长期清理，非本 issue 阻塞项）

修复完 PRAGMA 后，dangling edges 会 correctly 落库为 warning。但真正干净的方案是让 Pass 1 就别产出 dangling：`find_closest_node` 失败 → 保留原 edge 不动，或降级到原始 tree-sitter 匹配结果，而不是写 `target=None`。这属于 LSP refinement 的独立 cleanup，另开 issue 跟进。

## 验收标准

1. ✅ `execute_migration_batch` 能接受 dangling edges 不报错
2. ✅ FK 在 batch 结束后重新开启（正常/错误/panic 三种路径都要）
3. ✅ 新增/修正测试全绿；老的 `test_migration_batch_fk_disabled_bug` 被翻转
4. ✅ Error 分类能区分 FK / UNIQUE / CHECK / NOTNULL
5. ✅ `cd /Users/potato/clawd/projects/gid-rs && cargo build --release` 通过
6. ✅ 在 RustClaw 项目上重跑 `gid extract --lsp`，transaction 不再 rollback，refinement 统计 > 0
7. ✅ `cargo test -p gid-core` 全绿

## 影响范围

**改动文件（预估）**:
- `crates/gid-core/src/storage/sqlite.rs` — `execute_migration_batch` 重写 + 错误映射
- `crates/gid-core/src/storage/error.rs` — 新增 constraint variant
- `crates/gid-core/src/storage/migration.rs` — 注释更新

**行为变更**:
- `gid extract --lsp` 真正 work（目前 silent failure）
- 错误信息更精确（FK vs UNIQUE vs CHECK）
- 现有 callers 若 pattern-match `ForeignKeyViolation` 需要补 match arms（或加 catch-all）

**回归风险**: 低。修复把现有被动接受的 bug 行为转成设计意图；所有测试配合更新。

## 关联

- `sqlite.rs:1956-1968` — 老的 bug 测试（将翻转）
- `migration.rs:506-523` — 注释已说明意图，实现未兑现
- `code_graph/build.rs:505-524` — Pass 1 dangling 来源
- GOAL-2.9 — migration batch 对 dangling edge 的语义要求
- ISS-004 (LSP client) — LSP refinement 链路上游
