import { el, invoke, listen, fillStatus, resetError, copyText, toast } from "./shared.js";
import { GPT_SYNC_SERVER_PROMPT } from "./usage_sync_prompt.js";

const errors = {
  sync_invalid_url: "请填写 HTTPS 服务器地址，不包含账号、密码、查询参数或片段。",
  sync_key_required: "请填写同步密钥，或清空服务器地址以停止同步。",
  sync_invalid_key: "同步密钥须为不含空格的可见 ASCII 字符，最长 4096 字符。",
  sync_invalid_interval: "请选择支持的同步间隔。",
  sync_invalid_device: "设备名称无效，最多 128 个 UTF-8 字节且不能包含控制字符。",
  sync_auth: "同步密钥无效或没有访问权限，请检查配置。",
  sync_unauthorized: "同步密钥无效或已被重置。请从服务端管理员处取得当前密钥，并更新该账户所有机器的配置。",
  sync_forbidden: "同步账户已停用。请联系服务端管理员启用账户，再点击“立即同步”。本机历史仍保留。",
  sync_network: "无法连接同步服务器或请求超时，已保留本机数据。",
  sync_server: "同步服务器暂时不可用，稍后自动重试。",
  sync_rate_limited: "同步服务器限制了请求频率，稍后自动重试。",
  sync_protocol_version: "服务器协议版本不兼容，需要 v1 协议。",
  sync_protocol_invalid: "服务器响应不符合同步协议，已暂停自动同步。",
  sync_response_too_large: "服务器返回的数据超过协议限制，已暂停自动同步。",
  sync_invalid_record: "收到无效用量记录，未保存该页数据。",
  sync_reset_required: "服务器数据代次或游标已改变，请重新同步。",
  sync_not_configured: "尚未配置同步服务器和密钥。",
  sync_cancelled: "配置或缓存已更改，本轮同步已取消。",
};
export function syncError(error) {
  const code = resetError(error);
  if (code.startsWith("sync_store_failed")) return "用量存档读写失败，请检查磁盘空间与文件权限。";
  return errors[code] || code;
}
let savedForm = "";
let busy = false;
let ready = false;
function form() {
  return {
    serverUrl: el("#gpt-sync-url").value.trim(),
    key: el("#gpt-sync-clear-key").checked ? "" : el("#gpt-sync-key").value.trim() || null,
    intervalMinutes: Number(el("#gpt-sync-interval").value),
    sourceHome: el("#gpt-sync-home").value.trim(),
    deviceName: el("#gpt-sync-device-name").value.trim(),
  };
}
function setResult(kind, text) { fillStatus(el("#gpt-sync-result"), kind, text); }
export async function refreshSyncStatus() {
  try {
    const s = await invoke("codex_usage_sync_status");
    const states = { disabled: "未启用", running: "同步中", paused: "已暂停", error: "等待重试", idle: "已就绪" };
    const last = s.lastSuccessAt ? new Date(s.lastSuccessAt).toLocaleString("zh-CN", { hour12: false }) : "尚无成功记录";
    const text = `${states[s.state] || s.state} · 待上传 ${s.pendingRecords} 条 · 最近成功：${last}${s.error ? ` · ${syncError(s.error)}` : ""}`;
    for (const id of ["#gpt-sync-status", "#usage-sync-summary"]) {
      const node = document.querySelector(id);
      if (node) node.textContent = text;
    }
    el("#gpt-sync-now").disabled = busy || s.state === "running" || s.state === "disabled";
  } catch (error) { setResult("bad", syncError(error)); }
}
export async function loadSyncSettings() {
  ready = false;
  try {
    const c = await invoke("codex_usage_sync_get");
    el("#gpt-sync-url").value = c.serverUrl;
    el("#gpt-sync-key").value = "";
    el("#gpt-sync-key").placeholder = c.hasKey ? "已保存，留空保留原密钥" : "填写服务端管理员提供的账户同步密钥";
    el("#gpt-sync-clear-key").checked = false;
    el("#gpt-sync-interval").value = String(c.intervalMinutes);
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
  for (const id of ["#gpt-sync-save", "#gpt-sync-test", "#gpt-sync-now"]) el(id).disabled = true;
  try { await task(); } catch (error) { setResult("bad", syncError(error)); }
  finally {
    busy = false;
    el("#gpt-sync-save").disabled = false;
    el("#gpt-sync-test").disabled = false;
    await refreshSyncStatus();
  }
}
export function initSyncSettings() {
  el("#gpt-sync-copy-prompt").addEventListener("click", async () => {
    try {
      await copyText(GPT_SYNC_SERVER_PROMPT);
      toast("ok", "搭建提示词已复制，可粘贴给你的 agent。", { key: "gpt-sync-setup" });
    } catch {
      toast("bad", "复制失败，请重试。", { key: "gpt-sync-setup" });
    }
  });
  el("#gpt-sync-save").addEventListener("click", () => action(() => saveSyncSettings()));
  el("#gpt-sync-test").addEventListener("click", () => action(async () => {
    setResult("", "正在测试连接…");
    await invoke("codex_usage_sync_test", { config: form() });
    setResult("ok", "账户验证成功，服务器支持 v1 协议。此操作仅测试连接，未上传数据或登记设备。");
  }));
  el("#gpt-sync-now").addEventListener("click", () => action(async () => {
    if (!await saveSyncSettings(true)) return;
    setResult("", "正在同步 GPT 用量…");
    const s = await invoke("codex_usage_sync_now");
    setResult("ok", s.state === "running" ? "同步正在进行。" : "同步完成。");
  }));
  listen("codex-usage-sync-status", () => { void refreshSyncStatus(); }).catch(() => {});
  listen("codex-usage-changed", () => { void refreshSyncStatus(); }).catch(() => {});
  void refreshSyncStatus();
}
