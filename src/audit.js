import { el, invoke, fmtDateMs, resetError, toast, dismissToast } from "./shared.js";
import { onAccountsChanged } from "./accounts.js";

// 审计日志：展示后端记录的账户增删改、状态变化、Codex / Claude 续期、设置变更等事件。
// 日志由后端写入 {用户目录}/xilore/myaiassistant/audit.jsonl，这里只读展示 + 清空。

const EVENT_META = {
  account_add: { label: "添加账户", cls: "ok", group: "account" },
  account_update: { label: "编辑账户", cls: "accent", group: "account" },
  account_delete: { label: "删除账户", cls: "bad", group: "account" },
  account_import: { label: "本机导入", cls: "ok", group: "account" },
  account_import_file: { label: "导入账户", cls: "ok", group: "account" },
  account_export: { label: "导出账户", cls: "accent", group: "account" },
  usage_snapshot: { label: "用量快照", cls: "accent", group: "account" },
  account_state_changed: { label: "状态变化", cls: "warn", group: "state" },
  account_refresh_failed: { label: "刷新失败", cls: "bad", group: "state" },
  codex_renewed: { label: "自动续期", cls: "ok", group: "renew" },
  codex_renew_failed: { label: "续期失败", cls: "bad", group: "renew" },
  claude_renewed: { label: "自动续期", cls: "ok", group: "renew" },
  claude_renew_failed: { label: "续期失败", cls: "bad", group: "renew" },
  cursor_switch_local: { label: "切换登录", cls: "accent", group: "account" },
  codex_switch_local: { label: "切换登录", cls: "accent", group: "account" },
  claude_switch_local: { label: "切换登录", cls: "accent", group: "account" },
  settings_load_failed: { label: "设置载入失败", cls: "bad", group: "settings" },
  interval_set: { label: "定时设置", cls: "accent", group: "settings" },
  usage_interval_set: { label: "统计设置", cls: "accent", group: "settings" },
  autostart_set: { label: "开机启动", cls: "accent", group: "settings" },
  backup_export: { label: "导出备份", cls: "accent", group: "settings" },
  backup_import: { label: "导入备份", cls: "ok", group: "settings" },
};

let entries = [];
let total = 0;
let panelVisible = false;
let loading = false;
let clearArmed = null; // 两步清空确认的超时句柄
let reloadTimer = null;

function setStatus(kind, text) {
  toast(kind, text, { key: "audit" });
}
function clearStatus() {
  dismissToast("audit");
}

function metaFor(event) {
  return EVENT_META[event] || { label: event || "事件", cls: "", group: "other" };
}

function render() {
  const filter = el("#audit-filter").value;
  const rows = filter === "all" ? entries : entries.filter((e) => metaFor(e.event).group === filter);
  el("#audit-body").replaceChildren(
    ...rows.map((entry) => {
      const tr = document.createElement("tr");
      const timeTd = document.createElement("td");
      timeTd.className = "audit-time";
      timeTd.textContent = fmtDateMs(entry.ts);
      const typeTd = document.createElement("td");
      const meta = metaFor(entry.event);
      const tag = document.createElement("span");
      tag.className = `tag${meta.cls ? ` ${meta.cls}` : ""}`;
      tag.textContent = meta.label;
      typeTd.append(tag);
      const msgTd = document.createElement("td");
      msgTd.className = "audit-message";
      msgTd.textContent = entry.message || "";
      tr.append(timeTd, typeTd, msgTd);
      return tr;
    })
  );
  el("#audit-empty").hidden = rows.length > 0;
  const filtered = rows.length !== entries.length ? ` · 筛选后 ${rows.length} 条` : "";
  const capped = total > entries.length ? `（显示最近 ${entries.length} 条）` : "";
  el("#audit-count").textContent = total ? `共 ${total} 条${filtered}${capped}` : "";
}

async function load() {
  if (loading) return;
  loading = true;
  try {
    const view = await invoke("audit_list", { limit: 500 });
    entries = view && Array.isArray(view.entries) ? view.entries : [];
    total = Number(view && view.total) || 0;
    clearStatus();
    render();
  } catch (error) {
    setStatus("bad", `加载审计日志失败：${resetError(error)}`);
  } finally {
    loading = false;
  }
}

function disarmClear() {
  if (clearArmed != null) {
    clearTimeout(clearArmed);
    clearArmed = null;
  }
  const btn = el("#audit-clear");
  btn.textContent = "清空日志";
  btn.classList.remove("danger");
}

async function onClearClick() {
  if (clearArmed != null) {
    disarmClear();
    try {
      await invoke("audit_clear");
      entries = [];
      total = 0;
      render();
      setStatus("ok", "审计日志已清空。");
    } catch (error) {
      setStatus("bad", `清空失败：${resetError(error)}`);
    }
    return;
  }
  const btn = el("#audit-clear");
  btn.textContent = "确认清空？";
  btn.classList.add("danger");
  clearArmed = setTimeout(disarmClear, 3000);
}

export function initAudit() {
  el("#audit-refresh").addEventListener("click", () => {
    void load();
  });
  el("#audit-filter").addEventListener("change", render);
  el("#audit-clear").addEventListener("click", () => {
    void onClearClick();
  });

  window.addEventListener("panelshown", (event) => {
    panelVisible = !!(event.detail && event.detail.id === "audit-panel");
    if (panelVisible) void load();
  });

  // 账户操作 / 定时刷新会产生新日志：面板可见时稍作防抖后自动重载
  onAccountsChanged(() => {
    if (!panelVisible) return;
    if (reloadTimer != null) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => {
      reloadTimer = null;
      void load();
    }, 800);
  });
}
