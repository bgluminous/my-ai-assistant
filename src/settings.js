import { el, invoke, listen, getNumberUnit, setNumberUnit, fmtTokens, resetError, fillStatus } from "./shared.js";
import { getRefreshIntervalMinutes, setRefreshInterval, refreshAccounts } from "./accounts.js";

// 设置弹窗：
// - Token 数量单位（完整 / K·M·B / 万·亿），存 localStorage，切换时通过 unitchange 事件
//   通知已渲染的视图即时重绘；
// - 定时刷新间隔（账户状态与用量统计共用），实际存取与定时器由 accounts.js 托管，写入统一 settings.json；
// - 开机启动：自启动开关注册在系统（后端 autostart_get / set），静默启动偏好存 settings.json；
// - Cursor / ChatGPT / Claude Desktop 客户端路径，由后端持久化到 settings.json
//   （*_client_get / set / detect），用于切换账户后启动对应客户端；
// - 数据备份：全量导出 / 导入（backup_export / backup_import_*），文件含全部设置与账号，
//   以及已删除 Cursor 账户保留的统计数据，可选密码加密；界面偏好（主题、数字单位）存
//   localStorage，由本模块随备份收集与应用。

const SAMPLE = 1234567890;

/** 弹窗内某一区块的状态条：set(kind, text) 填充并显示，clear() 隐藏并清空。 */
function statusBox(selector) {
  return {
    set: (kind, text) => fillStatus(el(selector), kind, text),
    clear: () => {
      const box = el(selector);
      box.hidden = true;
      box.textContent = "";
    },
  };
}
const intervalStatus = statusBox("#interval-status");
const autostartStatus = statusBox("#autostart-status");
const backupStatus = statusBox("#backup-status");
const cursorPathStatus = statusBox("#cursor-path-status");
const codexPathStatus = statusBox("#codex-path-status");
const claudePathStatus = statusBox("#claude-path-status");

function renderUnitSeg() {
  const current = getNumberUnit();
  for (const seg of el("#unit-mode").querySelectorAll(".seg")) {
    seg.classList.toggle("active", seg.dataset.unit === current);
  }
  el("#unit-sample").textContent = `1,234,567,890 → ${fmtTokens(SAMPLE)}`;
}

/* ---------- 定时刷新（账户状态 + 用量统计） ---------- */

async function onIntervalChange() {
  const select = el("#accounts-interval");
  const previous = getRefreshIntervalMinutes();
  try {
    const minutes = await setRefreshInterval(Number(select.value));
    intervalStatus.set("ok", minutes > 0 ? `已开启定时刷新，每 ${minutes} 分钟自动刷新账户状态与用量统计。` : "已关闭定时刷新。");
  } catch (error) {
    select.value = String(previous);
    intervalStatus.set("bad", `设置定时刷新失败：${resetError(error)}`);
  }
}

/* ---------- 开机启动 ---------- */

function reflectAutostart(enabled, silent) {
  el("#autostart-enabled").checked = enabled;
  el("#autostart-silent").checked = silent;
  // 静默启动只在自启动开启时有意义
  el("#autostart-silent").disabled = !enabled;
}

async function loadAutostart() {
  try {
    const v = await invoke("autostart_get");
    reflectAutostart(!!(v && v.enabled), !!(v && v.silent));
  } catch {
    // 非 Tauri 环境读取失败，静默即可（仅影响回显）
  }
}

async function onAutostartChange() {
  const enabled = el("#autostart-enabled").checked;
  const silent = el("#autostart-silent").checked;
  try {
    const v = await invoke("autostart_set", { enabled, silent });
    reflectAutostart(!!v.enabled, !!v.silent);
    autostartStatus.set(
      "ok",
      v.enabled
        ? v.silent
          ? "已开启开机自启动，开机时将静默启动到托盘。"
          : "已开启开机自启动。"
        : "已关闭开机自启动。"
    );
  } catch (error) {
    // 写系统失败时回读真实状态，避免界面与系统不一致
    void loadAutostart();
    autostartStatus.set("bad", `设置开机启动失败：${resetError(error)}`);
  }
}

/* ---------- 数据备份（全量导出 / 导入） ---------- */

let backupBusy = false;
let backupPasswordMode = null; // null | "export" | "import"
let backupImportPath = ""; // 待解密导入的备份文件路径

function updateBackupButtons() {
  el("#backup-export").disabled = backupBusy;
  el("#backup-import").disabled = backupBusy;
  el("#backup-password-ok").disabled = backupBusy;
}

function hidePasswordRow() {
  backupPasswordMode = null;
  backupImportPath = "";
  el("#backup-password").value = "";
  el("#backup-password-row").hidden = true;
}

/** 显示密码行：导出 = 可选设置密码；导入 = 必填解密密码。 */
function showPasswordRow(mode, path) {
  backupPasswordMode = mode;
  backupImportPath = path || "";
  const input = el("#backup-password");
  input.value = "";
  input.placeholder = mode === "export" ? "可选：设置加密密码，留空则明文导出" : "该备份已加密，请输入密码";
  el("#backup-password-ok").textContent = mode === "export" ? "开始导出" : "解密导入";
  el("#backup-password-row").hidden = false;
  input.focus();
}

/** 收集存于 localStorage 的界面偏好，随备份导出。 */
function collectUiPrefs() {
  let theme = "dark";
  try {
    if (localStorage.getItem("theme") === "light") theme = "light";
  } catch { /* ignore */ }
  return { theme, numberUnit: getNumberUnit() };
}

/** 应用备份携带的界面偏好：主题即时切换，数字单位广播 unitchange 重绘各视图。 */
function applyUiPrefs(prefs) {
  if (!prefs || typeof prefs !== "object") return;
  if (prefs.theme === "light" || prefs.theme === "dark") {
    document.documentElement.dataset.theme = prefs.theme;
    try { localStorage.setItem("theme", prefs.theme); } catch { /* ignore */ }
    window.dispatchEvent(new CustomEvent("themechange", { detail: { theme: prefs.theme } }));
  }
  if (prefs.numberUnit === "full" || prefs.numberUnit === "en" || prefs.numberUnit === "zh") {
    setNumberUnit(prefs.numberUnit);
    renderUnitSeg();
  }
}

async function doExport(password) {
  backupBusy = true;
  updateBackupButtons();
  backupStatus.set("", "正在导出全部数据…");
  try {
    const r = await invoke("backup_export", { password: password || null, uiPrefs: collectUiPrefs() });
    if (r && r.cancelled) {
      backupStatus.clear();
      return;
    }
    backupStatus.set("ok", `已导出全部数据${r.encrypted ? "（已加密）" : ""}：${r.path}`);
  } catch (error) {
    backupStatus.set("bad", `导出失败：${resetError(error)}`);
  } finally {
    backupBusy = false;
    updateBackupButtons();
  }
}

async function doImportApply(path, password) {
  backupBusy = true;
  updateBackupButtons();
  backupStatus.set("", "正在导入并应用数据…");
  try {
    const r = await invoke("backup_import_apply", { path, password: password || null });
    hidePasswordRow();
    applyUiPrefs(r && r.uiPrefs);
    // 设置弹窗内的回显同步导入结果（间隔选择框由 accounts-changed 事件自动更新）
    void loadAutostart();
    void loadClientPaths();
    const parts = [`新增 ${Number(r && r.imported) || 0} 个账号`];
    const exists = Number(r && r.skippedExists) || 0;
    const invalid = Number(r && r.skippedInvalid) || 0;
    const usageRestored = Number(r && r.usageRestored) || 0;
    if (exists > 0) parts.push(`跳过 ${exists} 个已存在`);
    if (invalid > 0) parts.push(`${invalid} 个无效`);
    if (usageRestored > 0) parts.push(`恢复 ${usageRestored} 个已删除账户的统计数据`);
    backupStatus.set("ok", `导入完成：${parts.join("，")}；其余设置已应用。`);
    // 新导入的账号后台排队刷新验证，与账户页导入体验一致
    const ids = Array.isArray(r && r.importedIds) ? r.importedIds : [];
    if (ids.length) void refreshAccounts(ids);
  } catch (error) {
    const msg = resetError(error);
    if (msg === "wrong_password") {
      backupStatus.set("bad", "密码错误，请重试。");
    } else if (msg === "invalid_format") {
      hidePasswordRow();
      backupStatus.set("bad", "不是有效的备份文件。");
    } else {
      backupStatus.set("bad", `导入失败：${msg}`);
    }
  } finally {
    backupBusy = false;
    updateBackupButtons();
  }
}

function onBackupExportClick() {
  if (backupBusy) return;
  backupStatus.clear();
  showPasswordRow("export", "");
}

async function onBackupImportClick() {
  if (backupBusy) return;
  backupStatus.clear();
  hidePasswordRow();
  backupBusy = true;
  updateBackupButtons();
  let picked = null;
  try {
    const r = await invoke("backup_import_inspect");
    if (r && !r.cancelled) picked = r;
  } catch (error) {
    const msg = resetError(error);
    backupStatus.set("bad", msg === "invalid_format" ? "不是有效的备份文件。" : `导入失败：${msg}`);
  } finally {
    backupBusy = false;
    updateBackupButtons();
  }
  if (!picked) return;
  if (picked.encrypted) {
    showPasswordRow("import", picked.path);
  } else {
    await doImportApply(picked.path, null);
  }
}

function onBackupPasswordOk() {
  if (backupBusy) return;
  const password = el("#backup-password").value.trim();
  if (backupPasswordMode === "export") {
    hidePasswordRow();
    void doExport(password || null);
    return;
  }
  if (backupPasswordMode === "import") {
    if (!password) {
      backupStatus.set("bad", "请输入备份密码。");
      return;
    }
    // 密码错误时保留输入行以便重试（doImportApply 仅在成功或文件无效时收起）
    void doImportApply(backupImportPath, password);
  }
}

/* ---------- 客户端路径（Cursor / ChatGPT / Claude Desktop） ---------- */

// 三个客户端路径输入框与后端配置命令的对应；name / detect 仅 ChatGPT 与 Claude Desktop 有
//（它们走一键探测，Cursor 走下面的逐个扫描）
const CLIENT_PATHS = [
  {
    input: "#cursor-exe-path",
    get: "cursor_client_get",
    set: "cursor_client_set",
    invalid: "cursor_exe_invalid",
    status: cursorPathStatus,
  },
  {
    input: "#codex-exe-path",
    get: "codex_client_get",
    set: "codex_client_set",
    invalid: "codex_exe_invalid",
    status: codexPathStatus,
    name: "ChatGPT",
    detectBtn: "#codex-detect",
    detect: "codex_client_detect",
  },
  {
    input: "#claude-exe-path",
    get: "claude_client_get",
    set: "claude_client_set",
    invalid: "claude_exe_invalid",
    status: claudePathStatus,
    name: "Claude Desktop",
    detectBtn: "#claude-detect",
    detect: "claude_client_detect",
  },
];

/** 回显三个客户端已保存的路径；非 Tauri 环境读取失败静默（仅影响回显）。 */
async function loadClientPaths() {
  for (const c of CLIENT_PATHS) {
    try {
      const v = await invoke(c.get);
      el(c.input).value = (v && v.exePath) || "";
    } catch {
      // 浏览器直开等非 Tauri 环境静默
    }
  }
}

/** 一键探测常见安装位置，命中即填入输入框（保存仍需点「完成」）。 */
async function onDetect(c) {
  const btn = el(c.detectBtn);
  btn.disabled = true;
  c.status.set("", `正在搜索本机 ${c.name}…`);
  try {
    const r = await invoke(c.detect);
    if (r && r.exePath) {
      el(c.input).value = r.exePath;
      c.status.set("ok", `已填入：${r.exePath}。点「完成」保存。`);
    } else {
      c.status.set("warn", `未找到 ${c.name}，请手动填写完整路径。`);
    }
  } catch (error) {
    c.status.set("bad", `搜索失败：${resetError(error)}`);
  } finally {
    btn.disabled = false;
  }
}

/* ---------- Cursor 路径逐个扫描 ---------- */

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
  cursorPathStatus.set("", restart ? "正在扫描本机 Cursor…" : "继续搜索下一个…");
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
        cursorPathStatus.set("ok", `已填入（这是最后一个）：${r.found}。点「完成」保存。`);
      } else {
        sessionActive = true;
        cursorPathStatus.set("ok", `已填入：${r.found}。点「完成」保存，或「搜索下一个」继续。`);
      }
    } else if (r && r.cancelled) {
      sessionActive = false;
      cursorPathStatus.set("warn", "已取消扫描。");
    } else {
      sessionActive = false;
      cursorPathStatus.set("warn", restart ? "未找到 Cursor，请手动填写完整路径。" : "已无更多结果。");
    }
  } catch (error) {
    sessionActive = false;
    cursorPathStatus.set("bad", `扫描失败：${resetError(error)}`);
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

// 点「完成」：保存 Cursor / ChatGPT / Claude Desktop 路径并关闭；路径无效则提示且不关闭。空路径 = 清除配置。
async function onDone() {
  const modal = el("#settings-modal");
  if (scanning) void onScanCancel();
  for (const c of CLIENT_PATHS) {
    try {
      await invoke(c.set, { exePath: el(c.input).value.trim() });
    } catch (error) {
      if (resetError(error) === c.invalid) {
        c.status.set("bad", "路径无效或文件不存在，请修正或清空后再完成。");
        return;
      }
    }
  }
  modal.hidden = true;
}

/* ---------- 初始化 ---------- */

export function initSettings() {
  const modal = el("#settings-modal");
  el("#settings-btn").addEventListener("click", () => {
    renderUnitSeg();
    intervalStatus.clear();
    el("#accounts-interval").value = String(getRefreshIntervalMinutes());
    autostartStatus.clear();
    backupStatus.clear();
    hidePasswordRow();
    updateBackupButtons();
    for (const c of CLIENT_PATHS) c.status.clear();
    setScanProgress("");
    sessionActive = false;
    updateScanButton();
    el("#cursor-scan-cancel").hidden = true;
    modal.hidden = false;
    void loadAutostart();
    void loadClientPaths();
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
  el("#autostart-enabled").addEventListener("change", () => { void onAutostartChange(); });
  el("#autostart-silent").addEventListener("change", () => { void onAutostartChange(); });
  el("#backup-export").addEventListener("click", onBackupExportClick);
  el("#backup-import").addEventListener("click", () => { void onBackupImportClick(); });
  el("#backup-password-ok").addEventListener("click", onBackupPasswordOk);
  el("#backup-password-cancel").addEventListener("click", () => {
    hidePasswordRow();
    backupStatus.clear();
  });
  el("#backup-password").addEventListener("keydown", (e) => {
    if (e.key === "Enter") onBackupPasswordOk();
  });
  el("#cursor-detect").addEventListener("click", () => { void onStep(); });
  el("#cursor-scan-cancel").addEventListener("click", () => { void onScanCancel(); });
  for (const c of CLIENT_PATHS) {
    if (c.detectBtn) el(c.detectBtn).addEventListener("click", () => { void onDetect(c); });
  }
  el("#settings-done").addEventListener("click", () => { void onDone(); });
}
