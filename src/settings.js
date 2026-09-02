import { el, invoke, listen, getNumberUnit, setNumberUnit, fmtTokens, resetError, fillStatus } from "./shared.js";
import { getRefreshIntervalMinutes, setRefreshInterval } from "./accounts.js";

// 设置弹窗：
// - Token 数量单位（完整 / K·M·B / 万·亿），存 localStorage，切换时通过 unitchange 事件
//   通知已渲染的视图即时重绘；
// - 定时刷新间隔（账户状态与用量统计共用），实际存取与定时器由 accounts.js 托管，写入统一 settings.json；
// - ChatGPT 客户端路径，由后端持久化到 settings.json（codex_client_get / set / detect），用于切换账户后启动 ChatGPT。

const SAMPLE = 1234567890;

function renderUnitSeg() {
  const current = getNumberUnit();
  for (const seg of el("#unit-mode").querySelectorAll(".seg")) {
    seg.classList.toggle("active", seg.dataset.unit === current);
  }
  el("#unit-sample").textContent = `1,234,567,890 → ${fmtTokens(SAMPLE)}`;
}

/* ---------- 定时刷新（账户状态 + 用量统计） ---------- */

function setIntervalStatus(kind, text) {
  fillStatus(el("#interval-status"), kind, text);
}

function clearIntervalStatus() {
  const box = el("#interval-status");
  box.hidden = true;
  box.textContent = "";
}

async function onIntervalChange() {
  const select = el("#accounts-interval");
  const previous = getRefreshIntervalMinutes();
  try {
    const minutes = await setRefreshInterval(Number(select.value));
    setIntervalStatus("ok", minutes > 0 ? `已开启定时刷新，每 ${minutes} 分钟自动刷新账户状态与用量统计。` : "已关闭定时刷新。");
  } catch (error) {
    select.value = String(previous);
    setIntervalStatus("bad", `设置定时刷新失败：${resetError(error)}`);
  }
}

/* ---------- Cursor 客户端路径 ---------- */

function setCursorPathStatus(kind, text) {
  fillStatus(el("#cursor-path-status"), kind, text);
}

function clearCursorPathStatus() {
  const box = el("#cursor-path-status");
  box.hidden = true;
  box.textContent = "";
}

async function loadCursorPath() {
  try {
    const v = await invoke("cursor_client_get");
    el("#cursor-exe-path").value = (v && v.exePath) || "";
  } catch {
    // 浏览器直开等非 Tauri 环境下读取失败，静默即可（仅影响回显）
  }
}

function setCodexPathStatus(kind, text) {
  fillStatus(el("#codex-path-status"), kind, text);
}

function clearCodexPathStatus() {
  const box = el("#codex-path-status");
  box.hidden = true;
  box.textContent = "";
}

async function loadCodexPath() {
  try {
    const v = await invoke("codex_client_get");
    el("#codex-exe-path").value = (v && v.exePath) || "";
  } catch {
    // 非 Tauri 环境静默
  }
}

async function onCodexDetect() {
  const btn = el("#codex-detect");
  btn.disabled = true;
  setCodexPathStatus("", "正在搜索本机 ChatGPT…");
  try {
    const r = await invoke("codex_client_detect");
    if (r && r.exePath) {
      el("#codex-exe-path").value = r.exePath;
      setCodexPathStatus("ok", `已填入：${r.exePath}。点「完成」保存。`);
    } else {
      setCodexPathStatus("warn", "未找到 ChatGPT，请手动填写完整路径。");
    }
  } catch (error) {
    setCodexPathStatus("bad", `搜索失败：${resetError(error)}`);
  } finally {
    btn.disabled = false;
  }
}

let scanning = false;
let sessionActive = false; // 是否有可继续的扫描会话（上次命中且未扫完）
let unlistenScan = null;

function setScanProgress(text) {
  const box = el("#cursor-scan-progress");
  if (!text) {
    box.hidden = true;
    box.textContent = "";
    return;
  }
  box.hidden = false;
  box.textContent = text;
}

function updateScanButton() {
  el("#cursor-detect").textContent = sessionActive ? "搜索下一个" : "自动搜索";
}

// 逐个查找：命中一个即暂停并填入输入框，再点「搜索下一个」从原处继续，直到扫完。
async function onStep() {
  if (scanning) return;
  const restart = !sessionActive;
  scanning = true;
  el("#cursor-detect").disabled = true;
  el("#cursor-scan-cancel").hidden = false;
  if (restart) {
    setCursorPathStatus("", "正在扫描本机 Cursor…");
  } else {
    setCursorPathStatus("", "继续搜索下一个…");
  }
  setScanProgress("扫描中…");
  try {
    // 先挂上进度监听，再触发扫描，避免漏掉早期事件。
    unlistenScan = await listen("cursor-scan-progress", (e) => {
      const p = (e && e.payload) || {};
      const cur = p.current ? `：${p.current}` : "";
      setScanProgress(`已扫描 ${p.scanned || 0} 个目录（第 ${p.depth || 0} 层）${cur}`);
    });
    const r = await invoke("cursor_client_scan_step", { restart });
    if (r && r.found) {
      el("#cursor-exe-path").value = r.found;
      if (r.done) {
        sessionActive = false;
        setCursorPathStatus("ok", `已填入（这是最后一个）：${r.found}。点「完成」保存。`);
      } else {
        sessionActive = true;
        setCursorPathStatus("ok", `已填入：${r.found}。点「完成」保存，或「搜索下一个」继续。`);
      }
    } else if (r && r.cancelled) {
      sessionActive = false;
      setCursorPathStatus("warn", "已取消扫描。");
    } else {
      sessionActive = false;
      setCursorPathStatus("warn", restart ? "未找到 Cursor，请手动填写完整路径。" : "已无更多结果。");
    }
  } catch (error) {
    sessionActive = false;
    setCursorPathStatus("bad", `扫描失败：${resetError(error)}`);
  } finally {
    if (unlistenScan) {
      try { unlistenScan(); } catch { /* ignore */ }
      unlistenScan = null;
    }
    setScanProgress("");
    el("#cursor-scan-cancel").hidden = true;
    el("#cursor-detect").disabled = false;
    updateScanButton();
    scanning = false;
  }
}

async function onScanCancel() {
  setScanProgress("正在取消…");
  try { await invoke("cursor_client_scan_cancel"); } catch { /* ignore */ }
  sessionActive = false;
}

// 点「完成」：保存 Cursor / ChatGPT 路径并关闭；路径无效则提示且不关闭。空路径 = 清除配置。
async function onDone() {
  const modal = el("#settings-modal");
  if (scanning) void onScanCancel();
  const path = el("#cursor-exe-path").value.trim();
  try {
    await invoke("cursor_client_set", { exePath: path });
  } catch (error) {
    if (resetError(error) === "cursor_exe_invalid") {
      setCursorPathStatus("bad", "路径无效或文件不存在，请修正或清空后再完成。");
      return;
    }
  }
  const codexPath = el("#codex-exe-path").value.trim();
  try {
    await invoke("codex_client_set", { exePath: codexPath });
  } catch (error) {
    if (resetError(error) === "codex_exe_invalid") {
      setCodexPathStatus("bad", "路径无效或文件不存在，请修正或清空后再完成。");
      return;
    }
  }
  modal.hidden = true;
}

/* ---------- 初始化 ---------- */

export function initSettings() {
  const modal = el("#settings-modal");
  el("#settings-btn").addEventListener("click", () => {
    renderUnitSeg();
    clearIntervalStatus();
    el("#accounts-interval").value = String(getRefreshIntervalMinutes());
    clearCursorPathStatus();
    clearCodexPathStatus();
    setScanProgress("");
    sessionActive = false;
    updateScanButton();
    el("#cursor-scan-cancel").hidden = true;
    modal.hidden = false;
    void loadCursorPath();
    void loadCodexPath();
  });
  for (const node of modal.querySelectorAll("[data-close]")) {
    node.addEventListener("click", () => {
      if (scanning) void onScanCancel();
      modal.hidden = true;
    });
  }
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) {
      if (scanning) void onScanCancel();
      modal.hidden = true;
    }
  });
  for (const seg of el("#unit-mode").querySelectorAll(".seg")) {
    seg.addEventListener("click", () => {
      setNumberUnit(seg.dataset.unit);
      renderUnitSeg();
    });
  }
  el("#accounts-interval").addEventListener("change", () => { void onIntervalChange(); });
  el("#cursor-detect").addEventListener("click", () => { void onStep(); });
  el("#cursor-scan-cancel").addEventListener("click", () => { void onScanCancel(); });
  el("#codex-detect").addEventListener("click", () => { void onCodexDetect(); });
  el("#settings-done").addEventListener("click", () => { void onDone(); });
}
