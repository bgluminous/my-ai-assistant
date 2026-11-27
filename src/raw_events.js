import { errorCode, locale, translate } from "./i18n.js";
import { createModal } from "./modal.js";
import { el, fmtInt, fmtUsd, fmtDateMs, resetError, toast, fillStatus, escapeHtml } from "./shared.js";
import { fetchRawEvents, clearCursorLocalData, exportUsageEventsCsv } from "./usage_data.js";
import { confirmDialog } from "./accounts.js";

// 两种账单都读取本机归档。Cursor 报告实扣，OpenCode 请求日志记录成本。
const PAGE_SIZE = 300; // 分批渲染，避免几千行一次性插入卡顿

const state = {
  accountId: null,
  kind: "cursor",
  emptyMessage: "",
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
  if (!ev.pricedAs) return `<span class="tag unpriced">${escapeHtml(translate("rawEvents.unpriced"))}</span>`;
  const pricedAs = escapeHtml(ev.pricedAs);
  // 归一名与计价键不同（如 grok-4.6-fast 按 grok-4.6 计价）时把归一名也标出来
  const shown =
    ev.displayModel && ev.displayModel !== ev.pricedAs ? `<span class="raw-display">${escapeHtml(ev.displayModel)}</span>` : "";
  return `${shown}<span class="tag">${pricedAs}</span>`;
}

function subline(text) {
  return `<small class="raw-subline" title="${escapeHtml(text)}">${escapeHtml(text)}</small>`;
}

function recordedUsd(value) {
  if (value == null || !Number.isFinite(value)) return "—";
  if (value !== 0 && Math.abs(value) < 0.000001) {
    return "$" + value.toLocaleString(locale(), { maximumSignificantDigits: 6, useGrouping: false });
  }
  return "$" + value.toLocaleString(locale(), { minimumFractionDigits: 2, maximumFractionDigits: 8 });
}

function rowHtml(ev) {
  const openCode = state.kind === "opencode";
  const time = ev.timestampMs != null ? fmtDateMs(ev.timestampMs) : "—";
  return `<tr>
    <td class="raw-time">${escapeHtml(time)}${openCode ? subline(ev.requestId || "—") : ""}</td>
    <td class="raw-model" title="${escapeHtml(ev.model)}">${escapeHtml(ev.model)}${openCode ? subline(ev.product) : ""}</td>
    <td>${priceTag(ev)}</td>
    <td class="num">${fmtInt(ev.inputTokens)}</td>
    <td class="num">${fmtInt(ev.cacheReadTokens)}</td>
    <td class="num">${fmtInt(ev.cacheWriteTokens)}${openCode ? subline(translate("rawEvents.1HPortion") + fmtInt(ev.cacheWrite1hTokens)) : ""}</td>
    <td class="num">${fmtInt(ev.outputTokens)}${openCode ? subline(translate("rawEvents.reasoningPortion") + fmtInt(ev.reasoningTokens)) : ""}</td>
    <td class="num">${fmtInt(ev.totalTokens)}</td>
    <td class="num">${openCode ? recordedUsd(ev.reportedCostUsd) : fmtUsd(ev.actualUsd)}</td>
    <td class="num">${openCode ? recordedUsd(ev.equivalentUsd) : fmtUsd(ev.equivalentUsd)}</td>
  </tr>`;
}

function renderMore() {
  const body = el("#raw-body");
  const next = state.filtered.slice(state.shown, state.shown + PAGE_SIZE);
  body.insertAdjacentHTML("beforeend", next.map(rowHtml).join(""));
  state.shown += next.length;
  const remaining = state.filtered.length - state.shown;
  el("#raw-more-wrap").hidden = remaining <= 0;
  el("#raw-more").textContent = translate("rawEvents.showMoreRemaining", fmtInt(remaining));
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
  empty.textContent = state.events.length ? translate("rawEvents.noMatchingEvents") : state.emptyMessage;

  const totals = state.filtered.reduce(
    (acc, ev) => {
      acc.tokens += ev.totalTokens;
      const official = state.kind === "opencode" ? ev.reportedCostUsd : ev.actualUsd;
      if (official == null) acc.unreported += 1;
      else acc.actual += official;
      if (ev.equivalentUsd != null) acc.equivalent += ev.equivalentUsd;
      else acc.unpriced += 1;
      return acc;
    },
    { tokens: 0, actual: 0, equivalent: 0, unpriced: 0, unreported: 0 }
  );
  const parts = [
    translate("rawEvents.events", fmtInt(state.filtered.length)),
    `${fmtInt(totals.tokens)} tok`,
    state.kind === "opencode"
      ? (totals.unreported ? translate("rawEvents.knownRecordedCost") : translate("rawEvents.recordedCost"))
        + recordedUsd(totals.unreported < state.filtered.length ? totals.actual : null)
      : translate("rawEvents.actualCharge") + fmtUsd(totals.actual),
    translate("rawEvents.equivalent") + (state.kind === "opencode"
      ? recordedUsd(totals.unpriced < state.filtered.length ? totals.equivalent : null) : fmtUsd(totals.equivalent)),
  ];
  if (totals.unreported) parts.push(translate("rawEvents.missingCost", fmtInt(totals.unreported)));
  if (totals.unpriced) parts.push(translate("rawEvents.unpriced2", fmtInt(totals.unpriced)));
  el("#raw-count").textContent = parts.join(" · ");
  el("#raw-export").disabled = state.filtered.length === 0;
}

function csvCell(value) {
  let text = String(value ?? "");
  if (typeof value === "string" && /^\s*[=+@-]/.test(text) && !/^[+-]?\d+(\.\d+)?$/.test(text)) {
    text = "'" + text;
  }
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

/** 导出所有匹配记录。OpenCode 保留日志中的金额精度与缺失值。 */
export function usageEventsCsv(events, kind) {
  const openCode = kind === "opencode";
  const header = [translate("common.time"), translate("rawEvents.timestampMs")];
  if (openCode) header.push(translate("rawEvents.requestId"), translate("rawEvents.product"));
  header.push(translate("rawEvents.modelReported"), translate("rawEvents.normalizedName"), translate("rawEvents.pricingKey"), translate("usage.input"), translate("usage.cacheRead"), translate("usage.cacheWrite"), translate("usage.output"), translate("rawEvents.totalTokens"));
  if (openCode) header.push(translate("rawEvents.1HCacheWriteTokens"), translate("rawEvents.reasoningOutputTokens"));
  header.push(openCode ? translate("rawEvents.reportedCostUsd") : translate("rawEvents.actualChargeUsd"), translate("rawEvents.equivalentCostUsd"));
  const lines = [header.map(csvCell).join(",")];
  for (const ev of events) {
    const row = [ev.timestampMs != null ? fmtDateMs(ev.timestampMs) : "", ev.timestampMs ?? ""];
    if (openCode) row.push(ev.requestId, ev.product);
    row.push(ev.model, ev.displayModel, ev.pricedAs, Math.round(ev.inputTokens), Math.round(ev.cacheReadTokens),
      Math.round(ev.cacheWriteTokens), Math.round(ev.outputTokens), Math.round(ev.totalTokens));
    if (openCode) row.push(ev.cacheWrite1hTokens, ev.reasoningTokens, ev.reportedCostUsd, ev.equivalentUsd);
    else row.push(ev.actualUsd.toFixed(6), ev.equivalentUsd?.toFixed(6));
    lines.push(row.map(csvCell).join(","));
  }
  return lines.join("\r\n") + "\r\n";
}

async function onExport() {
  if (!state.filtered.length) return;
  const content = usageEventsCsv(state.filtered, state.kind);
  const seq = state.loadSeq;
  const btn = el("#raw-export");
  btn.disabled = true;
  try {
    const result = await exportUsageEventsCsv(state.fileName, content);
    if (seq === state.loadSeq && result && !result.cancelled) toast("ok", () => translate("rawEvents.exported", result.path), { key: "raw-export" });
  } catch (error) {
    if (seq === state.loadSeq) setStatus("bad", () => translate("accounts.exportFailed", resetError(error)));
  } finally {
    if (seq === state.loadSeq) btn.disabled = state.filtered.length === 0;
  }
}

/** 清除在用账户的本地用量数据：确认后删事件库 + 清缓存，成功即关闭弹窗并通知调用方。 */
async function onClear() {
  if (!state.accountId || state.deleted || state.kind !== "cursor") return;
  const session = getRawModal().session;
  const ok = await confirmDialog({
    title: translate("raw.clearLocalData"),
    body: translate("accounts.clearUsageEventsAndCachedStatisticsFor", state.label),
    confirmText: translate("accounts.clear"),
    danger: true,
  });
  if (!ok || !getRawModal().isCurrent(session)) return;
  const btn = el("#raw-clear");
  btn.disabled = true;
  try {
    const result = await clearCursorLocalData(state.accountId);
    if (!getRawModal().isCurrent(session)) return;
    toast("ok", () => translate("rawEvents.clearedLocalUsageForEvents", state.label, fmtInt(result.events)), { key: "raw-clear" });
    const cb = state.onCleared;
    closeModal();
    if (typeof cb === "function") cb();
  } catch (error) {
    if (!getRawModal().isCurrent(session)) return;
    const code = errorCode(error);
    setStatus("bad", () => code === "no_usage_data" ? translate("accounts.noLocalUsageDataForThisAccount") : translate("accounts.clearFailed", resetError(code)));
  } finally {
    if (getRawModal().isCurrent(session)) btn.disabled = false;
  }
}

let rawModal;
function getRawModal() {
  return rawModal ||= createModal(el("#raw-modal"), {
    mode: () => "view",
    onClose() {
      state.loadSeq += 1; // 作废在途加载
      state.events = [];
      state.filtered = [];
      state.onCleared = null;
      el("#raw-body").replaceChildren();
    },
  });
}
function closeModal() {
  getRawModal().close();
}

function safeFileName(text) {
  return String(text).replace(/[\\/:*?"<>|\s]+/g, "_").slice(0, 60) || "cursor";
}

function renderRawView() {
  const openCode = state.kind === "opencode";
  const range = state.rangeLabel();
  state.emptyMessage = translate("rawEvents.noUsageEventsInThisRange");
  el("#raw-time-heading").textContent = openCode ? translate("rawEvents.timeRequestId") : translate("common.time");
  el("#raw-model-heading").textContent = openCode ? translate("rawEvents.modelProduct") : translate("raw.modelAsReported");
  el("#raw-cost-heading").textContent = openCode ? translate("rawEvents.reportedCost") : translate("raw.actualCharge");
  el("#raw-modal-title").textContent = translate("rawEvents.usageEvents", state.label);
  el("#raw-modal-sub").textContent = translate("rawEvents.loading", range);
  const result = state.result;
  if (!result) return;
  const synced = result.syncedAt ? translate("rawEvents.synced", fmtDateMs(result.syncedAt)) : translate("rawEvents.notSyncedYet");
  el("#raw-modal-sub").textContent =
    translate("rawEvents.archivedEvents", range, fmtInt(result.total), synced) +
    (state.deleted ? translate("rawEvents.retainedDataForADeletedAccount") : "") +
    translate("rawEvents.fromTheLocalArchiveAvailableOffline");
  if (openCode) {
    const gaps = result.coverageGaps?.length || 0;
    const notes = [];
    if (state.start == null && result.coverageStart != null) {
      el("#raw-modal-sub").textContent += translate("rawEvents.archiveStarts") + fmtDateMs(result.coverageStart);
    }
    if (!result.syncedAt) {
      state.emptyMessage = translate("rawEvents.refreshWorkspaceUsageInTheUsageTab");
      notes.push(translate("rawEvents.initialSyncIncomplete"));
    } else {
      if (gaps) notes.push(translate("rawEvents.archiveGapsInThisRange", gaps));
      if (result.pendingSince != null) notes.push(translate("rawEvents.pendingSyncAfter", fmtDateMs(result.pendingSince)));
      if (notes.length) state.emptyMessage = translate("rawEvents.noUsageEventsInTheArchivedPortion");
    }
    if (notes.length) setStatus("warn", notes.join(" · "));
  }
  el("#raw-modal-sub").title = result.path || "";
  el("#raw-search").disabled = !state.events.length;
  render();
}

/**
 * 打开原始账单弹窗并加载事件。
 * - accountId / label：账户 id 与显示名；deleted 为 true 表示已删除账户保留的数据（不可清除）；
 * - start / end：unix 毫秒闭区间（null = 不限），rangeText 为区间的可读名称；
 * - onCleared：清除本地数据成功后的回调（用量页据此重载视图）。
 */
export async function openRawEvents({ accountId, label, kind = "cursor", deleted = false, start = null, end = null, rangeText = () => translate("usage.allTime"), onCleared = null }) {
  const openCode = kind === "opencode";
  state.accountId = accountId;
  state.kind = kind;
  state.start = start;
  state.result = null;
  state.rangeLabel = typeof rangeText === "function" ? rangeText : () => rangeText;
  state.label = label;
  state.deleted = !!deleted;
  state.onCleared = onCleared;
  state.fileName = `${kind}-usage_${safeFileName(label)}_${safeFileName(state.rangeLabel())}.csv`;
  state.events = [];
  state.filtered = [];
  const seq = ++state.loadSeq;

  renderRawView();
  el("#raw-search").value = "";
  el("#raw-search").disabled = true;
  el("#raw-body").replaceChildren();
  el("#raw-empty").hidden = true;
  el("#raw-more-wrap").hidden = true;
  el("#raw-count").textContent = "";
  el("#raw-export").disabled = true;
  el("#raw-clear").hidden = openCode || state.deleted;
  el("#raw-clear").disabled = false;
  clearStatus();
  getRawModal().open();

  try {
    const result = await fetchRawEvents(accountId, { start, end, kind });
    if (seq !== state.loadSeq) return;
    state.events = Array.isArray(result.events) ? result.events : [];
    state.result = result;
    renderRawView();
  } catch (error) {
    if (seq !== state.loadSeq) return;
    const code = errorCode(error);
    el("#raw-modal-sub").textContent = state.rangeLabel();
    if (code === "no_usage_data") {
      el("#raw-empty").hidden = false;
      el("#raw-empty").textContent = translate("rawEvents.thisAccountHasNoLocalEventArchive");
      el("#raw-clear").hidden = true;
    } else {
      setStatus("bad", () => translate("rawEvents.couldNotReadUsageEvents", resetError(code)));
    }
  }
}

export function initRawEvents() {
  window.addEventListener("uilanguagechange", () => {
    if (el("#raw-modal").hidden || !state.rangeLabel) return;
    const previous = state.shown;
    renderRawView();
    while (state.shown < previous && state.shown < state.filtered.length) renderMore();
  });
  el("#raw-search").addEventListener("input", render);
  el("#raw-more").addEventListener("click", renderMore);
  el("#raw-export").addEventListener("click", () => { void onExport(); });
  el("#raw-clear").addEventListener("click", () => { void onClear(); });
}
