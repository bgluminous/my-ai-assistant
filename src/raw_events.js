import { el, fmtInt, fmtUsd, fmtDateMs, resetError, toast, fillStatus, escapeHtml } from "./shared.js";
import { fetchRawEvents, clearCursorLocalData, exportUsageEventsCsv } from "./usage_data.js";
import { confirmDialog } from "./accounts.js";

// 原始账单弹窗：列出某个 Cursor 账户事件库在给定时间范围内的逐笔用量事件——Cursor 接口上报的原始
// 模型名、四类 token、官方实扣，以及当前价格表下的归一名 / 命中价格键 / 等价费用。数据完全来自本机
// 事件库（不联网）。在用账户与已删除账户保留的数据都能查看；在用账户还可在此清除本地用量数据。
// 入口：用量页单账户视图的工具栏按钮、总览「各来源账单」表的行内按钮。

const PAGE_SIZE = 300; // 分批渲染，避免几千行一次性插入卡顿

const state = {
  accountId: null,
  label: "",
  deleted: false,
  fileName: "",
  events: [], // 后端返回的全部事件（范围内，时间倒序）
  filtered: [], // 应用搜索后的子集
  shown: 0, // 已渲染的 filtered 行数
  loadSeq: 0,
  onCleared: null,
};

function setStatus(kind, text) {
  fillStatus(el("#raw-status"), kind, text);
}
function clearStatus() {
  const box = el("#raw-status");
  box.hidden = true;
  box.textContent = "";
}

function priceTag(ev) {
  if (!ev.pricedAs) return '<span class="tag unpriced">未定价</span>';
  const pricedAs = escapeHtml(ev.pricedAs);
  // 归一名与计价键不同（如 grok-4.6-fast 按 grok-4.6 计价）时把归一名也标出来
  const shown =
    ev.displayModel && ev.displayModel !== ev.pricedAs ? `<span class="raw-display">${escapeHtml(ev.displayModel)}</span>` : "";
  return `${shown}<span class="tag">${pricedAs}</span>`;
}

function rowHtml(ev) {
  const time = ev.timestampMs != null ? fmtDateMs(ev.timestampMs) : "—";
  return `<tr>
    <td class="raw-time">${escapeHtml(time)}</td>
    <td class="raw-model" title="${escapeHtml(ev.model)}">${escapeHtml(ev.model)}</td>
    <td>${priceTag(ev)}</td>
    <td class="num">${fmtInt(ev.inputTokens)}</td>
    <td class="num">${fmtInt(ev.cacheReadTokens)}</td>
    <td class="num">${fmtInt(ev.cacheWriteTokens)}</td>
    <td class="num">${fmtInt(ev.outputTokens)}</td>
    <td class="num">${fmtInt(ev.totalTokens)}</td>
    <td class="num">${fmtUsd(ev.actualUsd)}</td>
    <td class="num">${ev.equivalentUsd == null ? "—" : fmtUsd(ev.equivalentUsd)}</td>
  </tr>`;
}

function renderMore() {
  const body = el("#raw-body");
  const next = state.filtered.slice(state.shown, state.shown + PAGE_SIZE);
  body.insertAdjacentHTML("beforeend", next.map(rowHtml).join(""));
  state.shown += next.length;
  const remaining = state.filtered.length - state.shown;
  el("#raw-more-wrap").hidden = remaining <= 0;
  el("#raw-more").textContent = `显示更多（还有 ${fmtInt(remaining)} 条）`;
}

/** 按搜索词过滤后重绘：合计只统计过滤后的事件，与导出范围一致。 */
function render() {
  const q = el("#raw-search").value.trim().toLowerCase();
  state.filtered = q
    ? state.events.filter(
        (ev) =>
          ev.model.toLowerCase().includes(q) ||
          (ev.displayModel && ev.displayModel.toLowerCase().includes(q)) ||
          (ev.pricedAs && ev.pricedAs.toLowerCase().includes(q))
      )
    : state.events;
  el("#raw-body").replaceChildren();
  state.shown = 0;
  renderMore();

  const empty = el("#raw-empty");
  empty.hidden = state.filtered.length > 0;
  empty.textContent = state.events.length ? "没有匹配的事件。" : "该范围内没有用量事件。";

  const totals = state.filtered.reduce(
    (acc, ev) => {
      acc.tokens += ev.totalTokens;
      acc.actual += ev.actualUsd;
      if (ev.equivalentUsd != null) acc.equivalent += ev.equivalentUsd;
      else acc.unpriced += 1;
      return acc;
    },
    { tokens: 0, actual: 0, equivalent: 0, unpriced: 0 }
  );
  const parts = [
    `${fmtInt(state.filtered.length)} 条`,
    `${fmtInt(totals.tokens)} tok`,
    `实扣 ${fmtUsd(totals.actual)}`,
    `等价 ${fmtUsd(totals.equivalent)}`,
  ];
  if (totals.unpriced) parts.push(`${fmtInt(totals.unpriced)} 条未定价`);
  el("#raw-count").textContent = parts.join(" · ");
  el("#raw-export").disabled = state.filtered.length === 0;
}

function csvCell(value) {
  const text = String(value ?? "");
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

/** 导出当前列出（已过滤）的事件：时间用本地 ISO 风格，金额保留 6 位小数便于核对。 */
async function onExport() {
  if (!state.filtered.length) return;
  const header = [
    "时间",
    "时间戳(ms)",
    "模型(原始)",
    "归一名",
    "计价键",
    "输入",
    "缓存读",
    "缓存写",
    "输出",
    "合计Token",
    "实扣(USD)",
    "等价(USD)",
  ];
  const lines = [header.map(csvCell).join(",")];
  for (const ev of state.filtered) {
    lines.push(
      [
        ev.timestampMs != null ? fmtDateMs(ev.timestampMs) : "",
        ev.timestampMs ?? "",
        ev.model,
        ev.displayModel || "",
        ev.pricedAs || "",
        Math.round(ev.inputTokens),
        Math.round(ev.cacheReadTokens),
        Math.round(ev.cacheWriteTokens),
        Math.round(ev.outputTokens),
        Math.round(ev.totalTokens),
        ev.actualUsd.toFixed(6),
        ev.equivalentUsd == null ? "" : ev.equivalentUsd.toFixed(6),
      ]
        .map(csvCell)
        .join(",")
    );
  }
  const btn = el("#raw-export");
  btn.disabled = true;
  try {
    const result = await exportUsageEventsCsv(state.fileName, `${lines.join("\r\n")}\r\n`);
    if (result && !result.cancelled) toast("ok", `已导出：${result.path}`, { key: "raw-export" });
  } catch (error) {
    setStatus("bad", `导出失败：${resetError(error)}`);
  } finally {
    btn.disabled = state.filtered.length === 0;
  }
}

/** 清除在用账户的本地用量数据：确认后删事件库 + 清缓存，成功即关闭弹窗并通知调用方。 */
async function onClear() {
  if (!state.accountId || state.deleted) return;
  const ok = await confirmDialog({
    title: "清除本地数据",
    body: `确定清除“${state.label}”在本机保存的用量事件库与统计缓存吗？账户与 Token 会保留，下次刷新用量时会重新从 Cursor 全量拉取；Cursor 侧已不可查询的历史事件将无法恢复。`,
    confirmText: "清除",
    danger: true,
  });
  if (!ok) return;
  const btn = el("#raw-clear");
  btn.disabled = true;
  try {
    const result = await clearCursorLocalData(state.accountId);
    toast("ok", `已清除“${state.label}”的本地用量数据（${fmtInt(result.events)} 条事件）。`, { key: "raw-clear" });
    const cb = state.onCleared;
    closeModal();
    if (typeof cb === "function") cb();
  } catch (error) {
    const code = resetError(error);
    setStatus("bad", code === "no_usage_data" ? "本机没有该账户的用量数据。" : `清除失败：${code}`);
  } finally {
    btn.disabled = false;
  }
}

function closeModal() {
  el("#raw-modal").hidden = true;
  state.loadSeq += 1; // 作废在途加载
  state.events = [];
  state.filtered = [];
  state.onCleared = null;
  el("#raw-body").replaceChildren();
}

function safeFileName(text) {
  return String(text).replace(/[\\/:*?"<>|\s]+/g, "_").slice(0, 60) || "cursor";
}

/**
 * 打开原始账单弹窗并加载事件。
 * - accountId / label：账户 id 与显示名；deleted 为 true 表示已删除账户保留的数据（不可清除）；
 * - start / end：unix 毫秒闭区间（null = 不限），rangeText 为区间的可读名称；
 * - onCleared：清除本地数据成功后的回调（用量页据此重载视图）。
 */
export async function openRawEvents({ accountId, label, deleted = false, start = null, end = null, rangeText = "全部", onCleared = null }) {
  state.accountId = accountId;
  state.label = label;
  state.deleted = !!deleted;
  state.onCleared = onCleared;
  state.fileName = `cursor-usage_${safeFileName(label)}_${safeFileName(rangeText)}.csv`;
  state.events = [];
  state.filtered = [];
  const seq = ++state.loadSeq;

  el("#raw-modal-title").textContent = `原始账单 · ${label}`;
  el("#raw-modal-sub").textContent = `${rangeText} · 加载中…`;
  el("#raw-search").value = "";
  el("#raw-body").replaceChildren();
  el("#raw-empty").hidden = true;
  el("#raw-more-wrap").hidden = true;
  el("#raw-count").textContent = "";
  el("#raw-export").disabled = true;
  el("#raw-clear").hidden = state.deleted;
  el("#raw-clear").disabled = false;
  clearStatus();
  el("#raw-modal").hidden = false;

  try {
    const result = await fetchRawEvents(accountId, { start, end });
    if (seq !== state.loadSeq) return;
    state.events = Array.isArray(result.events) ? result.events : [];
    const synced = result.syncedAt ? `同步于 ${fmtDateMs(result.syncedAt)}` : "尚未同步";
    el("#raw-modal-sub").textContent =
      `${rangeText} · 事件库共 ${fmtInt(result.total)} 条 · ${synced}` +
      (state.deleted ? " · 已删除账户保留的数据" : "") +
      ` · 数据来自本机事件库，不联网`;
    el("#raw-modal-sub").title = result.path || "";
    render();
  } catch (error) {
    if (seq !== state.loadSeq) return;
    const code = resetError(error);
    el("#raw-modal-sub").textContent = rangeText;
    if (code === "no_usage_data") {
      el("#raw-empty").hidden = false;
      el("#raw-empty").textContent = "本机还没有该账户的用量事件库，请先在用量统计页拉取一次用量。";
      el("#raw-clear").hidden = true;
    } else {
      setStatus("bad", `读取原始账单失败：${code}`);
    }
  }
}

export function initRawEvents() {
  const modal = el("#raw-modal");
  for (const node of modal.querySelectorAll("[data-close]")) node.addEventListener("click", closeModal);
  document.addEventListener("keydown", (e) => {
    // 上层的确认弹窗（清除本地数据）打开时，Esc 只关它，不连带关掉本弹窗
    const confirmOpen = !document.querySelector("#switch-modal").hidden;
    if (e.key === "Escape" && !modal.hidden && !confirmOpen) closeModal();
  });
  el("#raw-search").addEventListener("input", render);
  el("#raw-more").addEventListener("click", renderMore);
  el("#raw-export").addEventListener("click", () => { void onExport(); });
  el("#raw-clear").addEventListener("click", () => { void onClear(); });
}
