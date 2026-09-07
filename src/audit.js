import { el, invoke, fmtDateMs, resetError, toast, dismissToast } from "./shared.js";
import { onAccountsChanged } from "./accounts.js";

// 审计日志：展示后端记录的账户增删改、状态变化、Codex / Claude 续期与本机同步、设置变更等事件。
// 日志由后端写入 {用户目录}/.xilore/myaiassistant/audit.jsonl，这里只读展示 + 清空。
// 每条记录带级别（信息 / 警告 / 错误）与分类，事件标签也由后端（audit.rs 的事件登记表）给出，
// 这里只维护级别与分类的展示文案，并按两者筛选。

const LEVEL_META = {
  info: { label: "信息", cls: "" },
  warn: { label: "警告", cls: "warn" },
  error: { label: "错误", cls: "bad" },
};

// 与 index.html 里 #audit-category 的选项一致
const CATEGORY_LABELS = {
  account: "账户管理",
  state: "状态与刷新",
  credential: "凭据与同步",
  usage: "用量数据",
  settings: "设置与系统",
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

function levelMeta(level) {
  return LEVEL_META[level] || LEVEL_META.info;
}

function render() {
  const level = el("#audit-level").value;
  const category = el("#audit-category").value;
  const rows = entries.filter(
    (e) => (level === "all" || e.level === level) && (category === "all" || e.category === category)
  );
  el("#audit-body").replaceChildren(
    ...rows.map((entry) => {
      const tr = document.createElement("tr");
      tr.className = `audit-row level-${entry.level || "info"}`;
      const timeTd = document.createElement("td");
      timeTd.className = "audit-time";
      timeTd.textContent = fmtDateMs(entry.ts);
      const levelTd = document.createElement("td");
      const meta = levelMeta(entry.level);
      const tag = document.createElement("span");
      tag.className = `tag${meta.cls ? ` ${meta.cls}` : ""}`;
      tag.textContent = meta.label;
      levelTd.append(tag);
      const categoryTd = document.createElement("td");
      categoryTd.className = "audit-category";
      categoryTd.textContent = CATEGORY_LABELS[entry.category] || entry.category || "";
      const eventTd = document.createElement("td");
      eventTd.className = "audit-event";
      eventTd.textContent = entry.label || entry.event || "事件";
      const msgTd = document.createElement("td");
      msgTd.className = "audit-message";
      msgTd.textContent = entry.message || "";
      tr.append(timeTd, levelTd, categoryTd, eventTd, msgTd);
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
  el("#audit-level").addEventListener("change", render);
  el("#audit-category").addEventListener("change", render);
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
