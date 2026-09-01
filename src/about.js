import { el, invoke } from "./shared.js";

// 关于页：填入由 Cargo.toml 提供的版本与作者；浏览器直开时保持「—」。

export function initAbout() {
  void loadInfo();
}

async function loadInfo() {
  try {
    const info = await invoke("app_info");
    if (info && info.version) el("#about-version").textContent = info.version;
    if (info && info.authors) el("#about-authors").textContent = info.authors;
  } catch {
    // 非 Tauri 环境静默
  }
}
