import { bindText, errorCode, languagePreference, setLanguage, translate } from "./i18n.js";
import { createModal } from "./modal.js";
import { el, invoke, listen, getNumberUnit, setNumberUnit, fmtTokens, resetError, fillStatus } from "./shared.js";
import { getRefreshIntervalMinutes, setRefreshInterval, refreshAccounts } from "./accounts.js";
import { initSyncSettings, loadSyncSettings, saveSyncSettings } from "./usage_sync.js";

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

/* ---------- 设置分类 ---------- */

function selectSettingsSection(section, focus = false) {
  const modal = el("#settings-modal");
  const tabs = [...modal.querySelectorAll("[data-settings-section]")];
  const selected = tabs.find((tab) => tab.dataset.settingsSection === section);
  if (!selected) return;
  for (const tab of tabs) {
    const active = tab === selected;
    tab.setAttribute("aria-selected", String(active));
    tab.tabIndex = active ? 0 : -1;
    el(`#${tab.getAttribute("aria-controls")}`).hidden = !active;
  }
  el("#settings-content").scrollTop = 0;
  if (focus) selected.focus();
}

function initSettingsSections() {
  const nav = el("#settings-nav");
  const tabs = [...nav.querySelectorAll("[data-settings-section]")];
  const narrow = window.matchMedia("(max-width: 680px)");
  const updateOrientation = () => nav.setAttribute("aria-orientation", narrow.matches ? "horizontal" : "vertical");
  updateOrientation();
  narrow.addEventListener("change", updateOrientation);
  tabs.forEach((tab, index) => {
    tab.addEventListener("click", () => selectSettingsSection(tab.dataset.settingsSection));
    tab.addEventListener("keydown", (event) => {
      const previous = narrow.matches ? "ArrowLeft" : "ArrowUp";
      const next = narrow.matches ? "ArrowRight" : "ArrowDown";
      let target;
      if (event.key === previous) target = (index - 1 + tabs.length) % tabs.length;
      else if (event.key === next) target = (index + 1) % tabs.length;
      else if (event.key === "Home") target = 0;
      else if (event.key === "End") target = tabs.length - 1;
      else return;
      event.preventDefault();
      selectSettingsSection(tabs[target].dataset.settingsSection, true);
    });
  });
}

let settingsModal;
function getSettingsModal() {
  return settingsModal ||= createModal(el("#settings-modal"), {
    isBusy: () => backupBusy,
    onClose() { if (scanning) void onScanCancel(); },
  });
}

function closeSettings() {
  getSettingsModal().close();
}

function beginSettingsSave() {
  return getSettingsModal().beginSave([
    el("#settings-done"), el("#accounts-interval"), el("#local-sync-interval"),
    el("#autostart-enabled"), el("#autostart-silent"),
  ]);
}

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
const localSyncStatus = statusBox("#local-sync-status");
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

/** 刷新间隔文案：不足 1 小时按分钟，整小时按小时（60 → 1 小时，1440 → 24 小时）。 */
function intervalText(minutes) {
  return minutes >= 60 && minutes % 60 === 0 ? translate("settings.hr", minutes / 60) : translate("settings.min", minutes);
}

async function onIntervalChange() {
  const operation = beginSettingsSave();
  if (!operation) return;
  const select = el("#accounts-interval");
  const previous = getRefreshIntervalMinutes();
  try {
    const minutes = await setRefreshInterval(Number(select.value));
    if (!operation.current()) return;
    intervalStatus.set(
      "ok",
      () => minutes > 0 ? translate("settings.globalIntervalSetToForSharedUsage", intervalText(minutes))
        : translate("settings.globalRefreshIsOffIndividualAccountSchedules")
    );
  } catch (error) {
    if (!operation.current()) return;
    select.value = String(previous);
    intervalStatus.set("bad", () => translate("settings.couldNotSaveTheRefreshInterval", resetError(error)));
  } finally {
    operation.finish();
  }
}

/* ---------- 同步本机 ChatGPT 凭据的间隔 ---------- */

async function loadLocalSyncInterval() {
  try {
    const minutes = await invoke("local_sync_get");
    el("#local-sync-interval").value = String(minutes);
  } catch {
    // 非 Tauri 环境读取失败，静默即可（仅影响回显）
  }
}

async function onLocalSyncChange() {
  const operation = beginSettingsSave();
  if (!operation) return;
  const select = el("#local-sync-interval");
  try {
    const minutes = await invoke("local_sync_set", { minutes: Number(select.value) });
    if (!operation.current()) return;
    const text = minutes === 1440 ? translate("settings.1Day") : intervalText(minutes);
    localSyncStatus.set("ok", () => translate("settings.localAuthJsonWillBeCheckedEvery", text));
  } catch (error) {
    if (!operation.current()) return;
    void loadLocalSyncInterval();
    localSyncStatus.set("bad", () => translate("settings.couldNotSaveTheSyncInterval", resetError(error)));
  } finally {
    operation.finish();
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
  const operation = beginSettingsSave();
  if (!operation) return;
  const enabled = el("#autostart-enabled").checked;
  const silent = el("#autostart-silent").checked;
  try {
    const v = await invoke("autostart_set", { enabled, silent });
    if (!operation.current()) return;
    reflectAutostart(!!v.enabled, !!v.silent);
    autostartStatus.set(
      "ok",
      () => v.enabled
        ? v.silent
          ? translate("settings.launchAtLoginEnabledTheAppWill")
          : translate("settings.launchAtLoginEnabled")
        : translate("settings.launchAtLoginDisabled")
    );
  } catch (error) {
    if (!operation.current()) return;
    // 写系统失败时回读真实状态，避免界面与系统不一致
    void loadAutostart();
    autostartStatus.set("bad", () => translate("settings.couldNotChangeLaunchAtLogin", resetError(error)));
  } finally {
    operation.finish();
    if (operation.current()) el("#autostart-silent").disabled = !el("#autostart-enabled").checked;
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
  bindText(input, () => mode === "export" ? translate("settings.optionalPasswordLeaveBlankForAnUnencrypted") : translate("settings.thisBackupIsEncryptedEnterItsPassword"), "placeholder");
  bindText(el("#backup-password-ok"), () => mode === "export" ? translate("settings.export") : translate("settings.decryptAndImport"));
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
  backupStatus.set("", () => translate("settings.exportingAllData"));
  try {
    const r = await invoke("backup_export", { password: password || null, uiPrefs: collectUiPrefs() });
    if (r && r.cancelled) {
      backupStatus.clear();
      return;
    }
    backupStatus.set("ok", () => translate("settings.exportedAllData", r.encrypted ? translate("settings.encrypted") : "", r.path));
  } catch (error) {
    backupStatus.set("bad", () => translate("accounts.exportFailed", resetError(error)));
  } finally {
    backupBusy = false;
    updateBackupButtons();
  }
}

async function doImportApply(path, password) {
  backupBusy = true;
  updateBackupButtons();
  backupStatus.set("", () => translate("settings.importingDataAndApplyingSettings"));
  try {
    const r = await invoke("backup_import_apply", { path, password: password || null });
    hidePasswordRow();
    applyUiPrefs(r && r.uiPrefs);
    // 设置弹窗内的回显同步导入结果（间隔选择框由 accounts-changed 事件自动更新）
    void loadAutostart();
    void loadClientPaths();
    void loadSyncSettings();
    const parts = [translate("settings.addedAccounts", Number(r && r.imported) || 0)];
    const exists = Number(r && r.skippedExists) || 0;
    const invalid = Number(r && r.skippedInvalid) || 0;
    const usageRestored = Number(r && r.usageRestored) || 0;
    if (exists > 0) parts.push(translate("settings.skippedExistingAccounts", exists));
    if (invalid > 0) parts.push(translate("settings.invalidAccounts", invalid));
    if (usageRestored > 0) parts.push(translate("settings.restoredUsageForDeletedAccounts", usageRestored));
    if (r.opencodeEventsRestored > 0) parts.push(translate("settings.restoredOpencodeRequestRecords", r.opencodeEventsRestored));
    if (r.opencodeUsageError) {
      const reason = {
        opencode_archive_open_failed: translate("settings.couldNotOpenTheArchiveDatabase"),
        opencode_archive_read_failed: translate("settings.couldNotReadTheArchiveDatabase"),
        opencode_archive_write_failed: translate("settings.couldNotWriteToTheArchiveDatabase"),
      }[r.opencodeUsageError] || resetError(r.opencodeUsageError);
      backupStatus.set("warn", () => translate("settings.partiallyRestoredAccountsAndSettingsWereApplied", parts.join("，"), reason));
    } else {
      backupStatus.set("ok", () => translate("settings.importCompletedOtherSettingsWereApplied", parts.join("，")));
    }
    // 新导入的账号后台排队刷新验证，与账户页导入体验一致
    const ids = Array.isArray(r && r.importedIds) ? r.importedIds : [];
    if (ids.length) void refreshAccounts(ids);
  } catch (error) {
    const msg = errorCode(error);
    if (msg === "wrong_password") {
      backupStatus.set("bad", () => translate("settings.incorrectPasswordTryAgain"));
    } else if (msg === "invalid_format") {
      hidePasswordRow();
      backupStatus.set("bad", () => translate("settings.invalidBackupFile"));
    } else {
      backupStatus.set("bad", () => translate("accounts.importFailed", resetError(msg)));
    }
  } finally {
    backupBusy = false;
    updateBackupButtons();
  }
}

function onBackupExportClick() {
  if (getSettingsModal().busy) return;
  backupStatus.clear();
  showPasswordRow("export", "");
}

async function onBackupImportClick() {
  if (getSettingsModal().busy) return;
  backupStatus.clear();
  hidePasswordRow();
  backupBusy = true;
  updateBackupButtons();
  let picked = null;
  try {
    const r = await invoke("backup_import_inspect");
    if (r && !r.cancelled) picked = r;
  } catch (error) {
    const msg = errorCode(error);
    backupStatus.set("bad", () => msg === "invalid_format" ? translate("settings.invalidBackupFile") : translate("accounts.importFailed", resetError(msg)));
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
  if (getSettingsModal().busy) return;
  const password = el("#backup-password").value.trim();
  if (backupPasswordMode === "export") {
    hidePasswordRow();
    void doExport(password || null);
    return;
  }
  if (backupPasswordMode === "import") {
    if (!password) {
      backupStatus.set("bad", () => translate("settings.enterTheBackupPassword"));
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
  { input: "#opencode-exe-path", get: "opencode_client_get", set: "opencode_client_set", invalid: "opencode_exe_invalid",
    status: statusBox("#opencode-path-status"), name: "OpenCode V2 CLI", detectBtn: "#opencode-detect", detect: "opencode_client_detect", dbInput: "#opencode-db-path", extraPaths: { desktopExePath: "#opencode-desktop-exe-path", desktopDbPath: "#opencode-desktop-db-path" } },
  {
    input: "#cursor-exe-path",
    get: "cursor_client_get",
    set: "cursor_client_set",
    invalid: "cursor_exe_invalid",
    status: cursorPathStatus,
  },
  {
    input: "#codex-exe-path",
    extraPaths: { cliExePath: "#codex-cli-exe-path" },
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
    extraPaths: { cliExePath: "#claude-cli-exe-path", desktopDataPath: "#claude-desktop-data-path" },
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
      if (c.dbInput) el(c.dbInput).value = v?.dbPath || "";
      for (const [key, input] of Object.entries(c.extraPaths || {})) el(input).value = v?.[key] || "";
    } catch {
      // 浏览器直开等非 Tauri 环境静默
    }
  }
}

/** 一键探测常见安装位置，命中即填入输入框（保存仍需点「完成」）。 */
async function onDetect(c) {
  const btn = el(c.detectBtn);
  btn.disabled = true;
  c.status.set("", translate("settings.searchingForLocal", c.name));
  try {
    const r = await invoke(c.detect);
    const found = [];
    if (r?.exePath) { el(c.input).value = r.exePath; found.push(r.exePath); }
    for (const [key, input] of Object.entries(c.extraPaths || {})) {
      if (r?.[key]) { el(input).value = r[key]; found.push(r[key]); }
    }
    if (found.length) {
      c.status.set("ok", translate("settings.foundClientPathsClickDoneToSave", found.length));
    } else {
      c.status.set("warn", translate("settings.couldNotFindEnterTheFullPath", c.name));
    }
  } catch (error) {
    c.status.set("bad", translate("settings.searchFailed", resetError(error)));
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
  bindText(box, text);
}

function updateScanButton() {
  bindText(el("#cursor-detect"), () => sessionActive ? translate("settings.findNext") : translate("settings.findAutomatically"));
}

// 逐个查找：命中一个即暂停并填入输入框，再点「搜索下一个」从原处继续，直到扫完。
async function onStep() {
  if (scanning) return;
  const restart = !sessionActive;
  scanning = true;
  el("#cursor-detect").disabled = true;
  el("#cursor-scan-cancel").hidden = false;
  cursorPathStatus.set("", () => restart ? translate("settings.scanningForCursor") : translate("settings.findingTheNextMatch"));
  setScanProgress(() => translate("settings.scanning"));
  try {
    // 先挂上进度监听，再触发扫描，避免漏掉早期事件。
    unlistenScan = await listen("cursor-scan-progress", (e) => {
      const p = (e && e.payload) || {};
      const cur = p.current ? `：${p.current}` : "";
      setScanProgress(() => translate("settings.scannedFoldersDepth", p.scanned || 0, p.depth || 0, cur));
    });
    const r = await invoke("cursor_client_scan_step", { restart });
    if (r && r.found) {
      el("#cursor-exe-path").value = r.found;
      if (r.done) {
        sessionActive = false;
        cursorPathStatus.set("ok", () => translate("settings.lastMatchClickDoneToSave", r.found));
      } else {
        sessionActive = true;
        cursorPathStatus.set("ok", () => translate("settings.foundClickDoneToSaveOrFind", r.found));
      }
    } else if (r && r.cancelled) {
      sessionActive = false;
      cursorPathStatus.set("warn", () => translate("settings.scanCancelled"));
    } else {
      sessionActive = false;
      cursorPathStatus.set("warn", () => restart ? translate("settings.cursorNotFoundEnterTheFullPath") : translate("settings.noFurtherMatches"));
    }
  } catch (error) {
    sessionActive = false;
    cursorPathStatus.set("bad", () => translate("settings.scanFailed", resetError(error)));
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
  setScanProgress(() => translate("settings.cancelling"));
  try { await invoke("cursor_client_scan_cancel"); } catch { /* ignore */ }
  sessionActive = false;
}

// 点「完成」：保存同步配置与客户端路径并关闭；配置无效则定位错误且不关闭。空路径 = 清除配置。
async function onDone() {
  const operation = beginSettingsSave();
  if (!operation) return;
  try {
    const saved = await saveSyncSettings(true);
    if (!operation.current()) return;
    if (!saved) {
      selectSettingsSection("sync");
      el("#gpt-sync-result").focus();
      return;
    }
    if (scanning) void onScanCancel();
    for (const c of CLIENT_PATHS) {
      try {
        await invoke(c.set, { exePath: el(c.input).value.trim(), ...(c.dbInput ? { dbPath: el(c.dbInput).value.trim() } : {}),
          ...Object.fromEntries(Object.entries(c.extraPaths || {}).map(([key, input]) => [key, el(input).value.trim()])) });
        if (!operation.current()) return;
      } catch (error) {
        if (!operation.current()) return;
        if (errorCode(error) === c.invalid) {
          c.status.set("bad", translate("settings.thePathIsInvalidOrMissingCorrect"));
        } else {
          c.status.set("bad", translate("settings.couldNotSavePath", resetError(error)));
        }
        operation.finish();
        selectSettingsSection("clients");
        el(c.input).focus();
        return;
      }
    }
    if (operation.current()) closeSettings();
  } finally {
    operation.finish();
  }
}

/* ---------- 初始化 ---------- */

export function initSettings() {
  window.addEventListener("uilanguagechange", renderUnitSeg);
  const language = el("#ui-language");
  el("#ui-language-folder").addEventListener("click", async () => {
    try { await invoke("ui_language_open_folder"); }
    catch (error) { fillStatus(el("#ui-language-status"), "bad", () => translate("settings.openLanguageFolderFailed", resetError(error))); }
  });
  language.value = languagePreference();
  language.addEventListener("change", async () => {
    const operation = getSettingsModal().beginSave([language, el("#settings-done")]);
    if (!operation) { language.value = languagePreference(); return; }
    try {
      await setLanguage(language.value);
      statusBox("#ui-language-status").clear();
    } catch (error) {
      language.value = languagePreference();
      fillStatus(el("#ui-language-status"), "bad", () => translate("settings.languageSaveFailed", resetError(error)));
    } finally { operation.finish(); }
  });
  initSettingsSections();
  initSyncSettings();
  el("#settings-btn").addEventListener("click", () => {
    if (getSettingsModal().busy) return;
    getSettingsModal().open();
    renderUnitSeg();
    intervalStatus.clear();
    el("#accounts-interval").value = String(getRefreshIntervalMinutes());
    localSyncStatus.clear();
    void loadLocalSyncInterval();
    void loadSyncSettings();
    autostartStatus.clear();
    backupStatus.clear();
    hidePasswordRow();
    updateBackupButtons();
    for (const c of CLIENT_PATHS) c.status.clear();
    setScanProgress("");
    sessionActive = false;
    updateScanButton();
    el("#cursor-scan-cancel").hidden = true;
    el("#settings-nav [aria-selected='true']").focus();
    void loadAutostart();
    void loadClientPaths();
  });
  for (const seg of el("#unit-mode").querySelectorAll(".seg")) {
    seg.addEventListener("click", () => {
      setNumberUnit(seg.dataset.unit);
      renderUnitSeg();
    });
  }
  el("#accounts-interval").addEventListener("change", () => { void onIntervalChange(); });
  el("#local-sync-interval").addEventListener("change", () => { void onLocalSyncChange(); });
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
