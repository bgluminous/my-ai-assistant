import { bindText, translate } from "./i18n.js";
import { el, invoke, fillStatus, copyText, toast } from "./shared.js";
import { syncError } from "./sync_errors.js";
import { mountSyncProgress, refreshSyncStatus } from "./sync_progress.js";
export { syncError } from "./sync_errors.js";
export { refreshSyncStatus } from "./sync_progress.js";

let savedForm = "";
let busy = false;
let ready = false;
function form() {
  return {
    serverUrl: el("#gpt-sync-url").value.trim(),
    key: el("#gpt-sync-clear-key").checked ? "" : el("#gpt-sync-key").value.trim() || null,
    useProxy: el("#gpt-sync-use-proxy").checked,
    sourceHome: el("#gpt-sync-home").value.trim(),
    deviceName: el("#gpt-sync-device-name").value.trim(),
  };
}
function setResult(kind, text) { fillStatus(el("#gpt-sync-result"), kind, text); }
export async function loadSyncSettings() {
  ready = false;
  try {
    const c = await invoke("codex_usage_sync_get");
    el("#gpt-sync-url").value = c.serverUrl;
    el("#gpt-sync-key").value = "";
    bindText(el("#gpt-sync-key"), () => c.hasKey ? translate("usageSync.keySavedLeaveBlankToKeepIt") : translate("usageSync.enterTheSyncKeyFromTheServer"), "placeholder");
    el("#gpt-sync-clear-key").checked = false;
    el("#gpt-sync-use-proxy").checked = c.useProxy !== false;
    el("#gpt-sync-home").value = c.sourceHome;
    el("#gpt-sync-device-name").value = c.deviceName;
    savedForm = JSON.stringify(form());
    ready = true;
    await refreshSyncStatus();
  } catch (error) { setResult("bad", () => syncError(error)); }
}
export async function saveSyncSettings(onlyIfDirty = false) {
  if (!ready) return true; // 设置未载入时不能用空表单覆盖现有配置。
  if (onlyIfDirty && savedForm === JSON.stringify(form())) return true;
  try {
    await invoke("codex_usage_sync_set", { config: form() });
    await loadSyncSettings();
    setResult("ok", () => translate("usageSync.syncSettingsSaved"));
    return true;
  } catch (error) { setResult("bad", () => syncError(error)); return false; }
}
async function action(task) {
  if (busy) return;
  busy = true;
  el("#gpt-sync-test").disabled = true;
  try { await task(); } catch (error) { setResult("bad", () => syncError(error)); }
  finally {
    busy = false;
    el("#gpt-sync-test").disabled = false;
    await refreshSyncStatus();
  }
}
export function initSyncSettings() {
  mountSyncProgress(el("#usage-sync-summary"));
  mountSyncProgress(el("#gpt-sync-progress"), { showDisabled: true });
  el("#gpt-sync-copy-prompt").addEventListener("click", async () => {
    try {
      await copyText(translate("sync.setupServerPrompt"));
      toast("ok", () => translate("usageSync.setupPromptCopiedPasteItIntoYour"), { key: "gpt-sync-setup" });
    } catch {
      toast("bad", () => translate("usageSync.copyFailedTryAgain"), { key: "gpt-sync-setup" });
    }
  });
  el("#gpt-sync-test").addEventListener("click", () => action(async () => {
    setResult("", () => translate("usageSync.testingConnection"));
    await invoke("codex_usage_sync_test", { config: form() });
    setResult("ok", () => translate("usageSync.accountVerifiedTheServerSupportsV1"));
  }));
}
