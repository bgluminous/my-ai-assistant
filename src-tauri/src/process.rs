//! 本机进程列举，替代 sysinfo：只要 pid / 进程名 / 可执行路径。

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub exe: Option<PathBuf>,
}

/// 当前可见进程。Windows 走 Toolhelp + 映像路径；macOS 解析 `ps`；其它平台空列表。
pub fn list_processes() -> Vec<ProcessInfo> {
    if cfg!(target_os = "windows") {
        list_windows()
    } else if cfg!(target_os = "macos") {
        list_macos()
    } else {
        Vec::new()
    }
}

#[cfg(windows)]
fn list_windows() -> Vec<ProcessInfo> {
    windows_toolhelp()
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn list_windows() -> Vec<ProcessInfo> {
    Vec::new()
}

#[cfg(windows)]
fn windows_toolhelp() -> Vec<ProcessInfo> {
    use std::os::windows::ffi::OsStringExt;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const INVALID_HANDLE: isize = -1;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(
            process: isize,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        std::ffi::OsString::from_wide(&buf[..end])
            .to_string_lossy()
            .into_owned()
    }

    fn image_path(pid: u32) -> Option<PathBuf> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle == 0 || handle == INVALID_HANDLE {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
            CloseHandle(handle);
            if ok == 0 || size == 0 {
                return None;
            }
            Some(PathBuf::from(wide_to_string(&buf[..size as usize])))
        }
    }

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == 0 || snap == INVALID_HANDLE {
            return Vec::new();
        }
        let mut entry = std::mem::zeroed::<ProcessEntry32W>();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        let mut out = Vec::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let pid = entry.th32_process_id;
                if pid != 0 {
                    out.push(ProcessInfo {
                        pid,
                        name: wide_to_string(&entry.sz_exe_file),
                        exe: image_path(pid),
                    });
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        out
    }
}

fn list_macos() -> Vec<ProcessInfo> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((pid_s, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_s.parse::<u32>() else {
            continue;
        };
        let command = rest.trim();
        if command.is_empty() {
            continue;
        }
        let name = std::path::Path::new(command)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| command.to_string());
        out.push(ProcessInfo {
            pid,
            name,
            exe: Some(PathBuf::from(command)),
        });
    }
    out
}
