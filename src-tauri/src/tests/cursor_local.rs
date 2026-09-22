//! cursor_local 模块的单元测试（由 cursor_local.rs 以 `#[path]` 挂载为 `crate::cursor_local::tests`）。

use super::*;

/// 临时目录下的一份最小认证库（ItemTable 结构与 Cursor 一致），测试结束时连同备份一起删除。
struct TempDb {
    dir: PathBuf,
    db: PathBuf,
}

impl TempDb {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "my-ai-assistant-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable(key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);")
            .unwrap();
        TempDb { dir, db }
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn clear_auth_removes_login_keys_and_keeps_others() {
    let tmp = TempDb::new("clear-auth");
    {
        let conn = tmp.conn();
        upsert_item(&conn, "cursorAuth/accessToken", "jwt").unwrap();
        upsert_item(&conn, "cursorAuth/refreshToken", "rt").unwrap();
        upsert_item(&conn, "cursor.email", "alice@example.com").unwrap();
        upsert_item(&conn, "cursorAuth/cachedTeam", "{}").unwrap();
        // 与登录无关的用户设置必须保留
        upsert_item(&conn, "workbench.theme", "dark").unwrap();
    }

    assert!(clear_auth(&tmp.db).unwrap(), "有 accessToken 应判定为已登录");

    let conn = tmp.conn();
    for key in AUTH_ITEM_KEYS {
        assert!(read_item(&conn, key).is_none(), "{key} 应已删除");
    }
    assert_eq!(read_item(&conn, "workbench.theme").as_deref(), Some("dark"));
    assert!(tmp.db.with_extension("vscdb.bak").exists(), "改库前应有整库备份");

    // 再清一次：已无登录态，返回 false 且不报错
    assert!(!clear_auth(&tmp.db).unwrap());
}

#[test]
fn clear_auth_without_db_is_not_logged_in() {
    let missing = std::env::temp_dir().join("my-ai-assistant-test-missing-state.vscdb");
    let _ = std::fs::remove_file(&missing);
    assert!(!clear_auth(&missing).unwrap());
    assert!(!missing.exists(), "不存在的认证库不得被创建");
}
