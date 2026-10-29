import { el, invoke, fillStatus, copyText, toast } from "./shared.js";
import { GPT_SYNC_SERVER_PROMPT } from "./usage_sync_prompt.js";
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
    el("#gpt-sync-key").placeholder = c.hasKey ? "已保存，留空保留原密钥" : "填写服务端管理员提供的账户同步密钥";
    el("#gpt-sync-clear-key").checked = false;
    el("#gpt-sync-use-proxy").checked = c.useProxy !== false;
    el("#gpt-sync-home").value = c.sourceHome;
    el("#gpt-sync-device-name").value = c.deviceName;
    savedForm = JSON.stringify(form());
    ready = true;
    await refreshSyncStatus();
  } catch (error) { setResult("bad", syncError(error)); }
}
export async function saveSyncSettings(onlyIfDirty = false) {
  if (!ready) return true; // 设置未载入时不能用空表单覆盖现有配置。
  if (onlyIfDirty && savedForm === JSON.stringify(form())) return true;
  try {
    await invoke("codex_usage_sync_set", { config: form() });
    await loadSyncSettings();
    setResult("ok", "同步设置已保存。");
    return true;
  } catch (error) { setResult("bad", syncError(error)); return false; }
}
async function action(task) {
  if (busy) return;
  busy = true;
  el("#gpt-sync-test").disabled = true;
  try { await task(); } catch (error) { setResult("bad", syncError(error)); }
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
      await copyText(GPT_SYNC_SERVER_PROMPT);
      toast("ok", "搭建提示词已复制，可粘贴给你的 agent。", { key: "gpt-sync-setup" });
    } catch {
      toast("bad", "复制失败，请重试。", { key: "gpt-sync-setup" });
    }
  });
  el("#gpt-sync-test").addEventListener("click", () => action(async () => {
    setResult("", "正在测试连接…");
    await invoke("codex_usage_sync_test", { config: form() });
    setResult("ok", "账户验证成功，服务器支持 v1 协议。此操作仅测试连接，未上传数据或登记设备。");
  }));
}
