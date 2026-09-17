//! 用户目录解析，语义对齐 dirs 5（home / config / data_local），避免引入 dirs crate。

use std::path::PathBuf;

/// 读取环境变量为非空路径（空白视为未设置）。
pub fn env_nonempty(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// 用户主目录。Windows：`USERPROFILE` 或 `HOMEDRIVE`+`HOMEPATH`；其它：`HOME`。
pub fn home_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        if let Some(p) = env_nonempty("USERPROFILE") {
            return Some(p);
        }
        let drive = std::env::var_os("HOMEDRIVE")?;
        let path = std::env::var_os("HOMEPATH")?;
        let mut s = drive;
        s.push(path);
        let p = PathBuf::from(s);
        if p.as_os_str().is_empty() {
            None
        } else {
            Some(p)
        }
    } else {
        env_nonempty("HOME")
    }
}

/// 用户配置目录。Windows：`APPDATA`；macOS：`~/Library/Application Support`；
/// 其它：`XDG_CONFIG_HOME` 或 `~/.config`。
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        env_nonempty("APPDATA")
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    } else {
        env_nonempty("XDG_CONFIG_HOME").or_else(|| home_dir().map(|h| h.join(".config")))
    }
}

/// 本应用用户数据目录：`{home_dir}/.xilore/myaiassistant`。
/// 设置 `MYAI_ASSISTANT_HOME` 时改用该目录（独立数据，不读写默认用户目录）。
pub fn app_dir() -> Option<PathBuf> {
    if let Some(p) = env_nonempty("MYAI_ASSISTANT_HOME") {
        return Some(p);
    }
    home_dir().map(|h| h.join(".xilore").join("myaiassistant"))
}

// ---------------------------------------------------------------------------
// 【过渡期临时代码，到期删除】旧用户目录迁移
//
// 用户目录曾为 `{home_dir}/xilore/myaiassistant`（无点号）。下面的 legacy_app_dir /
// migrate_legacy_app_dir / copy_dir_recursive 只负责在启动时把旧目录整体搬到 app_dir，
// 属于一次性迁移逻辑：确认使用中的机器都已完成迁移后（例如下一个大版本），把这一段连同
// lib.rs 里的调用、audit.js 的 data_dir_migrated / data_dir_migrate_failed 事件映射一起删除，
// 不要长期保留。
// ---------------------------------------------------------------------------

/// 旧版用户数据目录 `{home_dir}/xilore/myaiassistant`，仅供迁移使用。
fn legacy_app_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("xilore").join("myaiassistant"))
}

/// 迁移结果，供调用方写审计日志。
pub struct LegacyMigration {
    pub from: PathBuf,
    pub to: PathBuf,
    /// "rename"（整体改名）或 "copy"（改名失败后逐文件复制并删除旧目录）。
    pub method: &'static str,
    /// 复制方式下旧目录删除失败时的错误（新目录已完整，仅提示残留）。
    pub leftover_error: Option<String>,
}

/// 启动时把旧目录迁到 app_dir。必须在任何读写 app_dir 的动作（settings::load、audit::log）之前调用。
/// - 旧目录不存在，或新目录已存在（两者并存时以新目录为准、旧目录不动）：不迁移，返回 Ok(None)；
/// - 先整体 rename；失败则复制到同级临时目录 `myaiassistant.migrating`，完整后再改名到位，
///   最后删除旧目录——中途崩溃只会留下临时目录，下次启动清掉重来，不会出现半份新目录；
/// - 旧目录搬走后其父目录 `xilore` 若已空则一并删除。
pub fn migrate_legacy_app_dir() -> Result<Option<LegacyMigration>, String> {
    if env_nonempty("MYAI_ASSISTANT_HOME").is_some() {
        return Ok(None);
    }
    let (Some(from), Some(to)) = (legacy_app_dir(), app_dir()) else {
        return Ok(None);
    };
    if !from.is_dir() || to.exists() {
        return Ok(None);
    }
    let parent = to
        .parent()
        .ok_or_else(|| "invalid_app_dir".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_failed: {e}"))?;

    let mut method = "rename";
    let mut leftover_error = None;
    if std::fs::rename(&from, &to).is_err() {
        method = "copy";
        let staging = parent.join("myaiassistant.migrating");
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|e| format!("cleanup_staging_failed: {e}"))?;
        }
        copy_dir_recursive(&from, &staging).map_err(|e| format!("copy_failed: {e}"))?;
        std::fs::rename(&staging, &to).map_err(|e| format!("finalize_failed: {e}"))?;
        if let Err(e) = std::fs::remove_dir_all(&from) {
            leftover_error = Some(e.to_string());
        }
    }
    // 旧父目录 `xilore` 只在已空时删除（remove_dir 对非空目录直接失败，忽略即可）
    if let Some(old_parent) = from.parent() {
        let _ = std::fs::remove_dir(old_parent);
    }
    Ok(Some(LegacyMigration {
        from,
        to,
        method,
        leftover_error,
    }))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// 每用户本地数据目录。Windows：`LOCALAPPDATA`；macOS 与 `config_dir` 相同；
/// 其它：`XDG_DATA_HOME` 或 `~/.local/share`。
pub fn data_local_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        env_nonempty("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        config_dir()
    } else {
        env_nonempty("XDG_DATA_HOME").or_else(|| home_dir().map(|h| h.join(".local").join("share")))
    }
}
