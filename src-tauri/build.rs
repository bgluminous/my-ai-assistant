fn main() -> tauri_build::Result<()> {
    println!("cargo:rerun-if-env-changed=STATIC_VCRUNTIME");
    let mut attributes = tauri_build::Attributes::new();
    // 旧版 Tauri CLI 会自动注入此变量；转换为新 API，同时保留显式 false 的语义。
    if let Some(value) = std::env::var_os("STATIC_VCRUNTIME") {
        // 在调用构建库、启动其它线程之前移除，仅影响当前构建脚本进程。
        std::env::remove_var("STATIC_VCRUNTIME");
        attributes = attributes.windows_attributes(
            tauri_build::WindowsAttributes::new().static_vc_runtime(value != "false"),
        );
    }
    tauri_build::try_build(attributes)
}
