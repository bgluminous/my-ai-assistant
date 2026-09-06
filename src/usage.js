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
  iconAction,
} from "./shared.js";
import {
  getCachedAgg as cacheGetAgg,
  getCachedScan as cacheGetScan,
  getCachedClaudeScan as cacheGetClaudeScan,
  fetchCursorAggregate,
  fetchArchivedAggregate,
  listDeletedUsage,
  removeDeletedUsage,
  fetchCodexScan,
  fetchClaudeScan,
  purgeAccountCache,
  purgeMissingAccounts,
  setUsageCacheTtlMs,
  DEFAULT_USAGE_TTL_MS,
  USAGE_CACHE_PREFIX,
  USAGE_CACHE_EVENT,
  USAGE_CACHE_ORIGIN,
  USAGE_LOCAL_CLEARED_EVENT,
  USAGE_PRICING_CHANGED_EVENT,
  forgetUsageCacheFromEvent,
  todayRangeKey,
  dayRangeKey,
  multiDayRangeKey,
  dayStartMs,
  localYmd,
  addLocalDays,
  parseYmd,
} from "./usage_data.js";
import {
  getAccounts,
  onAccountsChanged,
  refreshAccounts,
  getRefreshIntervalMinutes,
  confirmDialog,
} from "./accounts.js";
import { membershipLabel, planMonthlyUsd, relativeFromUnixSeconds, cursorIdentity } from "./account_format.js";
import { generateCursorSnapshot } from "./snapshot.js";
import { openRawEvents } from "./raw_events.js";

// 用量统计：Cursor 账单数据源自「账户管理」中保存的 Cursor 账户，自动拉取，无需手动输入。
// Codex / Claude 账户不在本页展示（额度信息见「账户管理」）；它们的账单只能来自本地会话日志，
// 分别由本地扫描（CODEX_HOME / CLAUDE_CONFIG_DIR）折算，在总览中作为独立来源行参与合并。
// 统计对象为多选（点击 chip 切换选中 / 取消，「全部总览」= 全选）：
// - 全部总览：各 Cursor 账户 + 已删除账户保留数据 + 本地 Codex + 本地 Claude 合并；
// - 单选一项：专属视图——单个 Cursor 账户（可生成快照）/ 本地 Codex 或 Claude 用量分析（带目录
//   输入与重新扫描）/「已删除」（全部已删除账户合并）；
// - 选中两项及以上：所选来源的合并视图，结构与全部总览相同（卡片 / 图表 / 明细 + 各来源账单表）。
// 时间范围 = 周期（日 / 周 / 月 / 全部）+ 左右滑动一个周期，或自定义起止日期（按区间长度平移）；
// 默认「日」= 今天。缓存键：今天 today:YYYY-MM-DD（与托盘共用）、已结束的单日 day:YYYY-MM-DD、
// 多日 range:首日_末日、全部 "0"。不超过两天的区间按小时柱图（后端按区间内日期给出 hourly），
// 其余按日柱图，周 / 月按完整日历区间列轴（未到的日子留空）。所有柱图横轴统一从左到右由旧到新。
//
// 缓存策略：与托盘总览共用 usage_data.js（内存 + localStorage，键前缀 usage-cache:v4:）。
// 打开视图时先用缓存（含过期缓存）立即渲染，再在后台拉取最新数据原地刷新（stale-while-revalidate）。
// Cursor 账户的数据源是后端按账户维护的本地用量事件库：各跨度都由后端从事件库切片，事件库在
// 有效期内只切片不联网（切换跨度不再联网），过期才增量同步；「刷新」按钮强制全量重拉。
//
// 已删除账户：删除 Cursor 账户时勾选「保留统计数据」，其事件库会带账户展示信息留在本机。
// 总览把它们当作独立来源（标「已删除」）计入合计与合并图表，只在所选跨度内有用量时出现；
// 行内提供「删除统计数据」彻底清理。有记录时筛选末尾多一个「已删除」chip（键 DELETED_SELECTION），
// 作为一个整体项参与选择：单选即全部已删除账户的合并用量与逐账户账单表；托盘总览不计入。
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

// 统计对象为多选：selectedKeys 为选中的 chip 值（Cursor 账户 id / "local" / "local-claude" / "deleted"），
// 空集 = 「全部总览」。selection 由它推导（syncSelection）：
// "all"（空集）| 单个键（单选，走专属视图）| "multi"（两项及以上，合并视图）
const selectedKeys = new Set();
let selection = "all";
// 时间范围：period 为周期类型，anchor 为周期内任意一天（本地 0 点），日 / 周 / 月由它定位当前区间，
// 左右滑动即移动 anchor 一个周期；custom 用 customStart / customEnd（本地 0 点，含末日），
// 滑动按区间长度平移；all 无区间。
let period = "day"; // "day" | "week" | "month" | "all" | "custom"
let anchor = new Date(dayStartMs(0));
let customStart = new Date(dayStartMs(-6));
let customEnd = new Date(dayStartMs(0));
let loadSeq = 0; // 加载序号，防止过期的异步结果覆盖新视图
let loading = false;
let panelVisible = false;
let renderedFor = ""; // 结果区当前展示的 selection+range（隐藏时为空）
let renderedAt = 0; // 结果区数据的获取时间（显示「更新于」与缓存标记）
let lastAttemptAt = 0; // 最近一次完整加载的完成时间（新鲜度门控；失败来源不再把整页永久拖成过期）
let usageInterval = 0; // 定时刷新间隔（分钟，0 = 关闭），与账户状态刷新共用；本页只用它推导缓存有效期
let accountIdsSig = "";
const overviewResults = new Map(); // accountId -> { state, agg?, error? }
let deletedRecords = []; // 已删除账户保留的统计数据记录（后端 cursor_usage_deleted_list）
let deletedDirty = true; // 账户增删后置脏，下次总览加载时重新拉取已删除记录列表
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
  // 环形图的图例是 HTML（跟随 CSS 变量换色），这里只需换扇区分隔色
  for (const chart of [tokenChart, modelChart, doughnutChart]) {
    if (!chart) continue;
    chart.data.datasets[0].borderColor = t.border;
    chart.update("none");
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

const PERIODS = ["day", "week", "month", "all", "custom"];
const WEEKDAYS = ["日", "一", "二", "三", "四", "五", "六"];

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
// 「已删除」chip 的键：全部已删除账户保留的数据作为一个整体项参与选择
const DELETED_SELECTION = "deleted";
// 两项及以上被选中时的 selection 值：所选来源的合并视图
const MULTI_SELECTION = "multi";

function isLocalSelection(value = selection) {
  return Object.prototype.hasOwnProperty.call(LOCAL_SOURCES, value);
}
function isDeletedSelection(value = selection) {
  return value === DELETED_SELECTION;
}
function isMultiSelection(value = selection) {
  return value === MULTI_SELECTION;
}
/** 合并视图（全部总览 / 多选 / 已删除）：结果区为多来源合并，下方带「各来源账单」表。 */
function isMergedView() {
  return selection === "all" || isMultiSelection() || isDeletedSelection();
}
/** 由 selectedKeys 推导 selection。 */
function syncSelection() {
  if (!selectedKeys.size) selection = "all";
  else if (selectedKeys.size === 1) selection = [...selectedKeys][0];
  else selection = MULTI_SELECTION;
}
/**
 * 当前视图参与合并的来源：全部总览取全部；多选按 selectedKeys 过滤；
 * 「已删除」单选只含已删除账户。单账户 / 单本地视图不走这里。
 */
function activeSources() {
  const all = selection === "all";
  return {
    cursorAccounts: getAccounts().filter((a) => a.kind === "cursor" && (all || selectedKeys.has(a.id))),
    localKeys: LOCAL_KEYS.filter((key) => all || selectedKeys.has(key)),
    includeDeleted: all || selectedKeys.has(DELETED_SELECTION),
  };
}
/** 本地来源表单里填写的会话目录（已 trim，空串表示用默认目录）。 */
function localHomeRaw(key) {
  return el(LOCAL_SOURCES[key].homeInput).value.trim();
}

/* ---------- 时间范围（周期 + 滑动 / 自定义区间） ---------- */

function isAllPeriod() {
  return period === "all";
}
/** 本地自然日 0 点的 Date（按日历日推算，夏令时切换日也正确）。 */
function dayStart(d) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}
/** 所在周的周一 0 点。 */
function weekStart(d) {
  const base = dayStart(d);
  const offset = (base.getDay() + 6) % 7; // 周一 = 0
  return addLocalDays(base, -offset);
}
/**
 * 当前区间的首日与末日（本地 0 点 Date，末日含当天）；「全部」返回 null。
 * 周 / 月为完整日历周期（本周 / 本月含尚未到来的日子），拉取与轴标签各自截到今天。
 */
function currentRange() {
  if (isAllPeriod()) return null;
  if (period === "custom") return { startDate: customStart, endDate: customEnd };
  if (period === "day") return { startDate: anchor, endDate: anchor };
  if (period === "week") {
    const start = weekStart(anchor);
    return { startDate: start, endDate: addLocalDays(start, 6) };
  }
  const start = new Date(anchor.getFullYear(), anchor.getMonth(), 1);
  return { startDate: start, endDate: new Date(anchor.getFullYear(), anchor.getMonth() + 1, 0) };
}
/** 区间天数（含首末日）；「全部」为 0。 */
function rangeDays() {
  const r = currentRange();
  if (!r) return 0;
  return Math.round((r.endDate.getTime() - r.startDate.getTime()) / 86_400_000) + 1;
}
/** 区间是否包含今天（进行中的数据，柱图高亮当前小时）。 */
function rangeIncludesToday() {
  const r = currentRange();
  if (!r) return true;
  const today = dayStartMs(0);
  return r.startDate.getTime() <= today && today <= r.endDate.getTime();
}
/** 不超过两天的区间按小时柱图，其余按日柱图。 */
function isHourlyMode() {
  const days = rangeDays();
  return days > 0 && days <= 2;
}
/** 单日区间所展示的那一天（本地 YYYY-MM-DD）；多日区间返回首日。 */
function focusYmd() {
  const r = currentRange();
  return localYmd(r ? r.startDate : new Date());
}
/**
 * Cursor 聚合的缓存键：全部为 "0"；单日为 today:YYYY-MM-DD（今天，与托盘共用）或
 * day:YYYY-MM-DD（已结束的完整自然日）；多日为 range:首日_末日（含未到来的日子也按日历区间命名）。
 */
function rangeKey() {
  const r = currentRange();
  if (!r) return "0";
  const start = localYmd(r.startDate);
  const end = localYmd(r.endDate);
  if (start === end) return start === localYmd() ? todayRangeKey(start) : dayRangeKey(start);
  return multiDayRangeKey(start, end);
}
/** 本地 Codex / Claude 扫描的缓存键：与 rangeKey 一致，「全部」为 all。 */
function scanKey() {
  return isAllPeriod() ? "all" : rangeKey();
}
/** 拉取用的毫秒闭区间：首日 0 点到末日 24 点前 1ms，末日在未来时截到当前时刻；「全部」不限。 */
function rangeBounds() {
  const r = currentRange();
  if (!r) return { start: null, end: null };
  const end = Math.min(addLocalDays(r.endDate, 1).getTime() - 1, Date.now());
  return { start: r.startDate.getTime(), end };
}
function fmtMd(d) {
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
/** 当前区间的可读名称（工具栏导航与结果区说明共用）。 */
function rangeText() {
  const r = currentRange();
  if (!r) return "全部";
  const todayMs = dayStartMs(0);
  if (period === "day") {
    const ms = r.startDate.getTime();
    if (ms === todayMs) return "今天";
    if (ms === dayStartMs(-1)) return "昨天";
    return `${localYmd(r.startDate)}（周${WEEKDAYS[r.startDate.getDay()]}）`;
  }
  if (period === "week") {
    const prefix = weekStart(new Date(todayMs)).getTime() === r.startDate.getTime() ? "本周 " : "";
    return `${prefix}${fmtMd(r.startDate)} ～ ${fmtMd(r.endDate)}`;
  }
  if (period === "month") {
    const now = new Date(todayMs);
    const current = now.getFullYear() === r.startDate.getFullYear() && now.getMonth() === r.startDate.getMonth();
    return `${current ? "本月 " : ""}${r.startDate.getFullYear()} 年 ${r.startDate.getMonth() + 1} 月`;
  }
  const same = r.startDate.getTime() === r.endDate.getTime();
  return same
    ? `${localYmd(r.startDate)}（周${WEEKDAYS[r.startDate.getDay()]}）`
    : `${localYmd(r.startDate)} ～ ${localYmd(r.endDate)}（${rangeDays()} 天）`;
}
/** 按小时柱图的说明尾巴（结果区 meta 文案）。 */
function hourlyTail() {
  const days = rangeDays();
  const scope = days === 1 ? `该日（${focusYmd()}）` : "区间内两天";
  return `柱图为${scope} 0–24 时分布${rangeIncludesToday() ? "（当前小时高亮）" : ""}`;
}

/**
 * Cursor 账户在本页的统一名称（筛选 chip / 总览表 / 图例 / 结果说明 / 原始账单共用）：
 * 与账户页主显示同一口径——手填备注 > 用户名 > 邮箱 > 自动备注。
 */
function accountLabel(account) {
  return cursorIdentity(account).primary;
}
/** 已删除账户的名称：用删除时保留的展示信息按同一口径推导。 */
function deletedLabel(record) {
  return cursorIdentity({
    note: record.note,
    noteAuto: record.noteAuto !== false,
    status: { name: record.name, email: record.email },
  }).primary;
}
/** 聚合结果在所选跨度内是否有用量（已删除账户无用量时不占总览行）。 */
function hasUsage(agg) {
  return !!agg && (agg.totalTokens > 0 || (Array.isArray(agg.models) && agg.models.length > 0));
}
/** 已删除账户的删除日期（本地 YYYY-MM-DD），总览状态列展示「数据截至」。 */
function fmtDeletedAt(ms) {
  const n = Number(ms);
  return Number.isFinite(n) && n > 0 ? localYmd(new Date(n)) : "—";
}
function currentAccount() {
  return getAccounts().find((a) => a.id === selection) || null;
}
function selectionKey() {
  if (isLocalSelection()) return `${selection}:${scanKey()}:${localHomeRaw(selection)}`;
  // 多选：所选集合本身也是视图身份的一部分
  if (isMultiSelection()) return `multi:${[...selectedKeys].sort().join(",")}:${rangeKey()}:${scanKey()}`;
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
/**
 * 按日图横轴日期，从左到右由旧到新，与小时图 0:00 → 23:00 的方向一致。
 * 有界区间按完整日历区间列出（本周 / 本月尚未到来的日子留空）；「全部」从最早有数据的日期到今天。
 */
function dailyAxisLabels(sources) {
  const now = new Date();
  const today0 = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const range = currentRange();
  if (range) {
    const labels = [];
    let cur = range.startDate;
    while (cur.getTime() <= range.endDate.getTime() && labels.length < 3660) {
      labels.push(localYmd(cur));
      cur = addLocalDays(cur, 1);
    }
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
  const active = activeSources();
  for (const a of active.cursorAccounts) {
    const r = overviewResults.get(a.id) || null;
    entries.push({ kind: "cursor", account: a, result: r, tokens: r && r.agg ? r.agg.totalTokens : 0 });
  }
  // 已删除账户：只在所选跨度内有用量（或切片出错需要露出错误）时占一行，切片未完成的不占位
  if (active.includeDeleted) {
    for (const record of deletedRecords) {
      const r = overviewResults.get(record.accountId) || null;
      if (!r || (r.state !== "error" && !hasUsage(r.agg))) continue;
      entries.push({ kind: "cursor-deleted", record, result: r, tokens: r.agg ? r.agg.totalTokens : 0 });
    }
  }
  for (const key of active.localKeys) {
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
    if (src.kind === "cursor" || src.kind === "cursor-deleted") {
      const r = src.result;
      if (!r || !r.agg) continue;
      sources.push({
        label: src.kind === "cursor" ? accountLabel(src.account) : `${deletedLabel(src.record)}（已删除）`,
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
  // 不超过两天的区间渲染逐小时柱，0:00 → 23:00 从左到右（两天则 48 根，今天未到时段留空），
  // 其余区间按日渲染（同样从左到右由旧到新）；两种模式共用同一图表实例，切换时原地更新。
  const hourlyMode = isHourlyMode();
  const dayLabel = rangeText();
  el("#chart-daily-title").textContent = hourlyMode ? `按小时 Token（${dayLabel}）` : "按日 Token";
  // 小时模式的槽位：{ ymd, hour }，跨两天时标签带日期前缀
  let slots = null;
  let labels;
  if (hourlyMode) {
    slots = [];
    const range = currentRange();
    const days = rangeDays();
    for (let d = 0; d < days; d += 1) {
      const ymd = localYmd(addLocalDays(range.startDate, d));
      for (let h = 0; h <= 23; h += 1) slots.push({ ymd, hour: h });
    }
    labels = slots.map((s) => (days > 1 ? `${tickDate(s.ymd)} ${s.hour}:00` : `${s.hour}:00`));
  } else {
    labels = dailyAxisLabels(list);
  }
  const baseColors = list.map((_, i) => colorFor(i));
  const meta = [];
  const showActual = [];
  const datasets = list.map((s, i) => {
    let rows;
    if (hourlyMode) {
      const byKey = new Map((s.hourly || []).filter((r) => r && r.date).map((r) => [`${r.date}#${Number(r.hour)}`, r]));
      rows = slots.map((slot) => byKey.get(`${slot.ymd}#${slot.hour}`) || null);
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
    chart.$slots = slots;
    // 高亮带：小时图且区间含今天时标记当前小时列（已结束的日子与按日图不高亮）
    const now = new Date();
    const todayYmd = localYmd(now);
    chart.$todayIndex =
      hourlyMode && slots ? slots.findIndex((s) => s.ymd === todayYmd && s.hour === now.getHours()) : -1;
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
                const slot = chart.$slots ? chart.$slots[items[0].dataIndex] : null;
                if (!slot) return String(label);
                return `${titleDate(slot.ymd)} ${slot.hour}:00 – ${slot.hour + 1}:00`;
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

/**
 * 环形图的 HTML 图例（画布外定宽列表，见 .pie-legend）：色块 + 名字（过长省略，title 放全名与数值），
 * 点击切换对应扇区的显示 / 隐藏（与 Chart.js 自带图例一致）。每次渲染整体重建，
 * 隐藏状态从图表实例回读。
 */
function renderPieLegend(listId, chart, labels, colors, fmtValue) {
  const list = el(`#${listId}`);
  const data = (chart.data.datasets[0] && chart.data.datasets[0].data) || [];
  const total = data.reduce((sum, v, i) => sum + (chart.getDataVisibility(i) ? Number(v) || 0 : 0), 0);
  list.replaceChildren(
    ...labels.map((label, i) => {
      const li = document.createElement("li");
      const hidden = !chart.getDataVisibility(i);
      li.classList.toggle("slice-hidden", hidden);
      const swatch = document.createElement("span");
      swatch.className = "pie-legend-swatch";
      swatch.style.background = colors[i];
      const text = document.createElement("span");
      text.className = "pie-legend-label";
      text.textContent = label;
      const value = Number(data[i]) || 0;
      const share = hidden ? "" : fmtShare(value, total);
      li.title = `${label}：${fmtValue(value)}${share ? `（${share}）` : ""}`;
      li.append(swatch, text);
      li.addEventListener("click", () => {
        chart.toggleDataVisibility(i);
        chart.update();
        renderPieLegend(listId, chart, labels, colors, fmtValue);
      });
      return li;
    })
  );
}

/** 环形图 Chart.js 配置：图例交给画布外的 HTML 列表，画布只画圆环与扇区标注。 */
function doughnutOptions(labelFormatter) {
  return {
    responsive: true,
    maintainAspectRatio: false,
    plugins: {
      legend: { display: false },
      tooltip: {
        callbacks: {
          label(ctx) {
            const total = ctx.dataset.data.reduce(
              (sum, v, i) => sum + (ctx.chart.getDataVisibility(i) ? Number(v) || 0 : 0),
              0
            );
            const share = fmtShare(ctx.parsed, total);
            return ` ${ctx.label}: ${labelFormatter(ctx.parsed)}${share ? `（${share}）` : ""}`;
          },
        },
      },
    },
  };
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
      options: doughnutOptions(fmtTokens),
    });
    // 扇区上标注占比 + 紧凑 token 数；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    tokenChart.$pieSliceLabels = {
      formatter: (value, share) => [share, compactTokens(value)],
    };
  }
  renderPieLegend("legend-tokens", tokenChart, tokenLabels, tokenColors, fmtTokens);

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
      options: doughnutOptions(fmtUsd),
    });
    // 扇区上标注占比 + 金额；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    modelChart.$pieSliceLabels = {
      formatter: (value, share) => [share, fmtUsd(value)],
    };
  }
  renderPieLegend("legend-models", modelChart, modelLabels, modelColors, fmtUsd);

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
  const doughnutLabels = ["输入", "缓存读", "缓存写", "输出"];
  const doughnutColors = [colorFor(0), colorFor(2), colorFor(4), colorFor(1)];
  if (doughnutChart) {
    doughnutChart.data.datasets[0].data = doughnutData;
    doughnutChart.update();
  } else {
    doughnutChart = new Chart(el("#chart-doughnut"), {
      type: "doughnut",
      data: {
        labels: doughnutLabels,
        datasets: [
          {
            data: doughnutData,
            backgroundColor: doughnutColors,
            borderColor: t.border,
            borderWidth: 2,
          },
        ],
      },
      plugins: [pieSliceLabelsPlugin],
      options: doughnutOptions(fmtTokens),
    });
    // 扇区上标注占比 + 紧凑 token 数；配置挂实例属性，不能进 options（scriptable 解析陷阱）
    doughnutChart.$pieSliceLabels = {
      formatter: (value, share) => [share, compactTokens(value)],
    };
  }
  renderPieLegend("legend-doughnut", doughnutChart, doughnutLabels, doughnutColors, fmtTokens);
}

/* ---------- 数据源选择（chips） ---------- */

/**
 * 重建统计对象 chip 行，并清理已失效的选中项（账户已删除 / 已删除记录已清空），
 * 随后按 selectedKeys 重新推导 selection。多选：点击切换选中，「全部总览」= 全选（空集）。
 */
function rebuildChips() {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  const valid = new Set([...cursorAccounts.map((a) => a.id), ...LOCAL_KEYS]);
  if (deletedRecords.length) valid.add(DELETED_SELECTION);
  for (const key of [...selectedKeys]) {
    if (!valid.has(key)) selectedKeys.delete(key);
  }
  syncSelection();

  const chips = [{ value: "all", label: "全部总览", sub: "", kind: null }];
  // 账户 chip：名字为主显示，邮箱作为小字排在下一行（与主显示相同时不重复）
  for (const a of cursorAccounts) {
    const { primary, email } = cursorIdentity(a);
    chips.push({ value: a.id, label: primary, sub: email, kind: a.kind });
  }
  for (const key of LOCAL_KEYS) chips.push({ value: key, label: "本地用量分析", sub: "", kind: LOCAL_SOURCES[key].tag });
  // 已删除账户保留的统计数据：有记录时在末尾给一个「已删除」chip，作为一个整体项参与选择
  if (deletedRecords.length) chips.push({ value: DELETED_SELECTION, label: "已删除", sub: "", kind: "cursor" });

  el("#usage-sources").replaceChildren(
    ...chips.map((c) => {
      const btn = document.createElement("button");
      btn.type = "button";
      const active = c.value === "all" ? selection === "all" : selectedKeys.has(c.value);
      btn.className = `chip-select${c.sub ? " has-sub" : ""}${active ? " active" : ""}`;
      btn.setAttribute("aria-pressed", active ? "true" : "false");
      if (c.kind) {
        const tag = document.createElement("span");
        tag.className = `tag kind-${c.kind}`;
        tag.textContent = kindLabel(c.kind);
        btn.append(tag);
      }
      const label = document.createElement("span");
      label.textContent = c.label;
      if (c.sub) {
        const ident = document.createElement("span");
        ident.className = "chip-ident";
        const sub = document.createElement("span");
        sub.className = "chip-sub";
        sub.textContent = c.sub;
        ident.append(label, sub);
        btn.append(ident);
      } else {
        btn.append(label);
      }
      btn.addEventListener("click", () => selectSource(c.value));
      return btn;
    })
  );
}

function applyVisibility() {
  // 本地来源的目录输入 / 重新扫描只在单选该来源时显示；多选时用默认目录
  for (const key of LOCAL_KEYS) el(LOCAL_SOURCES[key].form).hidden = selection !== key;
  // 各来源账单表：合并视图（全部总览 / 多选 / 已删除）列出参与合并的来源
  el("#usage-overview").hidden = !isMergedView();
  // 「原始账单」「生成快照」仅对单个 Cursor 账户视图开放（合并视图 / 本地分析无对应存档口径）
  el("#usage-raw").hidden = isMergedView() || isLocalSelection();
  el("#usage-snapshot").hidden = isMergedView() || isLocalSelection();
  el("#usage-results").hidden = true;
  el("#usage-skeleton").hidden = true;
  renderedFor = ""; // 结果区已被隐藏，需要重新渲染
  lastAttemptAt = 0;
  clearStatus();
}

/** chip 点击：切换该项选中 / 取消；「全部总览」清空选择（已是总览时再点 = 重新加载）。 */
function selectSource(value) {
  if (value === "all") {
    if (!selectedKeys.size) {
      loadView(false);
      return;
    }
    selectedKeys.clear();
  } else if (selectedKeys.has(value)) {
    selectedKeys.delete(value);
  } else {
    selectedKeys.add(value);
  }
  syncSelection();
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

/**
 * 当前跨度下拉取 Cursor 账户聚合。force 跳过本地缓存并让后端立即增量同步事件库；
 * full 强制全量重拉（仅「刷新」按钮）。
 */
function fetchAggregate(account, { force = false, full = false } = {}) {
  const { start, end } = rangeBounds();
  return fetchCursorAggregate(account, rangeKey(), { start, end, force, full });
}

/** 当前跨度下切片某个已删除账户保留的事件库（纯本地）。 */
function fetchDeletedAggregate(record) {
  const { start, end } = rangeBounds();
  return fetchArchivedAggregate(record.accountId, { start, end });
}

/**
 * 重新拉取已删除账户记录列表（失败时沿用上次结果），并按有无记录重建筛选 chip。
 * 重建可能清掉「已删除」选中项而改变视图（如记录全部清空）：此时切到新视图并返回 true，
 * 调用方不必再继续原视图的加载。
 */
async function refreshDeletedRecords() {
  try {
    const list = await listDeletedUsage();
    deletedRecords = Array.isArray(list) ? list : [];
  } catch (error) {
    console.error("读取已删除账户统计数据列表失败：", error);
  }
  deletedDirty = false;
  const before = selectionKey();
  rebuildChips();
  if (selectionKey() === before) return false;
  applyVisibility();
  if (panelVisible) loadView(false);
  return true;
}

/**
 * 总览行「删除统计数据」：确认后彻底删除该已删除账户保留的事件库，然后重新加载总览
 * （事件库在有效期内，重载只是本地切片，不会联网）。
 */
async function onDeleteUsageData(record) {
  const ok = await confirmDialog({
    title: "删除统计数据",
    body: `确定删除已删除账户“${deletedLabel(record)}”保留的统计数据吗？此操作无法撤销。`,
    confirmText: "删除",
    danger: true,
  });
  if (!ok) return;
  try {
    await removeDeletedUsage(record.accountId);
  } catch (error) {
    setStatus("bad", `删除统计数据失败：${resetError(error)}`);
    return;
  }
  deletedRecords = deletedRecords.filter((r) => r.accountId !== record.accountId);
  overviewResults.delete(record.accountId);
  // 独立 key：随后重载视图时的 clearStatus 只清页面级提示，不吞掉这条结果
  toast("ok", `已删除“${deletedLabel(record)}”保留的统计数据。`, { key: "usage-deleted" });
  // 最后一条记录删掉后「已删除」chip 消失，rebuildChips 会清掉该选中项并切换视图
  const before = selectionKey();
  rebuildChips();
  if (selectionKey() !== before) {
    applyVisibility();
    if (panelVisible) loadView(false);
    return;
  }
  if (!isMergedView() || !activeSources().includeDeleted) return;
  renderedAt = 0;
  lastAttemptAt = 0;
  if (panelVisible) loadView(false);
}

/**
 * 打开某个 Cursor 账户（在用或已删除保留的数据）在当前时间范围内的原始账单弹窗。
 * 在用账户在弹窗里清除本地数据后，由 USAGE_LOCAL_CLEARED_EVENT 统一触发视图重载。
 */
function openRawBill({ accountId, label, deleted }) {
  const { start, end } = rangeBounds();
  void openRawEvents({ accountId, label, deleted, start, end, rangeText: rangeText() });
}

/**
 * 某账户的本地用量数据已被清除（本页弹窗或账户页发起）：该账户参与当前视图时作废其结果并重载
 * （事件库已删，重载会立即重新全量拉取）；页面隐藏时清掉 renderedFor，下次显示自然重载。
 */
function onLocalDataCleared(accountId) {
  const involved = selection === accountId || (isMergedView() && activeSources().cursorAccounts.some((a) => a.id === accountId));
  if (!involved) return;
  overviewResults.delete(accountId);
  renderedAt = 0;
  lastAttemptAt = 0;
  if (panelVisible) loadView(false);
  else renderedFor = "";
}

/** 当前区间对应的本地扫描参数：「全部」不限起止，其余给首日 0 点与末日次日 0 点。 */
function scanParams(home, force) {
  const r = currentRange();
  if (!r) return { days: null, home, force };
  // 起点为区间首日 0 点，终点为末日次日 0 点（开区间）；缓存键与 Cursor 侧一致，由 scanKey 给出
  return {
    key: scanKey(),
    sinceMs: r.startDate.getTime(),
    untilMs: addLocalDays(r.endDate, 1).getTime(),
    home,
    force,
  };
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

  // 一行一个来源：各 Cursor 账户（含已删除账户保留的数据）+ 本地 Codex / Claude 分析，按总 Token 降序；
  // showActual=false 的来源实扣列恒为 —。统计失败但有缓存数据的来源仍显示旧数字（状态列展示错误）。
  // 「数据更新」列为该来源用量数据的获取时间——各来源缓存时间可能不同，逐行展示。
  // 统计中 / 更新中的来源状态格带旋转指示并用强调色，避免与「完成」等静态文案混在一起看不出来。
  // Cursor 账户行：状态格放「原始账单」按钮；已删除账户行：名称旁标「已删除」，
  // 状态格放「原始账单」与「删除统计数据」按钮（actions）。
  const sourceRow = ({
    kind,
    label,
    title,
    deleted,
    stateText,
    stateTitle,
    stateBad,
    statePending,
    at,
    agg,
    showActual,
    planText,
    ratioText,
    actions,
  }) => {
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
    name.title = title || label;
    ident.append(tag, name);
    if (deleted) {
      const deletedTag = document.createElement("span");
      deletedTag.className = "tag deleted";
      deletedTag.textContent = "已删除";
      ident.append(deletedTag);
    }
    nameTd.append(ident);

    const stateTd = document.createElement("td");
    if (statePending) {
      const pending = document.createElement("span");
      pending.className = "cell-pending";
      const spinner = document.createElement("span");
      spinner.className = "spinner";
      spinner.setAttribute("aria-hidden", "true");
      pending.append(spinner, stateText);
      stateTd.append(pending);
    } else {
      stateTd.textContent = stateText;
      if (stateBad) {
        stateTd.className = "cell-bad";
        stateTd.title = stateTitle || stateText;
      }
    }
    // 行内操作用紧凑图标按钮（与账户页操作列同款），含义放悬停提示，避免文字按钮撑长状态格
    for (const action of actions || []) {
      const btn = iconAction(action.icon, action.title || action.label);
      btn.classList.add("overview-action");
      btn.addEventListener("click", action.onClick);
      stateTd.append(btn);
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
      const statePending = !!result && result.state === "pending";
      if (statePending) {
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
      if (agg && price > 0 && !isHourlyMode()) ratioText = `${(agg.totalEquivalentUsd / price).toFixed(1)}×`;
      if (agg && price != null) {
        totals.planUsd += price;
        totals.planEquiv += agg.totalEquivalentUsd;
        totals.planKnown = true;
      }
      const label = accountLabel(a);
      sourceRow({
        kind: "cursor",
        label,
        // 同名账户靠悬停提示里的邮箱区分（与筛选 chip 的副行同一来源）
        title: cursorIdentity(a).email || label,
        stateText,
        stateBad,
        statePending,
        at: result ? result.at : null,
        agg,
        showActual: true,
        planText,
        ratioText,
        actions: [
          {
            icon: "bill",
            label: "原始账单",
            title: "原始账单：查看该账户在当前时间范围内的逐笔用量事件",
            onClick: () => openRawBill({ accountId: a.id, label: accountLabel(a), deleted: false }),
          },
        ],
      });
    } else if (src.kind === "cursor-deleted") {
      // 已删除账户保留的数据：不再更新，状态列只标错误（切片失败）；套餐取删除时保留的信息
      const record = src.record;
      const result = src.result;
      const agg = result && result.agg ? result.agg : null;
      const stateBad = !!result && result.state === "error";
      const membership = record.membershipType || null;
      const planLabel = membership ? membershipLabel(membership) : "";
      const price = planMonthlyUsd(membership);
      const planText = planLabel ? (price != null ? `${planLabel} · $${price}` : planLabel) : "—";
      let ratioText = "—";
      if (agg && price > 0 && !isHourlyMode()) ratioText = `${(agg.totalEquivalentUsd / price).toFixed(1)}×`;
      if (agg && price != null) {
        totals.planUsd += price;
        totals.planEquiv += agg.totalEquivalentUsd;
        totals.planKnown = true;
      }
      sourceRow({
        kind: "cursor",
        label: deletedLabel(record),
        deleted: true,
        // 读取失败时只显示短文案，完整错误放悬停提示，避免撑开 / 裁掉旁边的按钮
        stateText: stateBad ? "读取失败" : `数据截至 ${fmtDeletedAt(record.deletedAt)}`,
        stateTitle: stateBad ? result.error : "",
        stateBad,
        statePending: false,
        at: result ? result.at : null,
        agg,
        showActual: true,
        planText,
        ratioText,
        actions: [
          {
            icon: "bill",
            label: "原始账单",
            title: "原始账单：查看该账户保留数据在当前时间范围内的逐笔用量事件",
            onClick: () => openRawBill({ accountId: record.accountId, label: deletedLabel(record), deleted: true }),
          },
          {
            icon: "delete",
            label: "删除统计数据",
            title: "删除统计数据：彻底删除该已删除账户保留的统计数据",
            onClick: () => void onDeleteUsageData(record),
          },
        ],
      });
    } else {
      // 本地 Codex / Claude 分析行：无论有无对应账户都展示；本地日志无账户归属，无套餐可比
      const local = src.result;
      const localAgg = local && local.scan ? local.scan.aggregate : null;
      let localState = "—";
      let localBad = false;
      const localPending = !!local && local.state === "pending";
      if (localPending) {
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
        statePending: localPending,
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
      totals.planUsd > 0 && !isHourlyMode() ? `${(totals.planEquiv / totals.planUsd).toFixed(1)}×` : "—";
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
 * 参与合并的来源由 activeSources 决定（全部总览 / 多选子集 / 仅已删除账户）。
 * 返回合并结果，无任何可用数据时返回 null（不主动隐藏结果区）。
 */
function renderOverviewMerged() {
  const active = activeSources();
  const cursorAccounts = active.cursorAccounts;
  const aggs = [];
  const ats = [];
  let planUsd = 0;
  let planEquiv = 0;
  let planKnown = false;
  let cursorCount = 0;
  let deletedCount = 0;
  const takeCursor = (r, membership) => {
    aggs.push(r.agg);
    if (Number.isFinite(r.at)) ats.push(r.at);
    const price = planMonthlyUsd(membership);
    if (price != null) {
      planUsd += price;
      planEquiv += r.agg.totalEquivalentUsd;
      planKnown = true;
    }
  };
  for (const a of cursorAccounts) {
    const r = overviewResults.get(a.id);
    if (!r || !r.agg) continue;
    cursorCount += 1;
    takeCursor(r, a.status ? a.status.membershipType : null);
  }
  // 已删除账户保留的数据：与账单表同一口径，只计入所选跨度内有用量的
  if (active.includeDeleted) {
    for (const record of deletedRecords) {
      const r = overviewResults.get(record.accountId);
      if (!r || !hasUsage(r.agg)) continue;
      deletedCount += 1;
      takeCursor(r, record.membershipType || null);
    }
  }
  const localNames = [];
  for (const key of active.localKeys) {
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
  if (cursorCount && deletedCount) {
    sources.push(`${cursorCount + deletedCount} 个 Cursor 账户（含 ${deletedCount} 个已删除）`);
  } else if (cursorCount) {
    sources.push(`${cursorCount} 个 Cursor 账户`);
  } else if (deletedCount) {
    sources.push(`${deletedCount} 个已删除 Cursor 账户`);
  }
  sources.push(...localNames);
  // 单日跨度：单日费用对比套餐月费无意义，隐藏月费倍数卡片；柱图切为 24 小时分布
  const tail = isHourlyMode()
    ? hourlyTail()
    : "倍数 = 等价费用 ÷ 套餐月费（仅计入 Cursor 账户，按「月」查看时最具参考性）";
  const title = selection === "all" ? "全部总览" : isDeletedSelection() ? "已删除账户" : "所选来源";
  renderAggregate(merged, {
    showActual: true,
    plan: planKnown && !isHourlyMode() ? { monthlyUsd: planUsd, equivalentUsd: planEquiv } : null,
    metaText: `${title} · ${rangeText()} · ${sources.join(" + ")} 合并 · 等价费用按官方 API 价折算，实扣为 Cursor 实际计费；${tail}。`,
    dailySources: collectDailySources(),
  });
  // 合并视图的数据时间取最早的来源时间（保守口径）
  markUpdated(ats.length ? Math.min(...ats) : Date.now());
  return merged;
}

// 上次总览加载的跨度签名：跨度切换后不复用上一跨度的内存结果作种子（口径不同会串数）
let overviewSpanSig = "";

/**
 * 已删除账户保留的数据：没有 localStorage 缓存，切片是纯本地操作——同跨度的上轮结果作种子
 * （previous），随后为每条记录发起切片，每条完成后重绘账单表与合并结果。总览与「已删除」视图共用。
 */
function startDeletedJobs(previous, seq) {
  for (const record of deletedRecords) {
    const prev = previous.get(record.accountId);
    overviewResults.set(record.accountId, {
      state: "pending",
      agg: (prev && prev.agg) || null,
      at: prev ? prev.at : record.syncedAt,
    });
  }
  return deletedRecords.map((record) =>
    fetchDeletedAggregate(record)
      .then((entry) => {
        overviewResults.set(record.accountId, { state: "ok", agg: entry.agg, at: entry.at });
      })
      .catch((error) => {
        const prev = overviewResults.get(record.accountId) || {};
        overviewResults.set(record.accountId, {
          state: "error",
          error: resetError(error),
          agg: prev.agg,
          at: prev.at,
        });
      })
      .then(() => {
        if (seq !== loadSeq) return;
        renderOverviewTable();
        renderOverviewMerged();
      })
  );
}

/** 已删除账户切片失败的条数（状态提示用）。 */
function deletedFailedCount() {
  return deletedRecords.filter((record) => {
    const r = overviewResults.get(record.accountId);
    return r && r.state === "error";
  }).length;
}

/**
 * 合并视图加载：全部总览 / 多选子集 / 仅已删除账户共用。参与来源由 activeSources 决定，
 * 每个来源完成后立即合并重绘；「已删除」是整体项，包含全部已删除账户保留的数据。
 */
async function loadOverview(force, seq) {
  const startKey = selectionKey();
  const { cursorAccounts, localKeys, includeDeleted } = activeSources();
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
  for (const key of localKeys) {
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

  // 已删除账户保留的数据：账户增删后先刷新记录列表（放在缓存种子渲染之后，不耽误首屏）。
  // 刷新会重建 chip 并清理失效选中项（如已删除记录全部清空），视图身份变了就改走新视图的加载
  let deletedJobs = [];
  if (includeDeleted) {
    if (deletedDirty) {
      const switched = await refreshDeletedRecords();
      if (switched || seq !== loadSeq || selectionKey() !== startKey) return;
    }
    deletedJobs = startDeletedJobs(previous, seq);
  }

  // 2) 本地 Codex / Claude 扫描与各 Cursor 账户并行拉取，每个来源完成后立即合并重绘
  //   （有效期内的来源在 usage_data 中直接命中缓存，不会走网络）
  const scanJobs = localKeys.map((key) =>
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

  // Cursor 账户：「刷新」按钮（force）强制全量重拉事件库，其余走后端 auto 模式
  let next = 0;
  const lane = async () => {
    while (next < cursorAccounts.length) {
      const acc = cursorAccounts[next];
      next += 1;
      try {
        const entry = await fetchAggregate(acc, { force, full: force });
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
    ...deletedJobs,
  ]);
  if (seq !== loadSeq) return;

  renderOverviewTable();
  const merged = renderOverviewMerged();
  const failed = cursorAccounts.filter((a) => {
    const r = overviewResults.get(a.id);
    return r && r.state === "error";
  }).length;
  const deletedFailed = includeDeleted ? deletedFailedCount() : 0;
  const deletedShown =
    includeDeleted && deletedRecords.some((record) => hasUsage((overviewResults.get(record.accountId) || {}).agg));
  const localFailed = localKeys.filter((key) => {
    const local = overviewResults.get(key);
    return local && local.state === "error";
  });

  if (merged) {
    const warns = [];
    if (failed) warns.push(`${failed} 个 Cursor 账户统计失败`);
    if (deletedFailed) warns.push(`${deletedFailed} 个已删除账户的保留数据读取失败`);
    for (const key of localFailed) warns.push(`${LOCAL_SOURCES[key].mergedName} 扫描失败`);
    if (warns.length) setStatus("warn", `${warns.join("；")}，明细见上表；失败来源如有上次数据则继续显示。`);
    else if (merged.models.length === 0) setStatus("warn", "该时间范围内没有用量记录。");
    else if (selection === "all" && !cursorAccounts.length && !deletedShown) {
      setStatus("", "尚未添加 Cursor 账户，总览目前仅含本地 ChatGPT / Claude 用量。");
    } else clearStatus();
    return;
  }
  // 什么都没合并出来：区分「没有来源」「全部失败」与「所选范围内确实没有用量」
  const sourceCount = cursorAccounts.length + localKeys.length + (includeDeleted ? deletedRecords.length : 0);
  if (!sourceCount) setStatus("", isDeletedSelection() ? "没有已删除账户保留的统计数据。" : "没有可统计的来源。");
  else if (failed + deletedFailed + localFailed.length >= sourceCount) {
    setStatus("bad", "全部来源统计失败，请检查 Token 与本地会话目录。");
  } else setStatus("warn", "该时间范围内没有用量记录。");
}

function renderCursorAccount(account, agg, at) {
  const price = planMonthlyUsd(account.status ? account.status.membershipType : null);
  const tail = isHourlyMode() ? hourlyTail() : "倍数 = 等价费用 ÷ 套餐月费";
  renderAggregate(agg, {
    showActual: true,
    plan:
      price != null && !isHourlyMode()
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
    const entry = await fetchAggregate(account, { force, full: force });
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
  setLoadingHint(true);
  try {
    if (isMergedView()) {
      await loadOverview(force, seq);
    } else if (isLocalSelection()) {
      await loadLocalScan(selection, force, seq);
    } else {
      const account = currentAccount();
      if (!account) {
        selectedKeys.clear();
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
      setLoadingHint(false);
      // 全部失败等场景什么都没渲染出来，不能让骨架屏永远转下去
      if (el("#usage-results").hidden) el("#usage-skeleton").hidden = true;
      // 记录本轮加载完成时间：失败来源不再把整页拖成永久过期（见 isRenderedFresh）
      lastAttemptAt = Date.now();
    }
  }
}

/** 统计进行中的界面提示：窗口顶部悬浮胶囊（旋转指示 + 文案）显隐，刷新按钮图标同步旋转。 */
function setLoadingHint(on) {
  el("#usage-loading").hidden = !on;
  el("#usage-refresh").classList.toggle("busy", on);
}

/**
 * 统一加载入口：发起加载后按同步段结果决定骨架屏与提示文案——
 * loadCurrent 的同步段会用缓存种子立即渲染（stale-while-revalidate），
 * 走完后结果区仍隐藏说明新视图 / 新跨度无任何缓存，露出骨架占位并提示「统计中…」；
 * 已有旧数据在展示则提示「更新中…」。首个结果渲染（renderAggregate）时骨架自动隐藏。
 */
function loadView(force) {
  void loadCurrent(!!force);
  const nothingShown = el("#usage-results").hidden;
  el("#usage-skeleton").hidden = !nothingShown;
  el("#usage-loading-text").textContent = nothingShown ? "统计中…" : "更新中…";
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
  const active = activeSources();
  for (const a of active.cursorAccounts) take(a.id, getCachedAgg(a), "agg");
  for (const key of active.localKeys) take(key, getCachedLocal(key, null), "scan");
  return changed;
}

/**
 * 用共享缓存里的最新条目原地重绘当前视图，不发起任何拉取——后台预取 / 托盘写入到本页的
 * 唯一落点，其它来源不会因此被顺带重新统计。结果区未展示当前视图（隐藏中 / 跨零点后键
 * 变化）时不动，交给随后的 loadView 走常规新鲜度判断。
 */
function rerenderFromCache() {
  if (renderedFor !== selectionKey()) return;
  if (isDeletedSelection()) return; // 数据不在共享缓存里，无需重绘
  if (isMergedView()) {
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
  // 合并视图可见时把预取中的来源标成「更新中…」（不在所选集合里的来源没有条目，标不上，天然跳过）
  const canMark = panelVisible && !loading && isMergedView() && renderedFor === selectionKey();
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
    // 预取只增量同步事件库（sync），不走「刷新」按钮的全量重拉
    enqueue(a.id, fetchAggregate(a, { force: true }));
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
  // 「已删除」视图的数据只来自本地事件库，不受共享缓存影响
  if (isDeletedSelection()) return false;
  if (!key || !key.startsWith(USAGE_CACHE_PREFIX)) return true; // 通配 / 整体清理保守处理
  const rest = key.slice(USAGE_CACHE_PREFIX.length);
  if (rest.startsWith("agg:")) {
    const suffix = `:${rangeKey()}`;
    if (isLocalSelection() || !rest.endsWith(suffix)) return false;
    if (selection === "all") return true;
    // 键形如 agg:<accountId>:<rangeKey>：只有该账户在当前视图里才需要重绘
    const accountId = rest.slice("agg:".length, rest.length - suffix.length);
    return isMultiSelection() ? selectedKeys.has(accountId) : accountId === selection;
  }
  for (const localKey of LOCAL_KEYS) {
    const prefix = LOCAL_SOURCES[localKey].cachePrefix;
    if (!rest.startsWith(prefix)) continue;
    const included = selection === "all" || selection === localKey || (isMultiSelection() && selectedKeys.has(localKey));
    if (!included) return false;
    return rest.slice(prefix.length).startsWith(`${scanKey()}:`);
  }
  return true;
}

/* ---------- 初始化 ---------- */

/** 把周期按钮、区间导航与自定义日期输入同步到当前范围状态。 */
function syncRangeControls() {
  for (const btn of el("#usage-period").querySelectorAll("[data-period]")) {
    const on = btn.dataset.period === period;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-selected", on ? "true" : "false");
  }
  const range = currentRange();
  el("#usage-range-nav").hidden = !range;
  el("#usage-range-label").textContent = rangeText();
  // 不能滑到未来：区间末日已到今天时禁用「下一个」
  el("#usage-range-next").disabled = !range || range.endDate.getTime() >= dayStartMs(0);
  const custom = period === "custom";
  el("#usage-range-custom").hidden = !custom;
  if (custom) {
    el("#usage-range-start").value = localYmd(customStart);
    el("#usage-range-end").value = localYmd(customEnd);
    el("#usage-range-end").max = localYmd();
    el("#usage-range-start").max = localYmd();
  }
}

/** 范围变化后的统一收尾：先隐藏结果区再加载（有缓存立即重绘，无缓存露骨架），不残留旧口径的数字。 */
function applyRangeChange() {
  syncRangeControls();
  applyVisibility();
  loadView(false);
}

/** 主窗口从托盘重新打开时回到默认范围（日 / 今天）；已是默认范围则不动。 */
function resetRangeToToday() {
  const today = dayStartMs(0);
  if (period === "day" && anchor.getTime() === today) return;
  period = "day";
  anchor = new Date(today);
  syncRangeControls();
  applyVisibility();
  if (panelVisible) loadView(false);
}

/** 切换周期：日 / 周 / 月以今天所在周期为起点，自定义沿用上次的起止日期。 */
function setPeriod(next) {
  if (!PERIODS.includes(next) || next === period) return;
  period = next;
  if (next !== "custom" && next !== "all") anchor = new Date(dayStartMs(0));
  applyRangeChange();
}

/** 左右滑动：日 / 周 / 月移动一个周期，自定义按区间长度平移；不越过今天。 */
function shiftRange(direction) {
  const range = currentRange();
  if (!range) return;
  const today = dayStartMs(0);
  if (direction > 0 && range.endDate.getTime() >= today) return;
  if (period === "day") anchor = addLocalDays(anchor, direction);
  else if (period === "week") anchor = addLocalDays(anchor, 7 * direction);
  else if (period === "month") anchor = new Date(anchor.getFullYear(), anchor.getMonth() + direction, 1);
  else {
    const days = rangeDays();
    let start = addLocalDays(customStart, days * direction);
    let end = addLocalDays(customEnd, days * direction);
    // 向后平移不越过今天：末日贴到今天，首日保持区间长度
    if (end.getTime() > today) {
      end = new Date(today);
      start = addLocalDays(end, -(days - 1));
    }
    customStart = start;
    customEnd = end;
  }
  applyRangeChange();
}

/** 自定义查询：读取两个日期输入，起止颠倒时自动交换，末日不晚于今天。 */
function applyCustomRange() {
  const startText = el("#usage-range-start").value;
  const endText = el("#usage-range-end").value;
  if (!startText || !endText) {
    setStatus("warn", "请选择开始与结束日期。");
    return;
  }
  let start = dayStart(parseYmd(startText));
  let end = dayStart(parseYmd(endText));
  if (!Number.isFinite(start.getTime()) || !Number.isFinite(end.getTime())) {
    setStatus("warn", "日期无效。");
    return;
  }
  if (start.getTime() > end.getTime()) [start, end] = [end, start];
  const today = new Date(dayStartMs(0));
  if (end.getTime() > today.getTime()) end = today;
  if (start.getTime() > end.getTime()) start = end;
  customStart = start;
  customEnd = end;
  period = "custom";
  applyRangeChange();
}

export function initUsage() {
  el("#usage-refresh").addEventListener("click", () => {
    // 统一刷新：重新统计当前视图，并顺带刷新视图相关账户的状态（合并视图取参与合并的账户）
    loadView(true);
    const ids = isMergedView()
      ? activeSources().cursorAccounts.map((a) => a.id)
      : currentAccount()
      ? [selection]
      : [];
    if (ids.length) void refreshAccounts(ids);
  });
  // 原始账单：单账户视图下查看该账户在当前时间范围内的逐笔用量事件（弹窗见 raw_events.js）
  el("#usage-raw").addEventListener("click", () => {
    const account = currentAccount();
    if (account) openRawBill({ accountId: account.id, label: accountLabel(account), deleted: false });
  });
  window.addEventListener(USAGE_LOCAL_CLEARED_EVENT, (event) => {
    const id = event.detail && event.detail.accountId;
    if (id) onLocalDataCleared(id);
  });
  // 价格表变更（保存 / 重置 / 在线更新）：共享缓存已被 notifyPricingChanged 清空，
  // 当前视图的等价费用是旧价，作废新鲜度后整体重算（后端只按新价重新切片，不联网）
  window.addEventListener(USAGE_PRICING_CHANGED_EVENT, () => {
    renderedAt = 0;
    lastAttemptAt = 0;
    if (panelVisible) loadView(false);
    else renderedFor = "";
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
  // 时间范围：周期按钮 + 左右滑动 + 自定义起止日期（默认「日」= 今天）
  el("#usage-period").addEventListener("click", (event) => {
    const btn = event.target instanceof Element ? event.target.closest("[data-period]") : null;
    if (btn) setPeriod(btn.dataset.period);
  });
  el("#usage-range-prev").addEventListener("click", () => shiftRange(-1));
  el("#usage-range-next").addEventListener("click", () => shiftRange(1));
  el("#usage-range-apply").addEventListener("click", () => applyCustomRange());
  for (const id of ["#usage-range-start", "#usage-range-end"]) {
    el(id).addEventListener("keydown", (event) => {
      if (event.key === "Enter") applyCustomRange();
    });
  }
  syncRangeControls();
  // 主窗口关闭到托盘后再打开：时间范围回到今天
  listen("main-window-shown", () => resetRangeToToday()).catch(() => {
    /* 非 Tauri 环境无事件桥 */
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
      // 清理已删除账户的聚合缓存（内存 + localStorage）；保留的统计数据由后端事件库承载，
      // 下次加载总览时重新读取已删除记录列表（删除时保留 / 重新添加后接管都会改变该列表）
      purgeMissingAccounts(list);
      deletedDirty = true;
      // 含已删除账户的合并视图会在随后的加载里重读列表；其它视图这里直接刷新，让「已删除」chip 及时出现
      if (!isMergedView() || !activeSources().includeDeleted) void refreshDeletedRecords();
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
    // rebuildChips 会清掉已不存在账户的选中项并重新推导 selection；视图类型变了就切过去
    const before = selection;
    rebuildChips();
    if (selection !== before) {
      applyVisibility();
      if (panelVisible) loadView(false);
      return;
    }
    if (!panelVisible || loading) return;
    // 账户状态更新时同步账单表中的套餐信息
    if (isMergedView()) renderOverviewTable();
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

  // 已删除账户保留的统计数据集合有变化（重新添加同一账号后接管 / 备份恢复）：
  // 该通知在 accounts-changed 之后到达，此时按旧列表发起的总览加载可能仍在进行，
  // 直接重新加载（作废在途结果；在途请求由 usage_data 的 in-flight 表去重，不会重复联网）
  listen("usage-archive-changed", () => {
    deletedDirty = true;
    // 含已删除账户的合并视图当场作废重载（加载时会重读记录列表并重建 chip）；
    // 其它视图只刷新列表让「已删除」chip 及时出现或消失，切回时 applyVisibility 会清空 renderedFor 自然重载
    if (!isMergedView() || !activeSources().includeDeleted) {
      void refreshDeletedRecords();
      return;
    }
    renderedAt = 0;
    lastAttemptAt = 0;
    if (panelVisible) loadView(false);
  }).catch(() => {
    /* 非 Tauri 环境无事件桥 */
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
  // 启动即读取已删除记录列表，让「已删除」chip 不必等到首次打开总览才出现
  void refreshDeletedRecords();
}
