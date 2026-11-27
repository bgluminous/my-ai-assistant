fn main() -> tauri_build::Result<()> {
    // Embed the same JSON packs served to the WebViews, including in the portable executable.
    let locales = std::path::Path::new("../src/locales");
    println!("cargo:rerun-if-changed={}", locales.display());
    let mut packs = Vec::new();
    for entry in std::fs::read_dir(locales)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") && path.file_name().unwrap() != "index.json" {
            packs.push(format!("include_str!({:?})", path.canonicalize()?));
        }
    }
    packs.sort();
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("language_packs.rs");
    std::fs::write(output, format!("&[{}]", packs.join(",\n")))?;
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
