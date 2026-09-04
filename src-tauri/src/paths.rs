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

/// 本应用用户数据目录：`{home_dir}/xilore/myaiassistant`。
pub fn app_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("xilore").join("myaiassistant"))
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
