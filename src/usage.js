import {
  el,
  listen,
  fmtInt,
  fmtTokens,
  fmtUsd,
  compactTokens,
  fmtShare,
  colorFor,
  hexAlpha,
  chartAnimMs,
  setChartHoverHit,
  bindChartHoverLeave,
  pieSliceLabelsPlugin,
  resetError,
  toast,
  dismissToast,
  escapeHtml,
  kindLabel,
  tickDate,
  topSlices,
} from "./shared.js";
import {
  getCachedAgg as cacheGetAgg,
  getCachedScan as cacheGetScan,
  getCachedClaudeScan as cacheGetClaudeScan,
  fetchCursorAggregate,
  fetchCodexScan,
  fetchClaudeScan,
  purgeAccountCache,
  purgeMissingAccounts,
  setUsageCacheTtlMs,
  DEFAULT_USAGE_TTL_MS,
  USAGE_CACHE_PREFIX,
  USAGE_CACHE_EVENT,
  USAGE_CACHE_ORIGIN,
  forgetUsageCacheFromEvent,
  todayRangeKey,
  dayRangeKey,
  todayStartMs,
  dayStartMs,
  localYmd,
  addLocalDays,
  parseYmd,
} from "./usage_data.js";
import { getAccounts, onAccountsChanged, refreshAccounts, getRefreshIntervalMinutes } from "./accounts.js";
import { membershipLabel, planMonthlyUsd, relativeFromUnixSeconds } from "./account_format.js";
import { generateCursorSnapshot } from "./snapshot.js";

// 用量统计：Cursor 账单数据源自「账户管理」中保存的 Cursor 账户，自动拉取，无需手动输入。
// Codex / Claude 账户不在本页展示（额度信息见「账户管理」）；它们的账单只能来自本地会话日志，
// 分别由本地扫描（CODEX_HOME / CLAUDE_CONFIG_DIR）折算，在总览中作为独立来源行参与合并。
// 视图：全部总览（各 Cursor 账户 + 本地 Codex + 本地 Claude 合并）/ 单个 Cursor 账户 /
// 本地 Codex 用量分析 / 本地 Claude 用量分析。
// 时间跨度为二级 TAB（今天 / 昨天 / 近 7 / 30 天 / 全部），默认今天；
// 「今天」用与托盘总览相同的今日缓存键（today:YYYY-MM-DD），两边数据互通；
// 「昨天」用完整自然日键（day:YYYY-MM-DD）。这两个单日跨度下按日图切换为该日 0–23 时的
// 24 小时柱（数据来自聚合结果的 hourly 序列，后端只记今天与昨天两天）。
// 所有柱图横轴统一从左到右由旧到新。
//
// 缓存策略：与托盘总览共用 usage_data.js（内存 + localStorage，键前缀 usage-cache:v4:）。
// 打开视图时先用缓存（含过期缓存）立即渲染，再在后台拉取最新数据原地刷新（stale-while-revalidate）。
//
// 刷新模型——只有两类入口会真正重新统计，其余一律只从缓存重绘：
// 1. 看数据：进入本页 / 切换数据源或跨度 / 再点当前 chip。结果区展示的正是当前视图且未过期
//    则什么都不做；否则重新加载当前视图，各来源缓存在有效期内直接命中，过期的才走网络。
//    「刷新」与本地「扫描」按钮是强制版（忽略缓存）。
// 2. 统一刷新：任何入口（账户页 / 托盘 / 本页「刷新」/ accounts.js 的定时刷新）刷新账户状态后，
//    本页检测「刚刷新过的账户」，后台按当前跨度强制预取其用量写入共享缓存；预取完成或托盘
//    写入缓存后，本页只用缓存原地重绘当前视图（rerenderFromCache），不会顺带重新统计其它来源。
//    定时刷新只有 accounts.js 一个定时器，本页不再自带定时器（同一间隔两条链会重复拉取）。

const DEFAULT_TTL_MS = DEFAULT_USAGE_TTL_MS; // 未开启定时刷新时的结果缓存有效期
const OVERVIEW_CONCURRENCY = 2;
// 账户刷新触发的用量预取：缓存比这更新鲜就跳过（防与刚完成的拉取重复走网络）
const PREFETCH_MIN_AGE_MS = 60_000;

let selection = "all"; // "all" | "local"（本地 Codex）| "local-claude" | Cursor 账户 id
let span = "today"; // 时间跨度："today" | "yesterday" | "7" | "30" | "0"（全部）
let loadSeq = 0; // 加载序号，防止过期的异步结果覆盖新视图
let loading = false;
let panelVisible = false;
let renderedFor = ""; // 结果区当前展示的 selection+range（隐藏时为空）
let renderedAt = 0; // 结果区数据的获取时间（显示「更新于」与缓存标记）
let lastAttemptAt = 0; // 最近一次完整加载的完成时间（新鲜度门控；失败来源不再把整页永久拖成过期）
let usageInterval = 0; // 定时刷新间隔（分钟，0 = 关闭），与账户状态刷新共用；本页只用它推导缓存有效期
let accountIdsSig = "";
const overviewResults = new Map(); // accountId -> { state, agg?, error? }
// accountId -> { at: lastRefreshAt, token }：检测「刚刷新过」（触发用量预取）与「凭据刚更换」（作废旧缓存）
const seenAccounts = new Map();
let seenInitialized = false; // 首批账户快照只登记不预取（启动时账户数据来自磁盘，并非刚刷新）
let rerenderTimer = null; // 预取完成 / 对端缓存写入后的重渲染合并计时器
let cacheDirty = false; // 页面隐藏期间共享缓存有过更新，下次显示先从缓存重绘

/* ---------- 图表主题 ---------- */

let tokenChart = null; // 各模型 Token 数量（环形饼图）
let modelChart = null; // 各模型等价费用（环形饼图）
let doughnutChart = null; // Token 构成
let dailyChart = null; // 按日 / 按小时柱图

function cssVar(name) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || undefined;
}
function chartTheme() {
  return {
    tick: cssVar("--chart-tick"),
    tickStrong: cssVar("--chart-tick-strong"),
    grid: cssVar("--chart-grid"),
    border: cssVar("--chart-border"),
  };
}
function applyChartTheme() {
  const t = chartTheme();
  if (tokenChart) {
    tokenChart.data.datasets[0].borderColor = t.border;
    tokenChart.options.plugins.legend.labels.color = t.tickStrong;
    tokenChart.update("none");
  }
  if (modelChart) {
    modelChart.data.datasets[0].borderColor = t.border;
    modelChart.options.plugins.legend.labels.color = t.tickStrong;
    modelChart.update("none");
  }
  if (doughnutChart) {
    doughnutChart.data.datasets[0].borderColor = t.border;
    doughnutChart.options.plugins.legend.labels.color = t.tickStrong;
    doughnutChart.update("none");
  }
  if (dailyChart) {
    dailyChart.options.scales.x.ticks.color = t.tick;
    dailyChart.options.scales.y.ticks.color = t.tickStrong;
    dailyChart.options.scales.y.grid.color = t.grid;
    dailyChart.options.plugins.legend.labels.color = t.tickStrong;
    dailyChart.$todayBandColor = todayBandColor();
    dailyChart.update("none");
  }
}
window.addEventListener("themechange", applyChartTheme);

/* ---------- 小工具 ---------- */

function setStatus(kind, text) {
  toast(kind, text, { key: "usage" });
}
function clearStatus() {
  dismissToast("usage");
}

const SPAN_LABELS = { today: "今天", yesterday: "昨天", 7: "近 7 天", 30: "近 30 天", 0: "全部" };

// 两个本地扫描来源（本地 Codex / Claude Code 会话日志）的全部差异点：总览行、图例、
// 类型标签、单来源视图的表单元素与缓存 / 拉取入口，其余逻辑按此配置驱动一份实现。
const LOCAL_SOURCES = {
  local: {
    tag: "codex",
    label: "本地 ChatGPT 分析",
    title: "本地 ChatGPT 用量分析",
    mergedName: "本地 ChatGPT",
    emptyText: "未在本地会话日志中找到用量。",
    form: "#usage-local-form",
    homeInput: "#usage-codex-home",
    scanBtn: "#usage-codex-scan",
    cachePrefix: "scan:",
    getCached: cacheGetScan,
    fetch: fetchCodexScan,
  },
  "local-claude": {
    tag: "claude",
    label: "本地 Claude 分析",
    title: "本地 Claude 用量分析",
    mergedName: "本地 Claude",
    emptyText: "未在本地 Claude Code 会话日志中找到用量。",
    form: "#usage-claude-form",
    homeInput: "#usage-claude-home",
    scanBtn: "#usage-claude-scan",
    cachePrefix: "cscan:",
    getCached: cacheGetClaudeScan,
    fetch: fetchClaudeScan,
  },
};
const LOCAL_KEYS = Object.keys(LOCAL_SOURCES);

function isLocalSelection(value = selection) {
  return Object.prototype.hasOwnProperty.call(LOCAL_SOURCES, value);
}
/** 本地来源表单里填写的会话目录（已 trim，空串表示用默认目录）。 */
function localHomeRaw(key) {
  return el(LOCAL_SOURCES[key].homeInput).value.trim();
}

function isTodaySpan() {
  return span === "today";
}
function isYesterdaySpan() {
  return span === "yesterday";
}
/** 单日跨度（今天 / 昨天）：按小时柱图、隐藏与月费对比的倍数。 */
function isSingleDaySpan() {
  return isTodaySpan() || isYesterdaySpan();
}
/** 单日跨度所展示的那一天（本地 YYYY-MM-DD）。 */
function focusYmd() {
  return localYmd(new Date(dayStartMs(isYesterdaySpan() ? -1 : 0)));
}
/** 单日跨度的缓存键：今天 today:YYYY-MM-DD（与托盘共用），昨天 day:YYYY-MM-DD（完整自然日）。 */
function singleDayKey() {
  return isYesterdaySpan() ? dayRangeKey(focusYmd()) : todayRangeKey();
}
/** Cursor 聚合的缓存键：单日跨度见 singleDayKey，其余为天数 / 0。 */
function rangeKey() {
  return isSingleDaySpan() ? singleDayKey() : span;
}
/** 本地 Codex / Claude 扫描的缓存键：单日跨度同上，其余为天数 / all。 */
function scanKey() {
  return isSingleDaySpan() ? singleDayKey() : span === "0" ? "all" : span;
}
function rangeBounds() {
  if (isTodaySpan()) return { start: todayStartMs(), end: Date.now() };
  // 昨天：[昨日 0 点, 今日 0 点)，终点退 1ms 避免把今天第一毫秒的事件算进去
  if (isYesterdaySpan()) return { start: dayStartMs(-1), end: dayStartMs(0) - 1 };
  const days = Number(span);
  if (days > 0) {
    const end = Date.now();
    return { start: end - days * 86400 * 1000, end };
  }
  return { start: null, end: null };
}
function rangeText() {
  return SPAN_LABELS[span] || "全部";
}
/** 单日跨度的说明尾巴（结果区 meta 文案）。 */
function singleDayTail() {
  return isYesterdaySpan()
    ? `柱图为昨日（${focusYmd()}）0–24 时分布`
    : "柱图为今日 0–24 时分布（当前小时高亮）";
}

function maskToken(token) {
  const t = String(token || "").trim();
  if (!t) return "—";
  return t.length > 12 ? `${t.slice(0, 8)}…` : t;
}
function accountLabel(account) {
  return account.note || `${kindLabel(account.kind)} ${maskToken(account.token)}`;
}
function currentAccount() {
  return getAccounts().find((a) => a.id === selection) || null;
}
function selectionKey() {
  if (isLocalSelection()) return `${selection}:${scanKey()}:${localHomeRaw(selection)}`;
  // 今天的键含日期（today:YYYY-MM-DD），跨零点后自动视为新视图
  return `${selection}:${rangeKey()}`;
}
/**
 * 标记结果区已渲染。at 为数据的实际获取时间（缓存数据传缓存时间），
 * 用于「更新于」文案与（缓存）标记；新鲜度门控另见 isRenderedFresh。
 */
function markUpdated(at) {
  renderedFor = selectionKey();
  renderedAt = Number.isFinite(at) && at > 0 ? at : Date.now();
  const stale = Date.now() - renderedAt >= effectiveTtlMs();
  const time = new Date(renderedAt).toLocaleTimeString("zh-CN", { hour12: false });
  el("#usage-updated").textContent = `数据更新于 ${time}${stale ? "（缓存）" : ""}`;
}

/** 结果缓存有效期：开启定时刷新后与刷新间隔保持一致。 */
function effectiveTtlMs() {
  return usageInterval > 0 ? usageInterval * 60_000 : DEFAULT_TTL_MS;
}

/**
 * 结果区是否无需重载：正在展示当前视图，且数据时间或最近一次完整加载在有效期内。
 * 门控同时看 lastAttemptAt 是关键——某来源持续失败时其数据时间永远陈旧，若只看
 * renderedAt，整页会被它拖成「永久过期」，每次进页都重新统计；改为「刚试过就不再试」，
 * 到期或手动刷新时才重试失败来源。
 */
function isRenderedFresh() {
  return (
    selectionKey() === renderedFor &&
    Date.now() - Math.max(renderedAt, lastAttemptAt) < effectiveTtlMs()
  );
}

/**
 * 与持久化的定时刷新设置同步（初次加载 / 设置弹窗修改时）：只更新缓存有效期。
 * 定时重新统计本身由 accounts.js 的定时器驱动——账户状态刷新完成后经 prefetchUsageFor
 * 预取用量，本页不再自带第二个定时器。
 */
function syncUsageInterval(minutes) {
  const n = Number(minutes);
  if (!Number.isFinite(n) || n === usageInterval) return;
  usageInterval = n;
  setUsageCacheTtlMs(usageInterval > 0 ? usageInterval * 60_000 : DEFAULT_TTL_MS);
}

/* ---------- 结果区渲染（卡片 / 图表 / 明细表） ---------- */

function tokenCostCell(tokens, usd, priced) {
  const count = fmtTokens(tokens);
  const full = fmtInt(tokens);
  if (!priced || !(tokens > 0)) return `<td class="num" title="${full}">${count}</td>`;
  return `<td class="num" title="${full}">${count}<span class="cell-cost">${fmtUsd(usd)}</span></td>`;
}

function renderBreakdown(agg) {
  const box = el("#usage-breakdown");
  const items = [
    ["输入", agg.totalInputUsd],
    ["缓存读", agg.totalCacheReadUsd],
    ["缓存写", agg.totalCacheWriteUsd],
    ["输出", agg.totalOutputUsd],
  ];
  box.hidden = false;
  box.innerHTML =
    `<span class="cost-breakdown-label">等价费用构成</span>` +
    items
      .map(
        ([label, usd]) =>
          `<span class="cost-chip"><span class="cost-chip-label">${label}</span> ${fmtUsd(usd || 0)}</span>`
      )
      .join("");
}

let lastAggRender = null; // 单位切换时用当前数据即时重绘

/**
 * 渲染结果区（卡片 / 图表 / 明细表）。
 * opts.plan = { monthlyUsd, equivalentUsd } 时显示「套餐月费 · 等价倍数」卡片，
 * equivalentUsd 为参与对比的等价费用（总览视图仅计入可定价套餐的 Cursor 账户）。
 * opts.dailySources 为按日 / 按小时堆叠柱的各来源（总览按账户分段；缺省则用 agg 单列）。
 * 各模型等价费用饼图直接取合并后的 agg.models，不区分来源。
 */
function renderAggregate(agg, { showActual, metaText, plan, dailySources }) {
  const sources =
    dailySources != null
      ? dailySources
      : [{ label: "用量", daily: agg.daily || [], hourly: agg.hourly || [], showActual: !!showActual }];
  lastAggRender = { agg, opts: { showActual, metaText, plan, dailySources: sources } };
  el("#usage-skeleton").hidden = true;
  el("#usage-results").hidden = false;
  el("#sum-equivalent").textContent = fmtUsd(agg.totalEquivalentUsd);
  el("#card-actual").hidden = !showActual;
  el("#sum-actual").textContent = showActual ? fmtUsd(agg.totalActualUsd) : "—";
  const planCard = el("#card-plan");
  if (plan && plan.monthlyUsd != null) {
    planCard.hidden = false;
    const ratio = plan.monthlyUsd > 0 ? plan.equivalentUsd / plan.monthlyUsd : null;
    el("#sum-plan").textContent = `$${plan.monthlyUsd} · ${ratio != null ? `${ratio.toFixed(1)}×` : "—"}`;
    el("#sum-plan").title = ratio != null ? `所选范围等价 API 费用为套餐月费的 ${ratio.toFixed(2)} 倍` : "";
  } else {
    planCard.hidden = true;
  }
  el("#sum-tokens").textContent = fmtTokens(agg.totalTokens);
  el("#sum-tokens").title = fmtInt(agg.totalTokens);
  el("#sum-unpriced").textContent = `${agg.unpricedModels} 个 / ${fmtTokens(agg.unpricedTokens)} tok`;
  el("#usage-meta").textContent = metaText || "";

  const body = el("#usage-body");
  body.replaceChildren();
  for (const m of agg.models) {
    const tr = document.createElement("tr");
    const priceTag = m.priced
      ? `<span class="tag">${m.pricedAs}</span>`
      : `<span class="tag unpriced">未定价</span>`;
    tr.innerHTML = `
      <td>${escapeHtml(m.model)} ${priceTag}</td>
      ${tokenCostCell(m.inputTokens, m.inputUsd, m.priced)}
      ${tokenCostCell(m.cacheReadTokens, m.cacheReadUsd, m.priced)}
      ${tokenCostCell(m.cacheWriteTokens, m.cacheWriteUsd, m.priced)}
      ${tokenCostCell(m.outputTokens, m.outputUsd, m.priced)}
      <td class="num" title="${fmtInt(m.totalTokens)}">${fmtTokens(m.totalTokens)}</td>
      <td class="num">${showActual ? fmtUsd(m.actualUsd) : "—"}</td>
      <td class="num">${m.priced ? fmtUsd(m.equivalentUsd) : "—"}</td>`;
    body.append(tr);
  }
  renderBreakdown(agg);
  renderCharts(agg, sources);
}

/** 按日图的横轴天数（仅多日跨度使用；今天 / 昨天走 24h 小时图）。全部 = 0（从最早日期起）。 */
function chartRangeDays() {
  const n = Number(span);
  return Number.isFinite(n) && n > 0 ? n : 0;
}
/** 按日图横轴日期，从左到右由旧到新（今天在最右），与小时图 0:00 → 23:00 的方向一致。 */
function dailyAxisLabels(sources) {
  const days = chartRangeDays();
  const now = new Date();
  const today0 = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  if (days > 0) {
    const labels = [];
    for (let i = days - 1; i >= 0; i -= 1) labels.push(localYmd(addLocalDays(today0, -i)));
    return labels;
  }
  let min = null;
  for (const s of sources || []) {
    for (const d of s.daily || []) {
      if (d && d.date && (!min || d.date < min)) min = d.date;
    }
  }
  if (!min) {
    const labels = [];
    for (let i = 6; i >= 0; i -= 1) labels.push(localYmd(addLocalDays(today0, -i)));
    return labels;
  }
  const labels = [];
  let cur = parseYmd(min);
  while (cur.getTime() <= today0.getTime() && labels.length < 3660) {
    labels.push(localYmd(cur));
    cur = addLocalDays(cur, 1);
  }
  return labels;
}
function titleDate(ymd) {
  const p = String(ymd).split("-");
  if (p.length !== 3) return ymd;
  return `${p[0]}年${Number(p[1])}月${Number(p[2])}日`;
}

function updateDailyHover(chart, hit) {
  setChartHoverHit(chart, hit, { stroke: chartTheme().tickStrong, hitBorder: 2 });
}

/**
 * 「今天」跨度下在 24h 小时图给当前小时列画背景带作高亮。索引挂在 chart.$todayIndex
 * （-1 / null 不画），颜色取 chart.$todayBandColor；独立于数据集配色，
 * 与悬停变暗逻辑（applyChartHoverDim 重建 backgroundColor）互不干扰。
 */
const todayBandPlugin = {
  id: "todayBand",
  beforeDatasetsDraw(chart) {
    const idx = chart.$todayIndex;
    if (idx == null || idx < 0) return;
    const x = chart.scales.x;
    const area = chart.chartArea;
    if (!x || !area) return;
    const labels = chart.data.labels || [];
    const step = labels.length > 1 ? Math.abs(x.getPixelForValue(1) - x.getPixelForValue(0)) : x.width;
    if (!Number.isFinite(step) || step <= 0) return;
    const cx = x.getPixelForValue(idx);
    const { ctx } = chart;
    ctx.save();
    ctx.fillStyle = chart.$todayBandColor || "rgba(110, 168, 254, 0.10)";
    ctx.fillRect(cx - step / 2, area.top, step, area.bottom - area.top);
    ctx.restore();
  },
};

function todayBandColor() {
  const accent = cssVar("--accent");
  return accent ? hexAlpha(accent, 0.12) : "rgba(110, 168, 254, 0.10)";
}

/**
 * 总览各来源（Cursor 账户 + 本地 Codex / Claude 分析）的统一顺序：按当前已有数据的
 * 总 Token 降序，无数据的来源垫底（相互间保持账户原顺序）。总览表行、按日堆叠图
 * 与模型柱图都按此顺序渲染，保证行序与两张图的账户配色一一对应。
 */
function overviewSourceOrder() {
  const entries = [];
  for (const a of getAccounts().filter((x) => x.kind === "cursor")) {
    const r = overviewResults.get(a.id) || null;
    entries.push({ kind: "cursor", account: a, result: r, tokens: r && r.agg ? r.agg.totalTokens : 0 });
  }
  for (const key of LOCAL_KEYS) {
    const local = overviewResults.get(key) || null;
    const scanAgg = local && local.scan ? local.scan.aggregate : null;
    entries.push({ kind: key, account: null, result: local, tokens: scanAgg ? scanAgg.totalTokens : 0 });
  }
  entries.sort((a, b) => b.tokens - a.tokens);
  return entries;
}

function collectDailySources() {
  const sources = [];
  for (const src of overviewSourceOrder()) {
    if (src.kind === "cursor") {
      const r = src.result;
      if (!r || !r.agg) continue;
      sources.push({
        label: src.account.note || maskToken(src.account.token),
        daily: r.agg.daily || [],
        hourly: r.agg.hourly || [],
        showActual: true,
      });
    } else {
      const scanAgg = src.result && src.result.scan ? src.result.scan.aggregate : null;
      if (scanAgg) {
        sources.push({
          label: LOCAL_SOURCES[src.kind].label,
          daily: scanAgg.daily || [],
          hourly: scanAgg.hourly || [],
          showActual: false,
        });
      }
    }
  }
  return sources;
}

function renderDailyChart(sources) {
  const t = chartTheme();
  const list = Array.isArray(sources) ? sources : [];
  // 单日跨度（今天 / 昨天）渲染该日 24 小时柱，0:00 → 23:00 从左到右（今天未到时段留空），
  // 其余跨度按日渲染（同样从左到右由旧到新）；两种模式共用同一图表实例，切换时原地更新。
  const hourlyMode = isSingleDaySpan();
  const dayLabel = isYesterdaySpan() ? "昨天" : "今天";
  el("#chart-daily-title").textContent = hourlyMode ? `按小时 Token（${dayLabel}）` : "按日 Token";
  let hours = null;
  let labels;
  if (hourlyMode) {
    hours = [];
    for (let h = 0; h <= 23; h += 1) hours.push(h);
    labels = hours.map((h) => `${h}:00`);
  } else {
    labels = dailyAxisLabels(list);
  }
  const ymd = focusYmd();
  const baseColors = list.map((_, i) => colorFor(i));
  const meta = [];
  const showActual = [];
  const datasets = list.map((s, i) => {
    let rows;
    if (hourlyMode) {
      const byHour = new Map(
        (s.hourly || []).filter((r) => r && r.date === ymd).map((r) => [Number(r.hour), r])
      );
      rows = hours.map((h) => byHour.get(h) || null);
    } else {
      const byDate = new Map((s.daily || []).map((d) => [d.date, d]));
      rows = labels.map((label) => byDate.get(label) || null);
    }
    meta.push(rows);
    showActual.push(!!s.showActual);
    return {
      label: s.label,
      data: rows.map((r) => (r && r.tokens > 0 ? r.tokens : null)),
      backgroundColor: baseColors[i],
      hoverBackgroundColor: baseColors[i],
      borderColor: baseColors[i],
      borderWidth: 0,
      borderRadius: 2,
      maxBarThickness: 42,
      stack: "daily",
      skipNull: true,
    };
  });
  const motion = chartAnimMs(600);
  const colorMs = chartAnimMs(150);
  const attachMeta = (chart) => {
    chart.$baseColors = baseColors;
    chart.$dailyMeta = meta;
    chart.$showActual = showActual;
    chart.$hoverKey = "";
    chart.$hourly = hourlyMode;
    chart.$dayLabel = dayLabel;
    // 高亮带：只有「今天」的小时图标记当前小时列（昨天已是完整的一天，按日图不高亮）
    chart.$todayIndex = hourlyMode && isTodaySpan() && hours ? hours.indexOf(new Date().getHours()) : -1;
    chart.$todayBandColor = todayBandColor();
  };

  if (dailyChart) {
    dailyChart.data.labels = labels;
    dailyChart.data.datasets = datasets;
    attachMeta(dailyChart);
    dailyChart.options.animation = { duration: motion, easing: "easeOutQuart" };
    dailyChart.update();
    return;
  }

  dailyChart = new Chart(el("#chart-daily"), {
    type: "bar",
    data: { labels, datasets },
    plugins: [todayBandPlugin],
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: "nearest", intersect: true, axis: "xy" },
      animation: { duration: motion, easing: "easeOutQuart" },
      animations: {
        colors: { duration: colorMs, easing: "easeOutQuad" },
        borderWidth: { duration: colorMs },
      },
      plugins: {
        legend: {
          position: "bottom",
          labels: { color: t.tickStrong, boxWidth: 10, boxHeight: 10 },
          onHover(event, item, legend) {
            if (event.native && event.native.target) event.native.target.style.cursor = "pointer";
            updateDailyHover(legend.chart, { datasetIndex: item.datasetIndex, index: -1 });
          },
          onLeave(_event, _item, legend) {
            updateDailyHover(legend.chart, null);
          },
        },
        tooltip: {
          position: "nearest",
          filter: (item) => item.raw != null && item.raw > 0,
          callbacks: {
            title(items) {
              if (!items.length) return "";
              const chart = items[0].chart;
              const label = chart.data.labels[items[0].dataIndex];
              if (chart.$hourly) {
                const h = parseInt(label, 10);
                return Number.isFinite(h) ? `${chart.$dayLabel || "今天"} ${h}:00 – ${h + 1}:00` : String(label);
              }
              return titleDate(label);
            },
            label(ctx) {
              if (ctx.raw == null) return null;
              return ` ${ctx.dataset.label}: ${fmtTokens(ctx.raw)}`;
            },
            afterLabel(ctx) {
              const row =
                ctx.chart.$dailyMeta && ctx.chart.$dailyMeta[ctx.datasetIndex]
                  ? ctx.chart.$dailyMeta[ctx.datasetIndex][ctx.dataIndex]
                  : null;
              if (!row) return "";
              const lines = [`等价 ${fmtUsd(row.equivalentUsd || 0)}`];
              if (ctx.chart.$showActual && ctx.chart.$showActual[ctx.datasetIndex] && row.actualUsd > 0) {
                lines.push(`实扣 ${fmtUsd(row.actualUsd)}`);
              }
              return lines;
            },
            footer(items) {
              if (!items.length) return "";
              const chart = items[0].chart;
              const idx = items[0].dataIndex;
              let sum = 0;
              for (const ds of chart.data.datasets) {
                const v = ds.data[idx];
                if (typeof v === "number") sum += v;
              }
              return `${chart.$hourly ? "该小时合计" : "当日合计"} ${fmtTokens(sum)}`;
            },
          },
        },
      },
      onHover(_event, elements, chart) {
        if (chart.canvas) chart.canvas.style.cursor = elements.length ? "pointer" : "default";
        const hit = elements[0];
        updateDailyHover(chart, hit ? { datasetIndex: hit.datasetIndex, index: hit.index } : null);
      },
      scales: {
        x: {
          stacked: true,
          ticks: {
            color: t.tick,
            maxRotation: 0,
            autoSkip: true,
            maxTicksLimit: 12,
            callback(value) {
              return tickDate(this.getLabelForValue(value));
            },
          },
          grid: { display: false },
        },
        y: {
          stacked: true,
          beginAtZero: true,
          ticks: {
            color: t.tickStrong,
            callback: (value) => fmtTokens(value),
          },
          grid: { color: t.grid },
        },
      },
    },
  });
  attachMeta(dailyChart);
  bindChartHoverLeave(dailyChart);
}

/** 销毁全部图表实例并清掉画布上的孤儿注册。
 *  Chart 构造中途失败时实例已注册到画布、模块变量却为 null，之后每次
 *  new Chart 都会抛「Canvas is already in use」；这里连孤儿一起清理才能重建。 */
function destroyUsageCharts() {
  for (const chart of [dailyChart, tokenChart, modelChart, doughnutChart]) {
    if (chart) {
      try { chart.destroy(); } catch { /* ignore */ }
    }
  }
  dailyChart = null;
  tokenChart = null;
  modelChart = null;
  doughnutChart = null;
  if (typeof Chart === "undefined" || typeof Chart.getChart !== "function") return;
  for (const id of ["chart-daily", "chart-tokens", "chart-models", "chart-doughnut"]) {
    const orphan = Chart.getChart(id);
    if (orphan) {
      try { orphan.destroy(); } catch { /* ignore */ }
    }
  }
}

/**
 * 图表渲染的防护壳：图表异常绝不能中断统计流程（否则各来源拉取根本不会
 * 发起，状态列永远停在「统计中…」）。失败时销毁并重建一次，仍失败则本轮
 * 放弃图表，卡片与明细表不受影响。
 */
function renderCharts(agg, dailySources) {
  try {
    renderChartsInner(agg, dailySources);
  } catch (error) {
    console.error("图表渲染失败，重置实例后重试：", error);
    destroyUsageCharts();
    try {
      renderChartsInner(agg, dailySources);
    } catch (retryError) {
      console.error("图表重建仍失败，本轮跳过图表：", retryError);
      destroyUsageCharts();
    }
  }
}

// 图表实例常驻，重复渲染时原地更新数据（渐进合并时不闪烁）
function renderChartsInner(agg, dailySources) {
  renderDailyChart(dailySources);
  const t = chartTheme();

  // 各模型 Token 数量饼图：Top 8 + 其他；未定价模型也计入
  const tokenSlices = topSlices(agg.models, "totalTokens", 8);
  const tokenLabels = tokenSlices.map((s) => s.label);
  const tokenData = tokenSlices.map((s) => Math.round(s.value));
  const tokenColors = tokenSlices.map((_, i) => colorFor(i));
  if (tokenChart) {
    tokenChart.data.labels = tokenLabels;
    tokenChart.data.datasets[0].data = tokenData;
    tokenChart.data.datasets[0].backgroundColor = tokenColors;
    tokenChart.update();
  } else {
    tokenChart = new Chart(el("#chart-tokens"), {
      type: "doughnut",
      data: {
        labels: tokenLabels,
        datasets: [
          {
            data: tokenData,
            backgroundColor: tokenColors,
            borderColor: t.border,
            borderWidth: 2,
          },
        ],
      },
      plugins: [pieSliceLabelsPlugin],
      options: {
        responsive: true,
        maintainAspectRatio: false,
        plugins: {
          legend: { position: "right", labels: { color: t.tickStrong, boxWidth: 12, boxHeight: 12 } },
          tooltip: {
            callbacks: {
              label(ctx) {
                const total = ctx.dataset.data.reduce((sum, v) => sum + (Number(v) || 0), 0);
                const share = fmtShare(ctx.parsed, total);
                return ` ${ctx.label}: ${fmtTokens(ctx.parsed)}${share ? `（${share}）` : ""}`;
              },
            },
          },
        },
      },
    });
    // 扇区上标注占比 + 紧凑 token 数；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    tokenChart.$pieSliceLabels = {
      formatter: (value, share) => [share, compactTokens(value)],
    };
  }

  // 各模型等价费用饼图：Top 8 + 其他；仅统计已定价（费用 > 0）的模型
  const costSlices = topSlices(agg.models, "equivalentUsd", 8);
  const modelLabels = costSlices.map((s) => s.label);
  const modelData = costSlices.map((s) => Number(s.value.toFixed(4)));
  const modelColors = costSlices.map((_, i) => colorFor(i));
  if (modelChart) {
    modelChart.data.labels = modelLabels;
    modelChart.data.datasets[0].data = modelData;
    modelChart.data.datasets[0].backgroundColor = modelColors;
    modelChart.update();
  } else {
    modelChart = new Chart(el("#chart-models"), {
      type: "doughnut",
      data: {
        labels: modelLabels,
        datasets: [
          {
            data: modelData,
            backgroundColor: modelColors,
            borderColor: t.border,
            borderWidth: 2,
          },
        ],
      },
      plugins: [pieSliceLabelsPlugin],
      options: {
        responsive: true,
        maintainAspectRatio: false,
        plugins: {
          legend: { position: "right", labels: { color: t.tickStrong, boxWidth: 12, boxHeight: 12 } },
          tooltip: {
            callbacks: {
              label(ctx) {
                const total = ctx.dataset.data.reduce((sum, v) => sum + (Number(v) || 0), 0);
                const share = fmtShare(ctx.parsed, total);
                return ` ${ctx.label}: ${fmtUsd(ctx.parsed)}${share ? `（${share}）` : ""}`;
              },
            },
          },
        },
      },
    });
    // 扇区上标注占比 + 金额；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    modelChart.$pieSliceLabels = {
      formatter: (value, share) => [share, fmtUsd(value)],
    };
  }

  const sums = agg.models.reduce(
    (acc, m) => {
      acc.input += m.inputTokens;
      acc.cacheRead += m.cacheReadTokens;
      acc.cacheWrite += m.cacheWriteTokens;
      acc.output += m.outputTokens;
      return acc;
    },
    { input: 0, cacheRead: 0, cacheWrite: 0, output: 0 }
  );
  const doughnutData = [sums.input, sums.cacheRead, sums.cacheWrite, sums.output];
  if (doughnutChart) {
    doughnutChart.data.datasets[0].data = doughnutData;
    doughnutChart.update();
  } else {
    doughnutChart = new Chart(el("#chart-doughnut"), {
      type: "doughnut",
      data: {
        labels: ["输入", "缓存读", "缓存写", "输出"],
        datasets: [
          {
            data: doughnutData,
            backgroundColor: [colorFor(0), colorFor(2), colorFor(4), colorFor(1)],
            borderColor: t.border,
            borderWidth: 2,
          },
        ],
      },
      plugins: [pieSliceLabelsPlugin],
      options: {
        responsive: true,
        maintainAspectRatio: false,
        plugins: {
          // 图例放右侧，饼图主体尽量占满定高容器
          legend: { position: "right", labels: { color: t.tickStrong, boxWidth: 12, boxHeight: 12 } },
          tooltip: { callbacks: { label: (ctx) => `${ctx.label}: ${fmtTokens(ctx.parsed)}` } },
        },
      },
    });
    // 扇区上标注占比 + 紧凑 token 数；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    doughnutChart.$pieSliceLabels = {
      formatter: (value, share) => [share, compactTokens(value)],
    };
  }
}

/* ---------- 数据源选择（chips） ---------- */

function rebuildChips() {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  if (selection !== "all" && !isLocalSelection() && !cursorAccounts.some((a) => a.id === selection)) {
    selection = "all";
  }
  const chips = [{ value: "all", label: "全部总览", kind: null }];
  for (const a of cursorAccounts) chips.push({ value: a.id, label: accountLabel(a), kind: a.kind });
  for (const key of LOCAL_KEYS) chips.push({ value: key, label: "本地用量分析", kind: LOCAL_SOURCES[key].tag });

  el("#usage-sources").replaceChildren(
    ...chips.map((c) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `chip-select${selection === c.value ? " active" : ""}`;
      if (c.kind) {
        const tag = document.createElement("span");
        tag.className = `tag kind-${c.kind}`;
        tag.textContent = kindLabel(c.kind);
        btn.append(tag);
      }
      const label = document.createElement("span");
      label.textContent = c.label;
      btn.append(label);
      btn.addEventListener("click", () => selectSource(c.value));
      return btn;
    })
  );
}

function applyVisibility() {
  for (const key of LOCAL_KEYS) el(LOCAL_SOURCES[key].form).hidden = selection !== key;
  el("#usage-overview").hidden = selection !== "all";
  // 「生成快照」仅对单个 Cursor 账户视图开放（总览 / 本地分析无对应存档口径）
  el("#usage-snapshot").hidden = selection === "all" || isLocalSelection();
  el("#usage-results").hidden = true;
  el("#usage-skeleton").hidden = true;
  renderedFor = ""; // 结果区已被隐藏，需要重新渲染
  lastAttemptAt = 0;
  clearStatus();
}

function selectSource(value) {
  if (selection === value) {
    loadView(false);
    return;
  }
  selection = value;
  rebuildChips();
  applyVisibility();
  loadView(false);
}

/* ---------- 数据加载与持久化缓存（与托盘总览共用 usage_data.js） ---------- */

function getCachedAgg(account) {
  return cacheGetAgg(account.id, rangeKey());
}

/** 本地来源在当前跨度下的缓存条目；home 为空时用默认目录（键中为空串）。 */
function getCachedLocal(key, home) {
  return LOCAL_SOURCES[key].getCached(scanKey(), home);
}

function fetchAggregate(account, force) {
  const { start, end } = rangeBounds();
  return fetchCursorAggregate(account, rangeKey(), { start, end, force });
}

/**
 * 当前跨度对应的本地扫描参数：今天只给起点（fetch 层落到 today: 键），
 * 昨天给起点 + 终点（落到 day: 键），其余按天数。
 */
function scanParams(home, force) {
  if (isTodaySpan()) return { sinceMs: todayStartMs(), home, force };
  if (isYesterdaySpan()) return { sinceMs: dayStartMs(-1), untilMs: dayStartMs(0), home, force };
  return { days: span === "0" ? null : Number(span), home, force };
}

function fetchLocal(key, home, force) {
  return LOCAL_SOURCES[key].fetch(scanParams(home, force));
}

/** 合并多个账户的聚合结果（同模型逐项累加）。 */
function mergeAggregates(aggs) {
  const models = new Map();
  for (const agg of aggs) {
    for (const m of agg.models || []) {
      const cur = models.get(m.model);
      if (!cur) {
        models.set(m.model, { ...m });
        continue;
      }
      cur.events += m.events;
      cur.inputTokens += m.inputTokens;
      cur.outputTokens += m.outputTokens;
      cur.cacheReadTokens += m.cacheReadTokens;
      cur.cacheWriteTokens += m.cacheWriteTokens;
      cur.totalTokens += m.totalTokens;
      cur.actualUsd += m.actualUsd;
      cur.equivalentUsd += m.equivalentUsd;
      cur.inputUsd += m.inputUsd;
      cur.outputUsd += m.outputUsd;
      cur.cacheReadUsd += m.cacheReadUsd;
      cur.cacheWriteUsd += m.cacheWriteUsd;
      cur.priced = cur.priced || m.priced;
    }
  }
  const list = [...models.values()].sort(
    (x, y) => y.equivalentUsd - x.equivalentUsd || y.totalTokens - x.totalTokens
  );
  const sum = (fn) => list.reduce((acc, m) => acc + fn(m), 0);
  const unpriced = list.filter((m) => !m.priced);
  return {
    models: list,
    totalEquivalentUsd: sum((m) => m.equivalentUsd),
    totalActualUsd: sum((m) => m.actualUsd),
    totalTokens: sum((m) => m.totalTokens),
    pricedModels: list.length - unpriced.length,
    unpricedModels: unpriced.length,
    unpricedTokens: unpriced.reduce((acc, m) => acc + m.totalTokens, 0),
    totalInputUsd: sum((m) => m.inputUsd),
    totalOutputUsd: sum((m) => m.outputUsd),
    totalCacheReadUsd: sum((m) => m.cacheReadUsd),
    totalCacheWriteUsd: sum((m) => m.cacheWriteUsd),
  };
}

function renderOverviewTable() {
  const body = el("#usage-overview-body");
  body.replaceChildren();
  const numTd = (text, title) => {
    const td = document.createElement("td");
    td.className = "num";
    td.textContent = text;
    if (title) td.title = title;
    return td;
  };

  const totals = { tokens: 0, actual: 0, equivalent: 0, planUsd: 0, planEquiv: 0, planKnown: false };
  let hasData = false;

  // 一行一个来源：各 Cursor 账户 + 本地 Codex 分析，按总 Token 降序；
  // showActual=false 的来源实扣列恒为 —。统计失败但有缓存数据的来源仍显示旧数字（状态列展示错误）。
  // 「数据更新」列为该来源用量数据的获取时间——各来源缓存时间可能不同，逐行展示。
  const sourceRow = ({ kind, label, stateText, stateBad, at, agg, showActual, planText, ratioText }) => {
    const tr = document.createElement("tr");
    const nameTd = document.createElement("td");
    const ident = document.createElement("div");
    ident.className = "account-ident";
    const tag = document.createElement("span");
    tag.className = `tag kind-${kind}`;
    tag.textContent = kindLabel(kind);
    const name = document.createElement("span");
    name.className = "account-note";
    name.textContent = label;
    ident.append(tag, name);
    nameTd.append(ident);

    const stateTd = document.createElement("td");
    stateTd.textContent = stateText;
    if (stateBad) {
      stateTd.className = "cell-bad";
      stateTd.title = stateText;
    }

    const timeTd = document.createElement("td");
    if (Number.isFinite(at) && at > 0) {
      // 原始时间戳存 dataset，供定时器重算相对文案（否则文本会冻结在渲染时刻，如一直显示「刚刚」）
      timeTd.className = "usage-data-at";
      timeTd.dataset.at = String(at);
      timeTd.textContent = relativeFromUnixSeconds(at / 1000);
      timeTd.title = new Date(at).toLocaleString("zh-CN", { hour12: false });
    } else {
      timeTd.textContent = "—";
    }

    const planTd = document.createElement("td");
    planTd.textContent = planText || "—";

    if (agg) {
      totals.tokens += agg.totalTokens;
      totals.actual += showActual ? agg.totalActualUsd : 0;
      totals.equivalent += agg.totalEquivalentUsd;
      hasData = true;
      tr.append(
        nameTd,
        stateTd,
        timeTd,
        planTd,
        numTd(fmtTokens(agg.totalTokens), fmtInt(agg.totalTokens)),
        numTd(showActual ? fmtUsd(agg.totalActualUsd) : "—"),
        numTd(fmtUsd(agg.totalEquivalentUsd)),
        numTd(ratioText || "—")
      );
    } else {
      tr.append(nameTd, stateTd, timeTd, planTd, numTd("—"), numTd("—"), numTd("—"), numTd("—"));
    }
    body.append(tr);
  };

  for (const src of overviewSourceOrder()) {
    if (src.kind === "cursor") {
      const a = src.account;
      const result = src.result;
      const agg = result && result.agg ? result.agg : null;
      let stateText = "—";
      let stateBad = false;
      if (result && result.state === "pending") {
        stateText = agg ? "更新中…" : "统计中…";
      } else if (result && result.state === "error") {
        stateText = agg ? `更新失败：${result.error}` : result.error;
        stateBad = true;
      } else if (result && result.state === "ok") {
        stateText = "完成";
      }
      // 套餐列：套餐名 + 月费；倍数列：等价费用 ÷ 月费（月费未知或为 0 时为 —；
      // 单日跨度（今天 / 昨天）下单日费用对比月费无意义，恒为 —）
      const membership = a.status ? a.status.membershipType : null;
      const planLabel = membership ? membershipLabel(membership) : "";
      const price = planMonthlyUsd(membership);
      const planText = planLabel ? (price != null ? `${planLabel} · $${price}` : planLabel) : "—";
      let ratioText = "—";
      if (agg && price > 0 && !isSingleDaySpan()) ratioText = `${(agg.totalEquivalentUsd / price).toFixed(1)}×`;
      if (agg && price != null) {
        totals.planUsd += price;
        totals.planEquiv += agg.totalEquivalentUsd;
        totals.planKnown = true;
      }
      sourceRow({
        kind: "cursor",
        label: a.note || maskToken(a.token),
        stateText,
        stateBad,
        at: result ? result.at : null,
        agg,
        showActual: true,
        planText,
        ratioText,
      });
    } else {
      // 本地 Codex / Claude 分析行：无论有无对应账户都展示；本地日志无账户归属，无套餐可比
      const local = src.result;
      const localAgg = local && local.scan ? local.scan.aggregate : null;
      let localState = "—";
      let localBad = false;
      if (local && local.state === "pending") {
        localState = localAgg ? "更新中…" : "统计中…";
      } else if (local && local.state === "error") {
        localState = localAgg ? `更新失败：${local.error}` : local.error;
        localBad = true;
      } else if (local && local.state === "ok") {
        localState = `${local.scan.sessions} 个会话`;
      }
      sourceRow({
        kind: LOCAL_SOURCES[src.kind].tag,
        label: LOCAL_SOURCES[src.kind].label,
        stateText: localState,
        stateBad: localBad,
        at: local ? local.at : null,
        agg: localAgg,
        showActual: false,
        planText: "—",
        ratioText: "—",
      });
    }
  }

  if (hasData) {
    const tr = document.createElement("tr");
    tr.className = "total-row";
    const label = document.createElement("td");
    label.textContent = "合计";
    const spacer = document.createElement("td");
    const timeSpacer = document.createElement("td");
    const planTd = document.createElement("td");
    planTd.textContent = totals.planKnown ? `$${totals.planUsd}/月` : "—";
    // 合计倍数只按「套餐月费已知的 Cursor 账户」口径计算，与套餐列保持一致
    const totalRatio =
      totals.planUsd > 0 && !isSingleDaySpan() ? `${(totals.planEquiv / totals.planUsd).toFixed(1)}×` : "—";
    tr.append(
      label,
      spacer,
      timeSpacer,
      planTd,
      numTd(fmtTokens(totals.tokens), fmtInt(totals.tokens)),
      numTd(fmtUsd(totals.actual)),
      numTd(fmtUsd(totals.equivalent)),
      numTd(totalRatio)
    );
    body.append(tr);
  }
}

/**
 * 用 overviewResults 中当前已有数据（含缓存种子与刚完成的来源）合并渲染结果区。
 * 渐进调用：每个来源完成后都重新合并一次，图表随数据逐步细化。
 * 返回合并结果，无任何可用数据时返回 null（不主动隐藏结果区）。
 */
function renderOverviewMerged() {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  const aggs = [];
  const ats = [];
  let planUsd = 0;
  let planEquiv = 0;
  let planKnown = false;
  let cursorCount = 0;
  for (const a of cursorAccounts) {
    const r = overviewResults.get(a.id);
    if (!r || !r.agg) continue;
    aggs.push(r.agg);
    cursorCount += 1;
    if (Number.isFinite(r.at)) ats.push(r.at);
    const price = planMonthlyUsd(a.status ? a.status.membershipType : null);
    if (price != null) {
      planUsd += price;
      planEquiv += r.agg.totalEquivalentUsd;
      planKnown = true;
    }
  }
  const localNames = [];
  for (const key of LOCAL_KEYS) {
    const local = overviewResults.get(key);
    const scanAgg = local && local.scan ? local.scan.aggregate : null;
    if (!scanAgg) continue;
    aggs.push(scanAgg);
    localNames.push(LOCAL_SOURCES[key].mergedName);
    if (Number.isFinite(local.at)) ats.push(local.at);
  }
  if (!aggs.length) return null;

  const merged = mergeAggregates(aggs);
  const sources = [];
  if (cursorCount) sources.push(`${cursorCount} 个 Cursor 账户`);
  sources.push(...localNames);
  // 单日跨度：单日费用对比套餐月费无意义，隐藏月费倍数卡片；柱图切为 24 小时分布
  const tail = isSingleDaySpan()
    ? singleDayTail()
    : "倍数 = 等价费用 ÷ 套餐月费（仅计入 Cursor 账户，选近 30 天时最具参考性）";
  renderAggregate(merged, {
    showActual: true,
    plan: planKnown && !isSingleDaySpan() ? { monthlyUsd: planUsd, equivalentUsd: planEquiv } : null,
    metaText: `全部总览 · ${rangeText()} · ${sources.join(" + ")} 合并 · 等价费用按官方 API 价折算，实扣为 Cursor 实际计费；${tail}。`,
    dailySources: collectDailySources(),
  });
  // 合并视图的数据时间取最早的来源时间（保守口径）
  markUpdated(ats.length ? Math.min(...ats) : Date.now());
  return merged;
}

// 上次总览加载的跨度签名：跨度切换后不复用上一跨度的内存结果作种子（口径不同会串数）
let overviewSpanSig = "";

async function loadOverview(force, seq) {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  el("#usage-overview").hidden = false;
  const spanSig = `${rangeKey()}:${scanKey()}`;
  const previous = spanSig === overviewSpanSig ? new Map(overviewResults) : new Map();
  overviewSpanSig = spanSig;
  overviewResults.clear();

  // 1) 缓存种子：先用缓存（含过期缓存）填充各来源并立即渲染卡片 / 图表 / 明细。
  //    有效期内的来源直接标「完成」，不再进入「更新中…」；全部新鲜时静默完成（不弹提示），
  //    这样账户同步预取过用量后再进本页，看起来就是「不重新统计」。
  const ttl = effectiveTtlMs();
  for (const a of cursorAccounts) {
    const cached = getCachedAgg(a);
    const prev = previous.get(a.id);
    const agg = (cached && cached.agg) || (prev && prev.agg) || null;
    const at = cached ? cached.at : prev && prev.at;
    const fresh = !force && !!cached && Date.now() - cached.at < ttl;
    overviewResults.set(a.id, { state: fresh ? "ok" : "pending", agg, at });
  }
  // 本地扫描来源（Codex / Claude）：缓存种子，与 Cursor 账户同一口径
  for (const key of LOCAL_KEYS) {
    const cachedEntry = getCachedLocal(key, null);
    const prev = previous.get(key);
    const scan = (cachedEntry && cachedEntry.scan) || (prev && prev.scan) || null;
    const at = cachedEntry ? cachedEntry.at : prev && prev.at;
    const fresh = !force && !!cachedEntry && Date.now() - cachedEntry.at < ttl;
    overviewResults.set(key, { state: fresh ? "ok" : "pending", scan, at });
  }
  renderOverviewTable();
  renderOverviewMerged();
  // 加载进度不再弹 toast：无缓存时骨架屏占位，有缓存时顶部「更新中…」+ 状态列体现
  clearStatus();

  // 2) 本地 Codex / Claude 扫描与各 Cursor 账户并行拉取，每个来源完成后立即合并重绘
  //   （有效期内的来源在 usage_data 中直接命中缓存，不会走网络）
  const scanJobs = LOCAL_KEYS.map((key) =>
    fetchLocal(key, null, force)
      .then((entry) => {
        overviewResults.set(key, { state: "ok", scan: entry.scan, at: entry.at });
      })
      .catch((error) => {
        const prev = overviewResults.get(key) || {};
        const fallback = error && error.usageCache;
        overviewResults.set(key, {
          state: "error",
          error: resetError(error),
          scan: (fallback && fallback.scan) || prev.scan,
          at: (fallback && fallback.at) || prev.at,
        });
      })
      .then(() => {
        if (seq !== loadSeq) return;
        renderOverviewTable();
        renderOverviewMerged();
      })
  );

  let next = 0;
  const lane = async () => {
    while (next < cursorAccounts.length) {
      const acc = cursorAccounts[next];
      next += 1;
      try {
        const entry = await fetchAggregate(acc, force);
        overviewResults.set(acc.id, { state: "ok", agg: entry.agg, at: entry.at });
      } catch (error) {
        const prev = overviewResults.get(acc.id) || {};
        const fallback = error && error.usageCache;
        overviewResults.set(acc.id, {
          state: "error",
          error: resetError(error),
          agg: (fallback && fallback.agg) || prev.agg,
          at: (fallback && fallback.at) || prev.at,
        });
      }
      if (seq !== loadSeq) return;
      renderOverviewTable();
      renderOverviewMerged();
    }
  };
  await Promise.all([
    ...Array.from({ length: Math.min(OVERVIEW_CONCURRENCY, cursorAccounts.length) }, lane),
    ...scanJobs,
  ]);
  if (seq !== loadSeq) return;

  renderOverviewTable();
  const merged = renderOverviewMerged();
  const failed = cursorAccounts.filter((a) => {
    const r = overviewResults.get(a.id);
    return r && r.state === "error";
  }).length;

  if (merged) {
    const warns = [];
    if (failed) warns.push(`${failed} 个 Cursor 账户统计失败`);
    for (const key of LOCAL_KEYS) {
      const local = overviewResults.get(key);
      if (local && local.state === "error") warns.push(`${LOCAL_SOURCES[key].mergedName} 扫描失败`);
    }
    if (warns.length) setStatus("warn", `${warns.join("；")}，明细见上表；失败来源如有上次数据则继续显示。`);
    else if (merged.models.length === 0) setStatus("warn", "该时间范围内没有用量记录。");
    else if (!cursorAccounts.length) setStatus("", "尚未添加 Cursor 账户，总览目前仅含本地 ChatGPT / Claude 用量。");
    else clearStatus();
  } else {
    setStatus("bad", "全部来源统计失败，请检查 Token 与本地会话目录。");
  }
}

function renderCursorAccount(account, agg, at) {
  const price = planMonthlyUsd(account.status ? account.status.membershipType : null);
  const tail = isSingleDaySpan() ? singleDayTail() : "倍数 = 等价费用 ÷ 套餐月费";
  renderAggregate(agg, {
    showActual: true,
    plan:
      price != null && !isSingleDaySpan()
        ? { monthlyUsd: price, equivalentUsd: agg.totalEquivalentUsd }
        : null,
    metaText: `${accountLabel(account)} · ${rangeText()} · 共 ${agg.models.length} 个模型 · 等价费用按官方 API 价折算，实扣为 Cursor 实际计费；${tail}。`,
    dailySources: [
      { label: accountLabel(account), daily: agg.daily || [], hourly: agg.hourly || [], showActual: true },
    ],
  });
  markUpdated(at);
  if (agg.models.length === 0) setStatus("warn", "该时间范围内没有用量记录。");
}

async function loadCursorAccount(account, force, seq) {
  // 先渲染缓存（含过期缓存），再后台拉取最新数据（进度不弹 toast，见骨架屏与「更新中…」）
  const cached = getCachedAgg(account);
  if (cached) renderCursorAccount(account, cached.agg, cached.at);
  try {
    const entry = await fetchAggregate(account, force);
    if (seq !== loadSeq) return;
    clearStatus();
    renderCursorAccount(account, entry.agg, entry.at);
  } catch (error) {
    if (seq !== loadSeq) return;
    const fallback = (error && error.usageCache) || cached;
    if (fallback && fallback.agg) renderCursorAccount(account, fallback.agg, fallback.at);
    setStatus("bad", fallback ? `更新失败（仍显示上次数据）：${resetError(error)}` : `统计失败：${resetError(error)}`);
  }
}

/** 单个本地来源（本地 Codex / Claude 分析）视图的结果区。 */
function renderLocalScan(key, scan, at) {
  const src = LOCAL_SOURCES[key];
  const rootsText = scan.roots.length ? scan.roots.join("、") : "未找到会话目录";
  renderAggregate(scan.aggregate, {
    showActual: false,
    plan: null,
    metaText: `${src.title} · ${rangeText()} · 扫描 ${scan.filesScanned} 个文件 / ${scan.sessions} 个会话 · 目录：${rootsText}`,
    dailySources: [
      {
        label: src.label,
        daily: scan.aggregate.daily || [],
        hourly: scan.aggregate.hourly || [],
        showActual: false,
      },
    ],
  });
  markUpdated(at);
  if (scan.aggregate.models.length === 0) {
    setStatus("warn", `${src.emptyText}目录：${rootsText}`);
  }
}

async function loadLocalScan(key, force, seq) {
  const home = localHomeRaw(key) || null;
  // 先渲染缓存（含过期缓存），再后台重新扫描（进度不弹 toast，见骨架屏与「更新中…」）
  const cached = getCachedLocal(key, home);
  if (cached) renderLocalScan(key, cached.scan, cached.at);
  const btn = el(LOCAL_SOURCES[key].scanBtn);
  btn.disabled = true;
  try {
    const entry = await fetchLocal(key, home, force);
    if (seq !== loadSeq) return;
    clearStatus();
    renderLocalScan(key, entry.scan, entry.at);
  } catch (error) {
    if (seq !== loadSeq) return;
    const fallback = (error && error.usageCache) || cached;
    if (fallback && fallback.scan) renderLocalScan(key, fallback.scan, fallback.at);
    setStatus("bad", fallback ? `扫描失败（仍显示上次数据）：${resetError(error)}` : `扫描失败：${resetError(error)}`);
  } finally {
    btn.disabled = false;
  }
}

async function loadCurrent(force) {
  // 结果区仍展示着当前选择且未过期时无需重载。
  // 此判断必须在递增 loadSeq 之前：否则一次「无操作」的调用（如重复点击当前 chip）
  // 会作废在途加载却不开启新加载——lanes 提前退出、loading 永远无法复位，
  // 总览行从此冻结在「更新中…」，预取后的缓存重绘也全部被 loading 挡住。
  if (!force && isRenderedFresh()) return;
  cacheDirty = false; // 完整加载本身就会重读缓存
  // 作废所有在途加载，避免旧结果渲染到已切换的视图上
  const seq = ++loadSeq;
  loading = true;
  el("#usage-loading").hidden = false;
  try {
    if (selection === "all") {
      await loadOverview(force, seq);
    } else if (isLocalSelection()) {
      await loadLocalScan(selection, force, seq);
    } else {
      const account = currentAccount();
      if (!account) {
        selection = "all";
        rebuildChips();
        applyVisibility();
        await loadOverview(force, seq);
      } else {
        await loadCursorAccount(account, force, seq);
      }
    }
  } catch (error) {
    // 统计编排本身异常（各来源的拉取失败已在内部消化）——必须可见，
    // 否则表现为状态列永远停在「统计中…」的无声冻结
    console.error("统计流程异常：", error);
    if (seq === loadSeq) setStatus("bad", `统计流程异常：${resetError(error)}，请点「刷新」重试。`);
  } finally {
    if (seq === loadSeq) {
      loading = false;
      el("#usage-loading").hidden = true;
      // 全部失败等场景什么都没渲染出来，不能让骨架屏永远转下去
      if (el("#usage-results").hidden) el("#usage-skeleton").hidden = true;
      // 记录本轮加载完成时间：失败来源不再把整页拖成永久过期（见 isRenderedFresh）
      lastAttemptAt = Date.now();
    }
  }
}

/**
 * 统一加载入口：发起加载后按同步段结果决定骨架屏——
 * loadCurrent 的同步段会用缓存种子立即渲染（stale-while-revalidate），
 * 走完后结果区仍隐藏说明新视图 / 新跨度无任何缓存，露出骨架占位；
 * 首个结果渲染（renderAggregate）时骨架自动隐藏。
 */
function loadView(force) {
  void loadCurrent(!!force);
  el("#usage-skeleton").hidden = !el("#usage-results").hidden;
}

/* ---------- 统一刷新：账户刷新联动预取 / 跨窗口缓存联动 ---------- */

/**
 * 把共享缓存里比 overviewResults 更新的条目合并进来（不发起任何拉取）。
 * 只替换获取时间变新的来源，其余（含失败态）保持原状；返回是否有变化。
 */
function applyCacheToOverview() {
  let changed = false;
  const take = (key, cached, field) => {
    if (!cached) return;
    const prev = overviewResults.get(key);
    if (prev && prev.at >= cached.at) return;
    overviewResults.set(key, { state: "ok", [field]: cached[field], at: cached.at });
    changed = true;
  };
  for (const a of getAccounts().filter((x) => x.kind === "cursor")) take(a.id, getCachedAgg(a), "agg");
  for (const key of LOCAL_KEYS) take(key, getCachedLocal(key, null), "scan");
  return changed;
}

/**
 * 用共享缓存里的最新条目原地重绘当前视图，不发起任何拉取——后台预取 / 托盘写入到本页的
 * 唯一落点，其它来源不会因此被顺带重新统计。结果区未展示当前视图（隐藏中 / 跨零点后键
 * 变化）时不动，交给随后的 loadView 走常规新鲜度判断。
 */
function rerenderFromCache() {
  if (renderedFor !== selectionKey()) return;
  if (selection === "all") {
    const changed = applyCacheToOverview();
    // 状态列总要落地（预取失败的来源从「更新中…」转为错误态），数据没变则不重画卡片与图表
    renderOverviewTable();
    if (changed) renderOverviewMerged();
    return;
  }
  if (isLocalSelection()) {
    const cached = getCachedLocal(selection, localHomeRaw(selection) || null);
    if (cached && cached.at > renderedAt) renderLocalScan(selection, cached.scan, cached.at);
    return;
  }
  const account = currentAccount();
  const cached = account ? getCachedAgg(account) : null;
  if (cached && cached.at > renderedAt) renderCursorAccount(account, cached.agg, cached.at);
}

/**
 * 共享缓存有更新（本页预取完成 / 对端窗口写入）后的落点：可见时合并 250ms 从缓存重绘；
 * 隐藏时只记脏标记，下次显示先重绘再走常规新鲜度判断。刻意不作废整页新鲜度——
 * 否则会连带把其它未过期来源一起重新统计（表现为一个账户刷新完、所有行都变「更新中…」）。
 */
function scheduleRerenderFromCache() {
  if (!panelVisible) {
    cacheDirty = true;
    return;
  }
  clearTimeout(rerenderTimer);
  rerenderTimer = setTimeout(() => {
    if (!panelVisible) {
      cacheDirty = true;
      return;
    }
    if (loading) return;
    // 结果区尚未展示当前视图（首次加载全部失败 / 跨零点后今日键变化）：走常规加载，
    // 刚写入的缓存会直接命中，其余来源按新鲜度决定
    if (renderedFor === selectionKey()) rerenderFromCache();
    else loadView(false);
  }, 250);
}

/**
 * 对比上次账户快照：lastRefreshAt 变化且非 0 的为「状态刚刷新过」（触发用量预取）；
 * Cursor 账户 token 变化的为「凭据刚更换」（旧用量缓存作废；Cursor 的 token 不会自动轮换，
 * 变化只可能来自用户编辑）。首批快照只登记（启动时账户数据来自磁盘，并非刚刷新）。
 */
function diffAccounts(list) {
  const ids = new Set(list.map((a) => a.id));
  for (const id of [...seenAccounts.keys()]) {
    if (!ids.has(id)) seenAccounts.delete(id);
  }
  const refreshed = [];
  const rekeyed = [];
  for (const a of list) {
    const at = Number(a.lastRefreshAt) || 0;
    const prev = seenAccounts.get(a.id);
    seenAccounts.set(a.id, { at, token: a.token });
    if (!seenInitialized) continue;
    if (at > 0 && at !== (prev && prev.at)) refreshed.push(a);
    if (prev && a.kind === "cursor" && a.token !== prev.token) rekeyed.push(a);
  }
  seenInitialized = true;
  return { refreshed, rekeyed };
}

/**
 * 统一刷新的用量侧：不论从哪个入口刷新账户状态（账户页 / 托盘 / 定时 / 本页「刷新」），
 * 都后台按当前跨度强制预取对应来源的用量写入共享缓存，完成后本页只从缓存重绘
 * （rerenderFromCache）——这是定时刷新抵达本页的唯一路径。
 * Cursor 按账户预取；本地 Codex / Claude 扫描：同类账户刷新过则重扫，否则仅在缓存临近 /
 * 已过期时顺带重扫（只有 Cursor 账户时本地来源也能随定时刷新更新）。
 * 缓存足够新（刚被本页或托盘拉过）时跳过；并发去重由 usage_data 的 in-flight 表保证
 * （与本页「刷新」正在进行的强制统计撞车时复用同一请求）。
 * 总览可见且不在加载中时，把预取中的来源标成「更新中…」，失败则显示错误并保留旧数据。
 */
function prefetchUsageFor(accounts) {
  const jobs = [];
  let marked = false;
  const canMark = panelVisible && !loading && selection === "all" && renderedFor === selectionKey();
  const enqueue = (key, fetchPromise) => {
    const prev = canMark ? overviewResults.get(key) : null;
    if (prev) {
      overviewResults.set(key, { ...prev, state: "pending" });
      marked = true;
    }
    jobs.push(
      fetchPromise.catch((error) => {
        const cur = overviewResults.get(key);
        if (cur && cur.state === "pending") {
          overviewResults.set(key, { ...cur, state: "error", error: resetError(error) });
        }
      })
    );
  };
  for (const a of accounts) {
    if (a.kind !== "cursor") continue;
    const cached = cacheGetAgg(a.id, rangeKey());
    if (cached && Date.now() - cached.at < PREFETCH_MIN_AGE_MS) continue;
    enqueue(a.id, fetchAggregate(a, true));
  }
  // 本地扫描：同类账户刷新过视同该来源刚被刷新，60s 内扫过才跳过；否则只在缓存临近 / 已过期
  // 时顺带重扫（阈值取 TTL 提前 60s，避免与定时刷新节拍差几秒而整轮错过）。
  for (const key of LOCAL_KEYS) {
    const minAge = accounts.some((a) => a.kind === LOCAL_SOURCES[key].tag)
      ? PREFETCH_MIN_AGE_MS
      : Math.max(effectiveTtlMs() - PREFETCH_MIN_AGE_MS, PREFETCH_MIN_AGE_MS);
    const cached = getCachedLocal(key, "");
    if (cached && Date.now() - cached.at < minAge) continue;
    enqueue(key, fetchLocal(key, null, true));
  }
  if (marked) renderOverviewTable();
  if (!jobs.length) return;
  void Promise.allSettled(jobs).then(() => scheduleRerenderFromCache());
}

/** 对端窗口写入的缓存键是否影响当前视图（跨度不同则无需重渲染）。 */
function cacheKeyAffectsCurrentView(key) {
  if (!key || !key.startsWith(USAGE_CACHE_PREFIX)) return true; // 通配 / 整体清理保守处理
  const rest = key.slice(USAGE_CACHE_PREFIX.length);
  if (rest.startsWith("agg:")) {
    return !isLocalSelection() && rest.endsWith(`:${rangeKey()}`);
  }
  for (const localKey of LOCAL_KEYS) {
    const prefix = LOCAL_SOURCES[localKey].cachePrefix;
    if (!rest.startsWith(prefix)) continue;
    if (selection !== "all" && selection !== localKey) return false;
    return rest.slice(prefix.length).startsWith(`${scanKey()}:`);
  }
  return true;
}

/* ---------- 初始化 ---------- */

function setSpan(next) {
  if (!Object.prototype.hasOwnProperty.call(SPAN_LABELS, next) || next === span) return;
  span = next;
  for (const btn of el("#usage-span").querySelectorAll("[data-span]")) {
    const on = btn.dataset.span === next;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-selected", on ? "true" : "false");
  }
  // 先隐藏结果区再加载：新跨度有缓存时同步段立即重渲染（无闪烁），
  // 无缓存时由 loadView 露出骨架占位，不残留上一跨度口径的数字
  applyVisibility();
  loadView(false);
}

export function initUsage() {
  el("#usage-refresh").addEventListener("click", () => {
    // 统一刷新：重新统计当前视图，并顺带刷新视图相关账户的状态
    loadView(true);
    const ids =
      selection === "all"
        ? getAccounts().filter((a) => a.kind === "cursor").map((a) => a.id)
        : currentAccount()
        ? [selection]
        : [];
    if (ids.length) void refreshAccounts(ids);
  });
  // 生成全量用量快照图片：数据获取（在线 / 本地存档回退）与绘制见 snapshot.js
  const snapshotBtn = el("#usage-snapshot");
  snapshotBtn.addEventListener("click", async () => {
    const account = currentAccount();
    if (!account || snapshotBtn.disabled) return;
    snapshotBtn.disabled = true;
    toast("", "正在生成用量快照…", { key: "snapshot" });
    try {
      const result = await generateCursorSnapshot(account);
      if (result && result.cancelled) dismissToast("snapshot");
      else toast("ok", `快照已保存：${result.path}`, { key: "snapshot" });
    } catch (error) {
      toast("bad", `生成快照失败：${resetError(error)}`, { key: "snapshot" });
    } finally {
      snapshotBtn.disabled = false;
    }
  });
  // 时间跨度二级 TAB（默认今天）
  el("#usage-span").addEventListener("click", (event) => {
    const btn = event.target instanceof Element ? event.target.closest("[data-span]") : null;
    if (btn) setSpan(btn.dataset.span);
  });
  for (const key of LOCAL_KEYS) {
    el(LOCAL_SOURCES[key].scanBtn).addEventListener("click", () => {
      if (selection === key) loadView(true);
    });
  }

  onAccountsChanged((list) => {
    syncUsageInterval(getRefreshIntervalMinutes());
    const sig = list.map((a) => a.id).join(",");
    const idsChanged = sig !== accountIdsSig;
    if (idsChanged) {
      accountIdsSig = sig;
      // 清理已删除账户的聚合缓存（内存 + localStorage）
      purgeMissingAccounts(list);
    }
    const { refreshed, rekeyed } = diffAccounts(list);
    // 刚更换过凭据的账户：旧 token 统计出的用量缓存与总览内存结果一并作废
    // （只在变化那一次做，不反复广播），重新加载时不再拿旧数据垫底
    for (const a of rekeyed) {
      purgeAccountCache(a.id);
      overviewResults.delete(a.id);
    }
    // 账户增删 / 换凭据是结构性变化，结果区视为过期
    if (idsChanged || rekeyed.length) {
      renderedAt = 0;
      lastAttemptAt = 0;
    }
    // 统一刷新：状态刚刷新过的账户（任意入口触发）后台预取其用量
    if (refreshed.length) prefetchUsageFor(refreshed);
    const stillExists =
      selection === "all" || isLocalSelection() || list.some((a) => a.id === selection && a.kind === "cursor");
    rebuildChips();
    if (!stillExists) {
      applyVisibility();
      if (panelVisible) loadView(false);
      return;
    }
    if (!panelVisible || loading) return;
    // 账户状态更新时同步总览表中的套餐信息
    if (selection === "all") renderOverviewTable();
    // 只有结构性变化才重新加载当前视图。状态刷新一律不走这里——否则页面恰好过期时，
    // 一个账户刷新完会把所有未过期来源一起重新统计（表现为所有行同时变「更新中…」）；
    // 刷新带来的数据更新由 prefetchUsageFor → rerenderFromCache 逐行落地。
    if (idsChanged || rekeyed.length) loadView(false);
  });

  window.addEventListener("panelshown", (event) => {
    panelVisible = !!(event.detail && event.detail.id === "usage-panel");
    if (!panelVisible || loading) return;
    // 隐藏期间后台预取 / 托盘写过缓存：先静默换上新数据，再按常规新鲜度决定是否重新加载
    if (cacheDirty) {
      cacheDirty = false;
      rerenderFromCache();
    }
    loadView(false);
  });

  // 数字单位切换后，用现有数据即时重绘结果区与总览表
  window.addEventListener("unitchange", () => {
    if (lastAggRender && !el("#usage-results").hidden) {
      renderAggregate(lastAggRender.agg, lastAggRender.opts);
    }
    if (!el("#usage-overview").hidden) renderOverviewTable();
  });

  // 对端窗口（托盘）写入共享用量缓存后：丢掉本窗口内存旧条目，跨度相关时从缓存重渲染。
  // 事件会回送到写入窗口，用 origin 过滤自己的写入避免自触发循环。
  listen(USAGE_CACHE_EVENT, (event) => {
    const payload = event.payload || {};
    if (payload.origin && payload.origin === USAGE_CACHE_ORIGIN) return;
    forgetUsageCacheFromEvent(payload.key);
    if (cacheKeyAffectsCurrentView(payload.key)) scheduleRerenderFromCache();
  }).catch(() => {
    /* 非 Tauri 环境无事件桥，靠 storage 事件兜底 */
  });
  window.addEventListener("storage", (event) => {
    if (!event.key || !event.key.startsWith(USAGE_CACHE_PREFIX)) return;
    forgetUsageCacheFromEvent(event.key);
    if (cacheKeyAffectsCurrentView(event.key)) scheduleRerenderFromCache();
  });

  // 总览表「数据更新」列的相对时间随时间流逝定期重算（与账户页节奏一致），
  // 只改时间文本，不整表重绘
  setInterval(() => {
    for (const cell of el("#usage-overview-body").querySelectorAll(".usage-data-at")) {
      const at = Number(cell.dataset.at);
      if (at > 0) cell.textContent = relativeFromUnixSeconds(at / 1000);
    }
  }, 30_000);

  rebuildChips();
  applyVisibility();
  setUsageCacheTtlMs(getRefreshIntervalMinutes() > 0 ? getRefreshIntervalMinutes() * 60_000 : DEFAULT_TTL_MS);
}
