//! usage_archive 模块的单元测试（由 usage_archive.rs 以 `#[path]` 挂载为 `crate::usage_archive::tests`）。

use super::*;

fn meta(note: &str, note_auto: bool, email: Option<&str>, name: Option<&str>) -> DeletedMeta {
    DeletedMeta {
        deleted_at: 0,
        note: note.to_string(),
        note_auto,
        email: email.map(str::to_string),
        name: name.map(str::to_string),
        membership_type: None,
    }
}

#[test]
fn deleted_note_manual_overrides_identity_and_blank_restores_auto() {
    let identity = Some("cursor:user_01ABC");
    let mut m = meta("alice@example.com", true, Some("alice@example.com"), Some("Alice"));
    assert_eq!(deleted_label(&m), "Alice");

    // 手填备注：显示时优先于用户名 / 邮箱，首尾空白去掉
    apply_deleted_note(&mut m, identity, "  工作号  ");
    assert!(!m.note_auto);
    assert_eq!(m.note, "工作号");
    assert_eq!(deleted_label(&m), "工作号");

    // 留空恢复自动备注：备注回到邮箱，显示回到用户名
    apply_deleted_note(&mut m, identity, "   ");
    assert!(m.note_auto);
    assert_eq!(m.note, "alice@example.com");
    assert_eq!(deleted_label(&m), "Alice");

    // 没有邮箱时自动备注取身份里的账号 ID；连身份也没有则为空，显示「未命名账户」
    let mut bare = meta("旧备注", false, None, None);
    apply_deleted_note(&mut bare, identity, "");
    assert!(bare.note_auto);
    assert_eq!(bare.note, "user_01ABC");
    assert_eq!(deleted_label(&bare), "user_01ABC");
    apply_deleted_note(&mut bare, None, "");
    assert_eq!(bare.note, "");
    assert_eq!(deleted_label(&bare), "未命名账户");
}

#[test]
fn membership_choice_normalizes_and_rejects_unknown() {
    // 登记的档位：去空白、统一小写
    assert_eq!(normalize_membership_choice(" Pro "), Ok(Some("pro".to_string())));
    assert_eq!(normalize_membership_choice("PRO_PLUS"), Ok(Some("pro_plus".to_string())));
    // 留空 = 清除为未知
    assert_eq!(normalize_membership_choice("   "), Ok(None));
    // 未登记的值不接受
    assert_eq!(normalize_membership_choice("platinum"), Err("invalid_membership".to_string()));
}
