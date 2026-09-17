import { el, invoke, listen, resetError, fmtInt, fmtTokens, fmtUsd, compactTokens, fmtShare, colorFor, chartAnimMs, setChartHoverHit, bindChartHoverLeave, pieSliceLabelsPlugin, setupDesktopGuards, kindLabel } from "./shared.js";
import { membershipLabel, codexPlanLabel, claudePlanLabel, relativeFromUnixSeconds, relativeFromMs, remainInfo, onDemandBrief, creditsBrief, resetCreditsBrief, maskToken, cursorIdentity, compareAccounts } from "./account_format.js";
import {
  USAGE_CACHE_PREFIX,
  USAGE_CACHE_EVENT,
  USAGE_CACHE_ORIGIN,
  DEFAULT_USAGE_TTL_MS,
  setUsageCacheTtlMs,
  isUsageCacheFresh,
  localYmd,
  dayStartMs,
  addLocalDays,
  todayRangeKey,
  dayRangeKey,
  peekAggForDay,
  peekAggForPastDay,
  peekAggSeries,
  peekScanForDay,
  peekScanForPastDay,
  peekScanSeries,
  peekClaudeScanForDay,
  peekClaudeScanForPastDay,
  peekClaudeScanSeries,
  fetchCursorAggregate,
  fetchCodexScan,
  fetchClaudeScan,
  sliceDay,
  modelsOnDay,
  forgetUsageCacheFromEvent,
} from "./usage_data.js";

// 托盘面板：托盘图标单击弹出的简易面板，分四个 tab：
// 总览（所选日的 token 用量合计，按天前后滑动）/ Cursor / ChatGPT / Claude（账户列表，
// 查看状态额度、单账户刷新 / 一键切换、全部刷新），增删改等完整功能在主窗口。
// 窗口失焦即隐藏（Rust 侧处理），每次获得焦点时重载数据。

let accounts = [];
let refreshing = false;
let switching = false;
let overviewLoading = false;
const refreshingIds = new Set(); // 本面板发起、进行中的单账户刷新
// 其它窗口（主窗口、定时刷新）发起的刷新：由后端 account-refreshing 事件同步，用于行内「刷新中」与总览忙碌提示
const remoteRefreshingIds = new Set();

/** 该账户是否正在刷新（本面板或其它窗口发起）。 */
function isRefreshing(id) {
  return refreshingIds.has(id) || remoteRefreshingIds.has(id);
}
// 总览所选统计日（本地 0 点毫秒），左右箭头按天前后滑动，不越过今天；仅内存保存（应用重启回到今天）。
// 今天用 today:YYYY-MM-DD 键，其它日子用完整自然日键 day:YYYY-MM-DD，与主窗口用量页「日」周期共用缓存。
let trayDayMs = dayStartMs(0);

function dayIsPast() {
  return trayDayMs < dayStartMs(0);
}
/** 所选统计日的口径：本地 0 点起止（终点为次日 0 点）、YYYY-MM-DD、文案用词。 */
function dayScope() {
  const start = trayDayMs;
  const end = addLocalDays(new Date(start), 1).getTime();
  const ymd = localYmd(new Date(start));
  const past = dayIsPast();
  const yesterday = start === dayStartMs(-1);
  const d = new Date(start);
  const word = !past ? "今日" : yesterday ? "昨日" : `${d.getMonth() + 1}月${d.getDate()}日`;
  return {
    past,
    start,
    end,
    ymd,
    aggKey: past ? dayRangeKey(ymd) : todayRangeKey(ymd),
    word,
    tipWord: !past ? "今天" : yesterday ? "昨天" : word,
  };
}

// 与主窗口账户表操作列同一套线性图标
const TRAY_ICONS = {
  refresh: '<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>',
  switch: '<path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><polyline points="17 11 19 13 23 9"/>',
};

function iconAction(icon, label) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "icon-action";
  btn.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${TRAY_ICONS[icon]}</svg>`;
  btn.title = label;
  btn.setAttribute("aria-label", label);
  return btn;
}

/**
 * 托盘紧凑账户身份：label 优先可读身份，自动生成的 Cursor user_id 采用两端保留的省略格式；
 * email 仅 Cursor 账户有（与主显示相同时为空串），账户行把它作为主显示后的小字。
 */
function trayIdentity(account) {
  const note = String((account && account.note) || "").trim();
  if (!account || account.kind !== "cursor") return { label: note || "未命名账户", email: "" };
  const { primary, email } = cursorIdentity(account);
  const looksLikeCredential = primary.startsWith("user_") || primary.includes("::") || primary.split(".").length === 3;
  return { label: looksLikeCredential ? maskToken(primary) : primary, email };
}

function trayAccountLabel(account) {
  return trayIdentity(account).label;
}

function syncHeaderRefresh() {
  const btn = el("#tray-refresh");
  // 其它窗口正在刷新账户时同样视为忙碌：按钮转圈、不允许再排一轮
  const remoteBusy = remoteRefreshingIds.size > 0;
  const accountBusy = refreshing || switching || refreshingIds.size > 0 || remoteBusy;
  if (trayTab === "overview") {
    // 总览刷新 = 账户状态 + 所选日用量一起刷，任一进行中都置忙
    btn.disabled = overviewLoading || refreshing || switching || remoteBusy;
    btn.classList.toggle("busy", overviewLoading || refreshing || remoteBusy);
  } else {
    btn.disabled = accountBusy || !accounts.length;
    btn.classList.toggle("busy", refreshing || remoteBusy);
  }
  syncOverviewLoading();
}

/**
 * 总览的进行中提示：与主窗口统计页同款的顶部悬浮胶囊。所选日用量在拉取、本面板或其它窗口
 * 在刷新账户状态时显示；已有数据在展示则文案为「更新中…」，否则「统计中…」。
 */
function syncOverviewLoading() {
  const busy = trayTab === "overview" && (overviewLoading || refreshing || remoteRefreshingIds.size > 0);
  el("#tray-ov-loading").hidden = !busy;
  if (busy) el("#tray-ov-loading-text").textContent = overviewDom ? "更新中…" : "统计中…";
}

/** 总览头部的数据时间：「更新于 HH:MM」，超过缓存有效期加「（缓存）」；无数据时隐藏。 */
function setOverviewUpdated(statAtMs) {
  const node = el("#tray-ov-updated");
  if (!Number.isFinite(statAtMs) || statAtMs <= 0) {
    node.hidden = true;
    node.textContent = "";
    return;
  }
  const time = new Date(statAtMs).toLocaleTimeString("zh-CN", { hour12: false, hour: "2-digit", minute: "2-digit" });
  node.textContent = `更新于 ${time}${isUsageCacheFresh(statAtMs) ? "" : "（缓存）"}`;
  node.title = new Date(statAtMs).toLocaleString("zh-CN", { hour12: false });
  node.hidden = false;
}

/* ---------- Tab 状态 ---------- */

const TAB_KEY = "trayTab";

function readTab() {
  try {
    const v = localStorage.getItem(TAB_KEY);
    return v === "cursor" || v === "codex" || v === "claude" || v === "overview" ? v : "overview";
  } catch {
    return "overview";
  }
}

let trayTab = readTab();

function applyTab() {
  for (const btn of document.querySelectorAll(".tray-tab")) {
    btn.classList.toggle("active", btn.dataset.tab === trayTab);
  }
  el("#tray-list").hidden = trayTab === "overview";
  el("#tray-overview").hidden = trayTab !== "overview";
  syncHeaderRefresh();
}

function setTab(tab) {
  if (tab === trayTab) return;
  trayTab = tab;
  try { localStorage.setItem(TAB_KEY, tab); } catch { /* ignore */ }
  applyTab();
  // 切到总览属于主动查看：数据超过 5 分钟就强制同步
  if (tab === "overview") void loadOverview({ viewing: true });
  else render();
}

/* ---------- 状态提示 ---------- */

function setStatus(kind, text) {
  const box = el("#tray-status");
  const msg = document.createElement("span");
  msg.className = "tray-status-text";
  msg.textContent = text;
  const close = document.createElement("button");
  close.type = "button";
  close.className = "tray-status-close";
  close.setAttribute("aria-label", "关闭提示");
  close.textContent = "×";
  close.addEventListener("click", () => clearStatus());
  box.className = `tray-status ${kind}`;
  box.title = text;
  box.replaceChildren(msg, close);
  box.hidden = false;
}

function clearStatus() {
  const box = el("#tray-status");
  box.hidden = true;
  box.title = "";
  box.replaceChildren();
}

/* ---------- 渲染 ---------- */

function tierClass(remaining) {
  if (remaining <= 15) return "q-low";
  if (remaining <= 50) return "q-warn";
  return "q-ok";
}

/** 迷你进度条（无动画）：标签 + 剩余百分比填充 + 数值，颜色按剩余量分档。 */
function miniBar(label, usedPercent) {
  const used = Number(usedPercent);
  if (!Number.isFinite(used)) return null;
  const remaining = Math.min(100, Math.max(0, 100 - used));
  const wrap = document.createElement("span");
  wrap.className = `tray-bar ${tierClass(remaining)}`;
  wrap.title = `${label} 剩余 ${Math.round(remaining)}%`;
  const name = document.createElement("span");
  name.className = "tray-bar-label";
  name.textContent = label;
  const track = document.createElement("span");
  track.className = "tray-bar-track";
  const fill = document.createElement("span");
  fill.style.width = `${remaining}%`;
  track.append(fill);
  const value = document.createElement("span");
  value.className = "tray-bar-val";
  value.textContent = `${Math.round(remaining)}%`;
  wrap.append(name, track, value);
  return wrap;
}

function barsFor(account) {
  const status = account.status || null;
  if (!status || status.alive === false) return [];
  const bars = [];
  if (account.kind !== "cursor") {
    // Codex / Claude 的额度窗口结构一致（windows: label / usedPercent）
    const windows = Array.isArray(status.windows) ? status.windows : [];
    for (const w of windows.slice(0, 2)) {
      const bar = w && miniBar(w.label || "窗口", w.usedPercent);
      if (bar) bars.push(bar);
    }
  } else {
    const plan = status.plan || {};
    const auto = miniBar("Auto", plan.autoPercentUsed);
    const api = miniBar("API", plan.apiPercentUsed);
    if (auto) bars.push(auto);
    if (api) bars.push(api);
    // Sand 周额度（Cursor 对 Grok Bot 额度的内部代号）：套餐包含时展示，
    // 额度耗尽按 100% 已用处理（与主窗口一致）
    const sand = status.sand || null;
    if (sand && sand.included) {
      let usedPct = Number(sand.usagePercent);
      if (sand.hasAvailableUsage === false) usedPct = 100;
      const sandBar = miniBar("Sand", usedPct);
      if (sandBar) bars.push(sandBar);
    }
  }
  return bars;
}

function accountRow(account) {
  const row = document.createElement("div");
  row.className = "tray-row";

  const dot = document.createElement("span");
  const alive = account.status && typeof account.status.alive === "boolean" ? account.status.alive : null;
  dot.className = `dot ${alive === true ? "ok" : alive === false ? "bad" : "unknown"}`;

  const main = document.createElement("div");
  main.className = "tray-main";
  const noteLine = document.createElement("div");
  noteLine.className = "tray-note";
  const note = document.createElement("span");
  note.className = "tray-note-text";
  const { label, email } = trayIdentity(account);
  note.textContent = label;
  note.title = label;
  noteLine.append(note);
  if (email) {
    const emailSpan = document.createElement("span");
    emailSpan.className = "tray-note-email";
    emailSpan.textContent = email;
    emailSpan.title = email;
    noteLine.append(emailSpan);
  }
  main.append(noteLine);

  // 副行：套餐 · 有效期 · 超额/余额 · 上次刷新时间 ·（已失效）
  const sub = document.createElement("div");
  sub.className = "tray-sub";
  const status = account.status || null;
  const plan = account.kind === "codex"
    ? codexPlanLabel(status && status.plan)
    : account.kind === "claude"
    ? claudePlanLabel(status && status.plan)
    : membershipLabel(status && status.membershipType);
  const parts = [];
  if (plan) parts.push(plan);
  // 有效期与主窗口套餐列口径一致：Cursor = 本期计费周期截止，Codex = 套餐订阅到期；
  // Claude 接口不提供订阅起止，不展示有效期
  const endIso = status && status.alive === true && account.kind !== "claude"
    ? (account.kind === "codex" ? status.planActiveUntil : status.billingCycleEnd)
    : null;
  const endMs = endIso ? Date.parse(endIso) : NaN;
  const titles = [];
  if (Number.isFinite(endMs)) {
    const exp = remainInfo(endMs);
    parts.push(exp.expired ? "已到期" : `有效期 ${exp.text}`);
    const endText = new Date(endMs).toLocaleDateString("zh-CN");
    const renewHint = account.kind === "codex" && status.planWillRenew === true
      ? "（到期自动续期）"
      : account.kind === "codex" && status.planWillRenew === false
      ? "（到期不续期）"
      : "";
    titles.push(account.kind === "codex"
      ? `订阅至 ${endText}${renewHint}`
      : `本期计费周期截止 ${endText}（到期自动续期，额度重置）`);
  }
  const extra = account.kind === "codex"
    ? creditsBrief(status)
    : account.kind === "cursor"
    ? onDemandBrief(status)
    : null; // Claude 无超额 / 余额概念
  if (extra) {
    parts.push(extra.text);
    if (extra.title) titles.push(extra.title);
  }
  // ChatGPT 剩余额度重置次数（接口提供时才有）
  const resets = account.kind === "codex" ? resetCreditsBrief(status) : null;
  if (resets) {
    parts.push(resets.text);
    titles.push(resets.title);
  }
  parts.push(`刷新于 ${relativeFromUnixSeconds(account.lastRefreshAt)}`);
  // ChatGPT：凭据组最后换新时间（来自账户保存的 auth.json 副本）
  const credAt = Number(account.codexAuthRefreshedAt);
  if (account.kind === "codex" && Number.isFinite(credAt) && credAt > 0) {
    parts.push(`凭据 ${relativeFromMs(credAt)}`);
    titles.push(`凭据组（access / refresh token）最后换新：${new Date(credAt).toLocaleString("zh-CN", { hour12: false })}`);
  }
  const info = document.createElement("span");
  info.textContent = parts.join(" · ");
  if (titles.length) info.title = titles.join("\n");
  sub.append(info);
  if (alive === false) {
    const bad = document.createElement("span");
    bad.className = "q-low";
    bad.textContent = "已失效";
    sub.append(bad);
  }
  main.append(sub);

  // 额度行：迷你进度条单行并排（Cursor 与 Codex 结构一致）
  const bars = barsFor(account);
  if (bars.length) {
    const barsRow = document.createElement("div");
    barsRow.className = "tray-bars";
    barsRow.append(...bars);
    main.append(barsRow);
  }

  row.append(dot, main);

  const actions = document.createElement("div");
  actions.className = "tray-actions";
  const refreshBusy = isRefreshing(account.id);
  const rowBusy = refreshing || switching || refreshBusy;

  const refreshBtn = iconAction("refresh", refreshBusy ? "刷新中…" : "刷新");
  refreshBtn.classList.toggle("busy", refreshBusy);
  refreshBtn.disabled = rowBusy;
  refreshBtn.addEventListener("click", () => { void refreshOne(account.id); });

  // 仅已验证有效的账户可切换（Codex / Claude 还需 Refresh Token 换新凭据）；确认在弹窗内进行
  const aliveOk = alive === true;
  const switchBtn = iconAction("switch", "切换");
  if (account.kind === "codex" || account.kind === "claude") {
    const hasRt = !!String(account.refreshToken || "").trim();
    switchBtn.disabled = rowBusy || !aliveOk || !hasRt;
    switchBtn.title = !aliveOk
      ? "请先刷新验证该账户"
      : !hasRt
      ? "该账户没有 Refresh Token，无法切换"
      : account.kind === "claude"
      ? "切换本机 Claude Code 登录（会关闭正在运行的 Claude Desktop）"
      : "切换本机 ChatGPT 登录（会关闭正在运行的 ChatGPT）";
    switchBtn.setAttribute("aria-label", switchBtn.title);
  } else {
    switchBtn.disabled = rowBusy || !aliveOk;
    switchBtn.title = aliveOk ? "切换本机 Cursor 到该账户（会关闭正在运行的 Cursor）" : "请先刷新验证该账户";
    switchBtn.setAttribute("aria-label", switchBtn.title);
  }
  switchBtn.addEventListener("click", () => { void doSwitch(account.id); });
  actions.append(refreshBtn, switchBtn);
  row.append(actions);
  return row;
}

/** 某一类型的账户按主窗口账户页同款套餐顺序排列（不改动 accounts 本身的存储顺序）。 */
function sortedAccounts(kind) {
  return accounts.filter((a) => a.kind === kind).sort(compareAccounts);
}

function render() {
  // 总览 tab 下列表隐藏，无需重建行（切回账户 tab 时会重新渲染）
  if (trayTab !== "overview") {
    const list = el("#tray-list");
    list.replaceChildren();
    const kind = trayTab === "codex" ? "codex" : trayTab === "claude" ? "claude" : "cursor";
    const subset = sortedAccounts(kind);
    if (!subset.length) {
      const empty = document.createElement("div");
      empty.className = "tray-empty";
      empty.textContent = `暂无 ${kindLabel(kind)} 账户，请到主窗口添加。`;
      list.append(empty);
    } else {
      for (const account of subset) list.append(accountRow(account));
    }
  }
  syncHeaderRefresh();
}

/* ---------- 数据 ---------- */

/** 重载账户列表（缓存状态，不刷新账户）；面板被唤起属于主动查看，总览按 5 分钟新鲜度补拉。 */
async function load() {
  try {
    const view = await invoke("accounts_list");
    accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
    const u = Number(view && view.intervalMinutes);
    setUsageCacheTtlMs(u > 0 ? u * 60_000 : DEFAULT_USAGE_TTL_MS);
    render();
    if (trayTab === "overview") void loadOverview({ viewing: true });
  } catch (error) {
    setStatus("bad", `加载失败：${resetError(error)}`);
  }
}

async function refreshOne(id) {
  if (refreshing || switching || isRefreshing(id)) return false;
  if (!accounts.some((a) => a.id === id)) return false;
  refreshingIds.add(id);
  render();
  try {
    const updated = await invoke("account_refresh", { id });
    const index = accounts.findIndex((a) => a.id === id);
    if (index >= 0 && updated && typeof updated === "object") {
      accounts[index] = updated;
    }
    return true;
  } catch (error) {
    setStatus("bad", `刷新失败：${resetError(error)}`);
    return false;
  } finally {
    refreshingIds.delete(id);
    render();
  }
}

async function refreshAll() {
  if (refreshing || switching || refreshingIds.size || !accounts.length) return;
  refreshing = true;
  render();
  // 排队逐个刷新（每次一个）；id 先快照，刷新期间列表可能被广播更新。
  // 顺序与主窗口一致：按类型分组（Cursor → ChatGPT → Claude）、组内按套餐排序，不在类型间来回跳
  const ids = ["cursor", "codex", "claude"].flatMap((kind) => sortedAccounts(kind).map((a) => a.id));
  let failed = 0;
  for (let i = 0; i < ids.length; i += 1) {
    setStatus("", `正在刷新账户 ${i + 1}/${ids.length}…`);
    try {
      await invoke("account_refresh", { id: ids[i] });
    } catch {
      failed += 1;
    }
  }
  refreshing = false;
  if (failed > 0) setStatus("bad", `刷新完成，${failed} 个账户失败。`);
  else setStatus("ok", "已刷新全部账户。");
  await load();
}

/* ---------- 总览：今天 / 昨天 token 用量（与主窗口用量页共用 usage_data 缓存） ---------- */

function overviewTip(text, statAtMs = 0) {
  teardownOverview();
  const tip = document.createElement("div");
  tip.className = "tray-empty";
  tip.textContent = text;
  el("#tray-ov-body").replaceChildren(tip);
  // 「暂无用量数据」同样是一次统计结果，带上数据时间；确实没有任何数据（首次统计中）时为 0，隐藏
  setOverviewUpdated(statAtMs);
  syncOverviewLoading();
}

/**
 * 头部统计日标签与日期导航跟随所选日：标签「今日 / 昨日 / M月D日已用」，导航显示「今天 / 昨天 /
 * MM-DD 周X」（紧凑，避免挤压左侧标签与数据时间），完整日期放悬停提示；到今天时禁用「下一天」。
 */
function applyDayHead() {
  const scope = dayScope();
  el("#tray-ov-label").textContent = `${scope.word}已用`;
  const d = new Date(scope.start);
  const label = el("#tray-day-label");
  label.textContent = !scope.past
    ? "今天"
    : scope.start === dayStartMs(-1)
    ? "昨天"
    : `${scope.ymd.slice(5)} 周${"日一二三四五六"[d.getDay()]}`;
  label.title = scope.ymd;
  el("#tray-day-next").disabled = !scope.past;
}

/** 面板重新打开时统计日回到今天（用户上次翻到的历史日期不保留）。 */
function resetDayToToday() {
  const today = dayStartMs(0);
  if (trayDayMs === today) return;
  trayDayMs = today;
  applyDayHead();
}

/** 按天前后滑动统计日（不越过今天），切换后按主动查看口径加载。 */
function shiftDay(direction) {
  const next = addLocalDays(new Date(trayDayMs), direction).getTime();
  if (next > dayStartMs(0)) return;
  trayDayMs = next;
  applyDayHead();
  void loadOverview({ viewing: true });
}

/* ---------- 总览图表（Chart.js 全局脚本，缺失时静默跳过） ---------- */

// 三联迷你饼图实例（模型 Token / 模型费用 / 来源）
const pies = { modelToken: null, modelCost: null, source: null };
let hourlyStack = null;
let overviewDom = null; // 常驻 DOM，刷新时原地改数字 / 更新图表，避免整页重建闪烁

function destroyCharts() {
  for (const chart of [pies.modelToken, pies.modelCost, pies.source, hourlyStack]) {
    if (chart) {
      try { chart.destroy(); } catch { /* ignore */ }
    }
  }
  pies.modelToken = null;
  pies.modelCost = null;
  pies.source = null;
  hourlyStack = null;
  // 构造中途失败的实例已注册到画布但模块变量拿不到，不清理会让之后的
  // new Chart 永远抛「Canvas is already in use」
  if (overviewDom && typeof Chart !== "undefined" && typeof Chart.getChart === "function") {
    const canvases = [
      overviewDom.tokenPie.canvas,
      overviewDom.costPie.canvas,
      overviewDom.sourcePie.canvas,
      overviewDom.bar.canvas,
    ];
    for (const canvas of canvases) {
      const orphan = Chart.getChart(canvas);
      if (orphan) {
        try { orphan.destroy(); } catch { /* ignore */ }
      }
    }
  }
}

function teardownOverview() {
  destroyCharts();
  overviewDom = null;
}

function cssVar(name) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || undefined;
}

function hasChartLib() {
  return typeof Chart !== "undefined";
}

function chartMotion() {
  const duration = chartAnimMs(520);
  return {
    duration,
    easing: "easeOutQuart",
    animations: {
      colors: { duration: chartAnimMs(150), easing: "easeOutQuad" },
      borderWidth: { duration: chartAnimMs(150) },
    },
  };
}

/** 图表小节：标题 + 定高画布容器 + 图例列表，返回 { wrap, label, canvas, box, legend }。 */
function chartSection(title, height, { legendRow = false } = {}) {
  const wrap = document.createElement("div");
  wrap.className = "tray-chart";
  const label = document.createElement("div");
  label.className = "tray-ov-label";
  label.textContent = title;
  const box = document.createElement("div");
  box.className = "tray-chart-box";
  box.style.height = `${height}px`;
  const canvas = document.createElement("canvas");
  box.append(canvas);
  const legend = document.createElement("ul");
  legend.className = `tray-legend${legendRow ? " tray-legend-row" : ""}`;
  legend.hidden = true;
  wrap.append(label, box, legend);
  return { wrap, label, canvas, box, legend };
}

/** 填充图例（只展示，不做点击切换）：色块 + 名称，超长省略，悬停看全名与数值。 */
function fillLegend(list, items) {
  list.replaceChildren(
    ...items.map(({ label, title, color }) => {
      const li = document.createElement("li");
      const swatch = document.createElement("span");
      swatch.className = "tray-legend-swatch";
      swatch.style.background = color;
      const text = document.createElement("span");
      text.className = "tray-legend-label";
      text.textContent = label;
      li.title = title || label;
      li.append(swatch, text);
      return li;
    })
  );
  list.hidden = !items.length;
}

function shortModel(name) {
  const s = String(name || "");
  if (s.length <= 20) return s;
  return `${s.slice(0, 9)}…${s.slice(-8)}`;
}

/** 各来源当日模型合并为总量表（token 与等价费用），供两个模型饼图各自取 Top。 */
function mergeModelTotals(lists) {
  const map = new Map();
  for (const list of lists || []) {
    for (const m of list || []) {
      const key = m.model || "未知模型";
      const cur = map.get(key) || { model: key, tokens: 0, usd: 0 };
      cur.tokens += Number(m.tokens) || 0;
      cur.usd += Number(m.usd) || 0;
      map.set(key, cur);
    }
  }
  return [...map.values()];
}

/** 按指定指标（tokens / usd）降序取 Top 5 切片，其余合并为「其他」。 */
function topModelShares(all, metric) {
  const other = metric === "usd" ? "tokens" : "usd";
  const list = (all || [])
    .filter((m) => m[metric] > 0)
    .slice()
    .sort((a, b) => b[metric] - a[metric] || b[other] - a[other]);
  if (list.length <= 5) return list;
  const top = list.slice(0, 5);
  const rest = list.slice(5);
  top.push({
    model: "其他",
    tokens: rest.reduce((s, m) => s + m.tokens, 0),
    usd: rest.reduce((s, m) => s + m.usd, 0),
  });
  return top;
}

function hoverOpts(hitBorder) {
  return {
    dim: 0.35,
    hitBorder,
    stroke: cssVar("--chart-tick-strong"),
  };
}

function onChartHover(_event, elements, chart) {
  if (chart.canvas) chart.canvas.style.cursor = elements.length ? "pointer" : "default";
  const hit = elements[0];
  setChartHoverHit(chart, hit ? { datasetIndex: hit.datasetIndex, index: hit.index } : null);
}

/**
 * 三联迷你饼图（环形，无图例——空间只有约 1/3 面板宽）：扇区上标注占比，
 * 名称与数值看 tooltip。cfg 提供各图差异点：
 * { titleOf(row), fmtValue(value), afterOf(row) }。
 */
function makeMiniPie(canvas, cfg) {
  const motion = chartMotion();
  const chart = new Chart(canvas, {
    type: "doughnut",
    data: {
      labels: [],
      datasets: [
        {
          data: [],
          backgroundColor: [],
          hoverBackgroundColor: [],
          borderColor: cssVar("--chart-border"),
          borderWidth: 2,
          hoverOffset: 6,
        },
      ],
    },
    plugins: [pieSliceLabelsPlugin],
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: motion.duration, easing: motion.easing, animateRotate: true, animateScale: true },
      animations: motion.animations,
      cutout: "55%",
      interaction: { mode: "nearest", intersect: true },
      onHover: onChartHover,
      plugins: {
        legend: { display: false },
        tooltip: {
          position: "nearest",
          callbacks: {
            title(items) {
              const row = items.length ? items[0].chart.$shares && items[0].chart.$shares[items[0].dataIndex] : null;
              return row ? cfg.titleOf(row) : "";
            },
            label(ctx) {
              const total = ctx.dataset.data.reduce((s, v) => s + (Number(v) || 0), 0);
              const share = fmtShare(ctx.parsed, total);
              return ` ${cfg.fmtValue(ctx.parsed)}${share ? `（${share}）` : ""}`;
            },
            afterLabel(ctx) {
              const row = ctx.chart.$shares && ctx.chart.$shares[ctx.dataIndex];
              return row ? cfg.afterOf(row) : "";
            },
          },
        },
      },
    },
  });
  // 扇区上只标占比（图窄放不下数值）；配置挂实例属性，不能进 options.plugins
  chart.$pieSliceLabels = {
    font: 9,
    minAngle: 0.5,
    formatter: (_value, share) => share,
  };
  chart.$hoverKey = "";
  chart.$hoverOpts = hoverOpts(0);
  bindChartHoverLeave(chart);
  return chart;
}

/** 同步一个迷你饼图：rows 为空则隐藏该小节。cfg 额外提供 labelOf / valueOf。 */
function syncMiniPie(key, section, rows, cfg) {
  const wrap = section.wrap;
  if (!hasChartLib() || !rows.length) {
    if (pies[key]) {
      pies[key].destroy();
      pies[key] = null;
    }
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  if (!pies[key]) pies[key] = makeMiniPie(section.canvas, cfg);
  const chart = pies[key];
  const colors = rows.map((_, i) => colorFor(i));
  chart.data.labels = rows.map((r) => cfg.labelOf(r));
  chart.data.datasets[0].data = rows.map((r) => cfg.valueOf(r));
  chart.data.datasets[0].backgroundColor = colors;
  chart.data.datasets[0].hoverBackgroundColor = colors;
  chart.$baseColors = [colors];
  chart.$shares = rows;
  chart.$hoverKey = "";
  chart.update();
  fillLegend(
    section.legend,
    rows.map((r, i) => ({
      label: cfg.labelOf(r),
      title: `${cfg.titleOf(r)}：${cfg.fmtValue(cfg.valueOf(r))}`,
      color: colors[i],
    }))
  );
}

// 三联饼图的差异配置：模型 Token / 模型费用 / 来源
const TOKEN_PIE_CFG = {
  labelOf: (r) => shortModel(r.model),
  valueOf: (r) => Math.round(r.tokens),
  titleOf: (r) => r.model,
  fmtValue: (v) => fmtTokens(v),
  afterOf: (r) => (r.usd > 0 ? `等价 ${fmtUsd(r.usd)}` : ""),
};
const COST_PIE_CFG = {
  labelOf: (r) => shortModel(r.model),
  valueOf: (r) => r.usd,
  titleOf: (r) => r.model,
  fmtValue: (v) => fmtUsd(v),
  afterOf: (r) => (r.tokens > 0 ? `${fmtTokens(r.tokens)} tok` : ""),
};
const SOURCE_PIE_CFG = {
  labelOf: (r) => r.name,
  valueOf: (r) => Math.round(r.tokens),
  titleOf: (r) => r.name,
  fmtValue: (v) => fmtTokens(v),
  afterOf: (r) => (Number.isFinite(r.usd) ? `等价 ${fmtUsd(r.usd)}` : ""),
};

/** 所选日 24 小时 Token 堆叠柱：每账户一段，0:00 → 23:00 从左到右，与统计页单日视图一致。 */
function makeHourlyStack(canvas, labels, datasets) {
  const motion = chartMotion();
  const chart = new Chart(canvas, {
    type: "bar",
    data: { labels, datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: motion.duration, easing: motion.easing },
      animations: motion.animations,
      interaction: { mode: "nearest", intersect: true, axis: "xy" },
      onHover: onChartHover,
      plugins: {
        legend: { display: false },
        tooltip: {
          position: "nearest",
          filter: (item) => item.raw != null && item.raw > 0,
          callbacks: {
            title(items) {
              if (!items.length) return "";
              const chart = items[0].chart;
              const h = parseInt(chart.data.labels[items[0].dataIndex], 10);
              return Number.isFinite(h) ? `${chart.$dayWord || "今天"} ${h}:00 – ${h + 1}:00` : "";
            },
            label(ctx) {
              if (ctx.raw == null) return null;
              return ` ${ctx.dataset.label}: ${fmtTokens(ctx.raw)}`;
            },
            footer(items) {
              if (!items.length) return "";
              const idx = items[0].dataIndex;
              let sum = 0;
              for (const ds of items[0].chart.data.datasets) {
                const v = ds.data[idx];
                if (typeof v === "number") sum += v;
              }
              return `该小时合计 ${fmtTokens(sum)}`;
            },
          },
        },
      },
      scales: {
        x: {
          stacked: true,
          ticks: {
            color: cssVar("--chart-tick"),
            font: { size: 9 },
            maxRotation: 0,
            autoSkip: true,
            maxTicksLimit: 7,
          },
          grid: { display: false },
        },
        y: {
          stacked: true,
          beginAtZero: true,
          ticks: {
            color: cssVar("--chart-tick-strong"),
            font: { size: 9.5 },
            maxTicksLimit: 4,
            callback: (v) => compactTokens(v),
          },
          grid: { color: cssVar("--chart-grid") },
        },
      },
    },
  });
  chart.$hoverKey = "";
  chart.$hoverOpts = hoverOpts(2);
  bindChartHoverLeave(chart);
  return chart;
}

/** 所选日 24 小时堆叠柱：数据取各来源聚合结果的 hourly（按 date 取所选日），0:00 → 23:00 从左到右。 */
function syncHourlyStack(sources, scope) {
  const wrap = overviewDom.bar.wrap;
  const ymd = scope.ymd;
  overviewDom.bar.label.textContent = `${scope.word} Token（按小时）`;
  const hours = [];
  for (let h = 0; h <= 23; h += 1) hours.push(h);
  const labels = hours.map((h) => `${h}:00`);
  const list = (sources || []).filter((s) => s && s.label);
  const baseColors = list.map((_, i) => colorFor(i));
  let hasAny = false;
  const datasets = list.map((s, i) => {
    const byHour = new Map(
      (s.hourly || []).filter((r) => r && r.date === ymd).map((r) => [Number(r.hour), r])
    );
    return {
      label: s.label,
      data: hours.map((h) => {
        const row = byHour.get(h);
        if (row && row.tokens > 0) hasAny = true;
        return row && row.tokens > 0 ? row.tokens : null;
      }),
      backgroundColor: baseColors[i],
      hoverBackgroundColor: baseColors[i],
      borderRadius: 2,
      maxBarThickness: 10,
      stack: "hourly",
      skipNull: true,
    };
  });
  if (!hasChartLib() || !list.length || !hasAny) {
    if (hourlyStack) { hourlyStack.destroy(); hourlyStack = null; }
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  fillLegend(
    overviewDom.bar.legend,
    list.map((s, i) => ({ label: s.label, title: s.label, color: baseColors[i] }))
  );
  if (hourlyStack) {
    hourlyStack.data.labels = labels;
    hourlyStack.data.datasets = datasets;
    hourlyStack.$baseColors = baseColors;
    hourlyStack.$hoverKey = "";
    hourlyStack.$dayWord = scope.tipWord;
    hourlyStack.update();
    return;
  }
  hourlyStack = makeHourlyStack(overviewDom.bar.canvas, labels, datasets);
  hourlyStack.$baseColors = baseColors;
  hourlyStack.$dayWord = scope.tipWord;
}

function ensureOverviewDom() {
  if (overviewDom) return;
  const root = el("#tray-ov-body");
  const total = document.createElement("div");
  total.className = "tray-ov-total";
  const tokens = document.createElement("span");
  tokens.className = "tray-ov-tokens";
  const usd = document.createElement("span");
  usd.className = "tray-ov-usd";
  total.append(tokens, usd);
  // 图表顺序与统计页一致：24 小时柱在上（图例横排在图下），下方三个饼图三等分并排（各带竖排图例）；
  // 柱图标题随所选日更新
  const bar = chartSection("今日 Token（按小时）", 128, { legendRow: true });
  bar.wrap.hidden = true;
  const tokenPie = chartSection("模型 Token", 104);
  tokenPie.wrap.hidden = true;
  const costPie = chartSection("模型费用", 104);
  costPie.wrap.hidden = true;
  const sourcePie = chartSection("来源", 104);
  sourcePie.wrap.hidden = true;
  const pieRow = document.createElement("div");
  pieRow.className = "tray-pie-row";
  pieRow.append(tokenPie.wrap, costPie.wrap, sourcePie.wrap);
  const rows = document.createElement("div");
  rows.className = "tray-ov-rows";
  root.replaceChildren(total, bar.wrap, pieRow, rows);
  overviewDom = { tokens, usd, rows, tokenPie, costPie, sourcePie, pieRow, bar };
}

function renderOverview(data, statAtMs, scope) {
  const hasChart = (data.hourlySources || []).some((s) => (s.hourly || []).some((r) => r && r.tokens > 0))
    || (data.modelTokenShares || []).some((m) => m && m.tokens > 0)
    || (data.modelCostShares || []).some((m) => m && m.usd > 0);
  if (!data.rows.length && !hasChart) {
    overviewTip("暂无用量数据", statAtMs);
    return;
  }
  ensureOverviewDom();
  overviewDom.tokens.textContent = fmtTokens(data.totalTokens);
  overviewDom.tokens.title = fmtInt(data.totalTokens);
  overviewDom.usd.textContent = fmtUsd(data.totalUsd);

  const shares = data.rows.filter((r) => r.tokens > 0 && !r.empty);
  // 图表异常不得中断总览渲染（数字与来源明细仍要照常更新），
  // 失败时销毁实例（含孤儿注册），下轮渲染自动重建。
  try {
    syncHourlyStack(data.hourlySources || [], scope);
    syncMiniPie("modelToken", overviewDom.tokenPie, data.modelTokenShares || [], TOKEN_PIE_CFG);
    syncMiniPie("modelCost", overviewDom.costPie, data.modelCostShares || [], COST_PIE_CFG);
    syncMiniPie("source", overviewDom.sourcePie, shares, SOURCE_PIE_CFG);
  } catch (error) {
    console.error("托盘图表渲染失败，已重置实例：", error);
    destroyCharts();
  }
  // 三个饼图全空时整行收起，避免残留空隙
  overviewDom.pieRow.hidden =
    overviewDom.tokenPie.wrap.hidden && overviewDom.costPie.wrap.hidden && overviewDom.sourcePie.wrap.hidden;

  overviewDom.rows.replaceChildren();
  for (const r of data.rows) {
    const row = document.createElement("div");
    row.className = "tray-ov-row";
    const main = document.createElement("div");
    main.className = "tray-ov-main";
    const name = document.createElement("span");
    name.className = "tray-ov-name";
    name.textContent = r.name;
    name.title = r.name;
    main.append(name);
    if (r.error != null) {
      row.classList.add("bad");
      row.title = r.error;
      const err = document.createElement("div");
      err.className = "tray-ov-err";
      err.textContent = r.error;
      main.append(err);
    }
    const val = document.createElement("span");
    val.className = "tray-ov-val";
    if (r.empty) {
      val.textContent = "—";
    } else {
      val.textContent = `${fmtTokens(r.tokens)} · ${fmtUsd(r.usd)}`;
      val.title = fmtInt(r.tokens);
    }
    row.append(main, val);
    overviewDom.rows.append(row);
  }

  // 数据时间放在总览头部（「今日已用 · 更新于 HH:MM」），进行中提示由 syncOverviewLoading 负责
  setOverviewUpdated(statAtMs);
  syncOverviewLoading();
}

/** 优先取单日键聚合里的 hourly，缺失时退回长范围缓存（后端聚合同样带今天 / 昨天的 hourly）。 */
function pickHourly(dayAgg, seriesAgg) {
  if (dayAgg && Array.isArray(dayAgg.hourly) && dayAgg.hourly.length) return dayAgg.hourly;
  return (seriesAgg && seriesAgg.hourly) || [];
}

/**
 * 所选日各来源的缓存命中方式：今天可用今日键或任意长范围序列的当天切片（都是进行中的数据）；
 * 昨天只认完整数据（day: 键，或该日结束后才拉取的序列），不用半天的旧缓存凑数。
 */
function dayPeekers(scope) {
  if (scope.past) {
    return {
      agg: (id) => peekAggForPastDay(id, scope.ymd),
      aggSeries: () => null,
      scan: (home) => peekScanForPastDay(scope.ymd, home),
      scanSeries: () => null,
      claude: (home) => peekClaudeScanForPastDay(scope.ymd, home),
      claudeSeries: () => null,
    };
  }
  return {
    agg: (id) => peekAggForDay(id, scope.ymd),
    aggSeries: (id) => peekAggSeries(id, scope.ymd),
    scan: (home) => peekScanForDay(scope.ymd, home),
    scanSeries: (home) => peekScanSeries(scope.ymd, home),
    claude: (home) => peekClaudeScanForDay(scope.ymd, home),
    claudeSeries: (home) => peekClaudeScanSeries(scope.ymd, home),
  };
}

/** 从共享缓存拼出托盘总览：所选日数字用当天切片，24 小时柱用聚合结果的 hourly。
 *  来源（各账户 + 本地分析）按当日 Token 降序排列，行序与各图表配色一一对应；
 *  所选日没有用量的来源不列出。fetchErrors 为本轮拉取失败的来源 id → 错误，用于数据时间的取舍。 */
function buildOverviewData(scope, fetchErrors = new Map()) {
  const ymd = scope.ymd;
  const peek = dayPeekers(scope);
  const cursorAccounts = accounts.filter((a) => a.kind === "cursor");
  const entries = []; // { row, hourlySource, modelSource }
  // 数据时间取所有拿到数据的来源（与当天有没有用量无关），但不计本轮拉取失败与已失效的账户：
  // 它们的缓存时间永远不再前进，计入会把「更新于」拖回很久以前，与实际刷新不符
  const ats = [];
  const noteAt = (id, at) => {
    if (!fetchErrors.has(id) && Number.isFinite(at)) ats.push(at);
  };
  let totalTokens = 0;
  let totalUsd = 0;

  for (const a of cursorAccounts) {
    const label = trayAccountLabel(a);
    const dayHit = peek.agg(a.id);
    const seriesHit = peek.aggSeries(a.id);
    if (!dayHit && !seriesHit) continue;
    const hit = dayHit || seriesHit;
    if (!(a.status && a.status.alive === false)) noteAt(a.id, hit.entry.at);
    const sliceAgg = hit.entry.agg;
    const sliceKey = hit.rangeKey;
    const slice = sliceDay(sliceAgg, ymd, sliceKey);
    // 所选日没有用量的账户不占行（拉取失败的由 applyOverviewErrors 补一行错误提示）
    if (!(slice.tokens > 0)) continue;
    totalTokens += slice.tokens;
    totalUsd += slice.usd;
    entries.push({
      row: { id: a.id, name: label, tokens: slice.tokens, usd: slice.usd },
      hourlySource: {
        label,
        hourly: pickHourly(dayHit && dayHit.entry.agg, seriesHit && seriesHit.entry.agg),
      },
      modelSource: { id: a.id, label, models: modelsOnDay(sliceAgg, ymd, sliceKey) },
    });
  }

  // 本地扫描来源（Codex / Claude）共用的切片逻辑；与账户同一规则，所选日没有用量不占行
  const pushScanEntry = (id, name, scanDay, scanSeries) => {
    if (!scanDay && !scanSeries) return;
    const hit = scanDay || scanSeries;
    noteAt(id, hit.entry.at);
    const agg = hit.entry.scan.aggregate;
    const slice = sliceDay(agg, ymd, hit.rangeKey);
    if (!(slice.tokens > 0)) return;
    totalTokens += slice.tokens;
    totalUsd += slice.usd;
    entries.push({
      row: { id, name, tokens: slice.tokens, usd: slice.usd },
      hourlySource: {
        label: name,
        hourly: pickHourly(
          scanDay && scanDay.entry.scan.aggregate,
          scanSeries && scanSeries.entry.scan.aggregate
        ),
      },
      modelSource: { id, label: name, models: modelsOnDay(agg, ymd, hit.rangeKey) },
    });
  };
  pushScanEntry("local", "本地 ChatGPT", peek.scan(""), peek.scanSeries(""));
  pushScanEntry("local-claude", "本地 Claude", peek.claude(""), peek.claudeSeries(""));

  entries.sort((a, b) => b.row.tokens - a.row.tokens);
  const modelTotals = mergeModelTotals(entries.map((e) => e.modelSource.models));
  return {
    data: {
      totalTokens,
      totalUsd,
      rows: entries.map((e) => e.row),
      hourlySources: entries.map((e) => e.hourlySource),
      modelTokenShares: topModelShares(modelTotals, "tokens"),
      modelCostShares: topModelShares(modelTotals, "usd"),
    },
    at: ats.length ? Math.min(...ats) : 0,
  };
}

function applyOverviewErrors(data, fetchErrors, cursorAccounts) {
  if (!fetchErrors.size) return data;
  for (const a of cursorAccounts) {
    if (!fetchErrors.has(a.id)) continue;
    const err = fetchErrors.get(a.id);
    const row = data.rows.find((r) => r.id === a.id);
    if (row) {
      row.error = err;
    } else {
      data.rows.push({ id: a.id, name: trayAccountLabel(a), tokens: 0, usd: 0, error: err, empty: true });
    }
  }
  for (const [id, name] of [["local", "本地 ChatGPT"], ["local-claude", "本地 Claude"]]) {
    if (!fetchErrors.has(id)) continue;
    const err = fetchErrors.get(id);
    const row = data.rows.find((r) => r.id === id);
    if (row) row.error = err;
    else data.rows.push({ id, name, tokens: 0, usd: 0, error: err, empty: true });
  }
  return data;
}

// 用户主动查看总览（打开面板 / 切到总览 tab / 切换统计日）时数据最多允许多旧：超过就强制同步，
// 不受定时刷新间隔决定的缓存有效期（可能长达 24 小时）影响。
// 后台事件（账户变化、其它窗口写入缓存）触发的重载仍按缓存有效期判断，避免连带联网。
const VIEW_MAX_AGE_MS = 5 * 60_000;

/**
 * 某来源是否需要拉取：没有缓存、强制刷新，或缓存超过允许的最大年龄
 * （主动查看时为 VIEW_MAX_AGE_MS，否则为缓存有效期）。
 */
function needsDayFetch(peeked, { force, maxAgeMs }) {
  if (force || !peeked) return true;
  if (maxAgeMs != null) return Date.now() - peeked.entry.at > maxAgeMs;
  return !isUsageCacheFresh(peeked.entry.at);
}

let overviewTail = Promise.resolve();

/**
 * 排队加载总览。opts.force 为刷新按钮的强制刷新（全部来源立即重拉）；
 * opts.viewing 为用户主动查看：超过 VIEW_MAX_AGE_MS 的来源强制同步，其余用缓存。
 */
function loadOverview(opts = {}) {
  const run = () => loadOverviewInner(opts);
  overviewTail = overviewTail.then(run, run);
  return overviewTail;
}

/**
 * 统计所选日用量：先用共享缓存立即渲染，缺的 / 过期的再拉取并写回同一套 localStorage
 * （今天写 today: 键、昨天写 day: 键，与主窗口用量页互通）。加载期间用户切了统计日，
 * 本轮结果不再渲染，交给随后排队的新一轮。
 */
async function loadOverviewInner({ force = false, viewing = false } = {}) {
  const scope = dayScope();
  const stillCurrent = () => trayDayMs === scope.start;
  const cursorAccounts = accounts.filter((a) => a.kind === "cursor");
  const peek = dayPeekers(scope);
  const rule = { force, maxAgeMs: viewing ? VIEW_MAX_AGE_MS : null };
  // 主动查看时过期的来源要真正同步（sync 模式），不能让后端按更长的有效期判定为新鲜而只切片
  const fetchForce = force || viewing;

  const cached = buildOverviewData(scope);

  // 今天拉到当前时刻；昨天拉完整自然日 [0 点, 次日 0 点)，终点退 1ms 与主窗口口径一致
  const aggRange = scope.past
    ? { start: scope.start, end: scope.end - 1 }
    : { start: scope.start, end: Date.now() };
  const scanArgs = scope.past
    ? { sinceMs: scope.start, untilMs: scope.end, force: fetchForce }
    : { sinceMs: scope.start, force: fetchForce };

  const jobs = [];
  const fetchErrors = new Map();
  for (const a of cursorAccounts) {
    const peeked = peek.agg(a.id);
    // 已失效账户无法再同步事件库，syncedAt 停在最后一次成功时间，按有效期会永远过期。
    // 有缓存就直接用，避免拉取后写缓存广播再把总览打进下一轮加载。
    if (peeked && a.status && a.status.alive === false) continue;
    if (!needsDayFetch(peeked, rule)) continue;
    jobs.push(
      fetchCursorAggregate(a, scope.aggKey, { ...aggRange, force: fetchForce }).catch((error) => {
        fetchErrors.set(a.id, resetError(error));
      })
    );
  }
  if (needsDayFetch(peek.scan(""), rule)) {
    jobs.push(
      fetchCodexScan(scanArgs).catch((error) => {
        fetchErrors.set("local", resetError(error));
      })
    );
  }
  if (needsDayFetch(peek.claude(""), rule)) {
    jobs.push(
      fetchClaudeScan(scanArgs).catch((error) => {
        fetchErrors.set("local-claude", resetError(error));
      })
    );
  }

  if (cached.data.rows.length) renderOverview(cached.data, cached.at, scope);
  else if (!jobs.length) overviewTip("暂无用量数据", cached.at);

  if (!jobs.length) return;
  overviewLoading = true;
  syncHeaderRefresh();
  if (!cached.data.rows.length && !overviewDom) overviewTip(`正在统计${scope.word}用量…`, cached.at);
  try {
    await Promise.allSettled(jobs);
    if (!stillCurrent()) return;
    const next = buildOverviewData(scope, fetchErrors);
    applyOverviewErrors(next.data, fetchErrors, cursorAccounts);
    // 没有任何来源成功时不冒充「刚更新」，数据时间留空
    renderOverview(next.data, next.at, scope);
  } finally {
    overviewLoading = false;
    syncHeaderRefresh();
  }
}

/* ---------- 切换账户（弹窗内确认 + 分步进度 + 结果） ---------- */

const SWITCH_ERRORS = {
  account_not_verified: "账户未验证，请先在主窗口刷新。",
  cursor_running: "Cursor 仍在运行，请关闭后重试。",
  cursor_exe_not_found: "未找到 Cursor，请在主窗口设置路径。",
  cursor_exe_invalid: "Cursor 路径无效，请在主窗口设置。",
  state_db_not_found: "未找到 Cursor 认证库，请先启动过一次 Cursor。",
  invalid_session_token: "Token 已失效，请在主窗口更新。",
  poll_timeout: "未能换取登录凭证，请重试。",
  session_exchange_failed: "换取登录凭证失败，请检查网络。",
  not_codex_account: "该账户不是 ChatGPT 账户。",
  codex_running: "ChatGPT 仍在运行，请关闭后重试。",
  codex_exe_not_found: "未找到 ChatGPT，请在主窗口设置路径。",
  codex_exe_invalid: "ChatGPT 路径无效，请在主窗口设置。",
  codex_no_refresh_token: "该账户没有 Refresh Token，无法切换本机登录。",
  // codex_refresh_denied 带归类后缀，见 switchErrorText
  codex_id_token_missing: "未能获取登录所需的 id_token，请稍后重试。",
  not_claude_account: "该账户不是 Claude 账户。",
  claude_running: "Claude Desktop 仍在运行，请关闭后重试。",
  claude_no_refresh_token: "该账户没有 Refresh Token，无法切换本机登录。",
  claude_refresh_denied: "Refresh Token 已失效，请编辑账户更新凭据后重试。",
};

/** 切换成功的结果弹窗停留秒数，到点自动关闭（按钮上倒计时，期间可手动关闭）。 */
const SWITCH_RESULT_AUTO_CLOSE_SECONDS = 3;

// 切换弹窗：busy（检测中）→ confirm（可取消）→ steps（进行中，禁止关闭）→ result（仅「关闭」）
// 成功结果倒计时自动关闭，失败结果保留到用户手动关闭。
const trayModal = (() => {
  const root = el("#tray-modal");
  const title = el("#tray-modal-title");
  const body = el("#tray-modal-body");
  const stepsBox = el("#tray-modal-steps");
  const result = el("#tray-modal-result");
  const foot = el(".tray-modal-foot", root);
  const btnCancel = el("#tray-modal-cancel");
  const btnOk = el("#tray-modal-ok");
  let phase = "idle"; // idle | busy | confirm | steps | result
  let confirmResolve = null;
  let icons = [];
  let current = -1;
  let autoCloseTimer = null;

  function settleConfirm(value) {
    const resolve = confirmResolve;
    confirmResolve = null;
    if (resolve) resolve(value);
  }

  function stopAutoClose() {
    if (autoCloseTimer) {
      clearTimeout(autoCloseTimer);
      autoCloseTimer = null;
    }
  }

  // 结果阶段倒计时：按钮显示「关闭（3s）」逐秒递减，归零自动 close
  function startAutoClose() {
    stopAutoClose();
    let remain = SWITCH_RESULT_AUTO_CLOSE_SECONDS;
    const tick = () => {
      if (remain <= 0) {
        close();
        return;
      }
      btnOk.textContent = `关闭（${remain}s）`;
      remain -= 1;
      autoCloseTimer = setTimeout(tick, 1000);
    };
    tick();
  }

  function openBusy(titleText, text) {
    stopAutoClose();
    phase = "busy";
    root.hidden = false;
    title.textContent = titleText;
    body.hidden = false;
    body.textContent = text;
    stepsBox.hidden = true;
    result.hidden = true;
    foot.hidden = true;
  }

  /** 切到确认态；title 省略时沿用 openBusy 设置的标题。Esc / 取消按钮 resolve(false)。 */
  function toConfirm({ title: titleText, body: bodyText, confirmText, danger }) {
    stopAutoClose();
    phase = "confirm";
    root.hidden = false;
    if (titleText) title.textContent = titleText;
    body.hidden = false;
    body.textContent = bodyText;
    stepsBox.hidden = true;
    result.hidden = true;
    foot.hidden = false;
    btnCancel.hidden = false;
    btnOk.textContent = confirmText || "切换";
    btnOk.classList.toggle("danger", !!danger);
    return new Promise((resolve) => { confirmResolve = resolve; });
  }

  function toSteps(labels) {
    phase = "steps";
    body.hidden = true;
    foot.hidden = true;
    icons = [];
    current = -1;
    stepsBox.replaceChildren();
    for (const label of labels) {
      const row = document.createElement("div");
      row.className = "tray-step";
      const icon = document.createElement("span");
      icon.className = "tray-step-icon";
      const text = document.createElement("span");
      text.textContent = label;
      row.append(icon, text);
      stepsBox.append(row);
      icons.push(icon);
    }
    stepsBox.hidden = false;
  }

  function setIcon(state) {
    const icon = icons[current];
    if (icon) icon.className = `tray-step-icon ${state}`;
  }

  function stepStart() {
    current++;
    setIcon("running");
  }

  /** detail 为可选补充说明，追加在当前步骤文案之后（如「写入登录凭证 · 已换取新凭据」）。 */
  function stepDone(detail) {
    setIcon("done");
    const icon = icons[current];
    const text = icon && icon.nextElementSibling;
    if (detail && text) text.textContent = `${text.textContent} · ${detail}`;
  }

  // 仅当有进行中的步骤时标红；检测 / 确认阶段出错则无步骤可标，静默跳过
  function stepFail() {
    const icon = icons[current];
    if (icon && icon.classList.contains("running")) icon.className = "tray-step-icon fail";
  }

  function finish(ok, message) {
    phase = "result";
    body.hidden = true;
    result.hidden = false;
    result.className = ok ? "ok" : "bad";
    result.textContent = message;
    foot.hidden = false;
    btnCancel.hidden = true;
    btnOk.textContent = "关闭";
    btnOk.classList.remove("danger");
    if (ok) startAutoClose();
  }

  function close() {
    stopAutoClose();
    phase = "idle";
    settleConfirm(false);
    root.hidden = true;
    stepsBox.replaceChildren();
    stepsBox.hidden = true;
    result.hidden = true;
    result.className = "";
    btnOk.classList.remove("danger");
    icons = [];
    current = -1;
  }

  btnOk.addEventListener("click", () => {
    if (phase === "confirm") settleConfirm(true);
    else if (phase === "result") close();
  });
  btnCancel.addEventListener("click", () => {
    if (phase === "confirm") settleConfirm(false);
  });
  // busy / steps 阶段忽略 Esc（无任何关闭途径），防止流程中途被关闭
  window.addEventListener("keydown", (event) => {
    if (event.key !== "Escape" || root.hidden) return;
    if (phase === "confirm") settleConfirm(false);
    else if (phase === "result") close();
  });

  return { openBusy, toConfirm, toSteps, stepStart, stepDone, stepFail, finish, close };
})();

async function doSwitch(id) {
  if (switching) return;
  switching = true;
  render();
  const account = accounts.find((a) => a.id === id);
  try {
    if (account && account.kind === "codex") {
      trayModal.openBusy("切换本机 ChatGPT 登录", "正在检测本地 ChatGPT…");
      const st = await invoke("codex_client_status");
      if (!st.exeConfigured) {
        trayModal.finish(false, "未找到 ChatGPT，请到主窗口设置。");
        return;
      }
      const agreed = await trayModal.toConfirm(st.running
        ? { body: "ChatGPT 正在运行，切换将先关闭它，未保存内容可能丢失。确定继续？", confirmText: "关闭并切换", danger: true }
        : { body: "将把该账户写入本机 ChatGPT 登录并启动 ChatGPT。确定继续？", confirmText: "切换" });
      if (!agreed) {
        trayModal.close();
        return;
      }
      trayModal.toSteps(st.running
        ? ["关闭 ChatGPT", "写入登录凭证", "启动 ChatGPT"]
        : ["写入登录凭证", "启动 ChatGPT"]);
      if (st.running) {
        trayModal.stepStart();
        const c = await invoke("codex_client_close");
        if (!c.closed) {
          trayModal.stepFail();
          trayModal.finish(false, "未能完全关闭 ChatGPT，请手动关闭后重试。");
          return;
        }
        trayModal.stepDone();
      }
      trayModal.stepStart();
      const sw = await invoke("codex_switch_local", { id });
      // 体现本次是换了新凭据还是直接写入保存的副本
      const how = typeof (sw && sw.exchanged) === "boolean"
        ? (sw.exchanged ? "已换取新凭据" : "直接写入保存的副本，未换票")
        : "";
      trayModal.stepDone(how);
      await load();
      trayModal.stepStart();
      const l = await invoke("codex_client_launch");
      trayModal.stepDone();
      const note = how ? `（${how}）` : "";
      trayModal.finish(true, l.launched ? `已切换账户并启动 ChatGPT${note}。` : `已切换${note}，请手动启动 ChatGPT。`);
      return;
    }
    if (account && account.kind === "claude") {
      // 写入的是 Claude Code 凭据；Claude Desktop 仅联动关闭 / 启动，未安装也可切
      trayModal.openBusy("切换本机 Claude Code 登录", "正在检测本地 Claude Desktop…");
      const st = await invoke("claude_client_status");
      const hasDesktop = !!st.exeConfigured;
      const agreed = await trayModal.toConfirm(st.running
        ? { body: "Claude Desktop 正在运行，切换将先关闭它，未保存内容可能丢失。确定继续？", confirmText: "关闭并切换", danger: true }
        : hasDesktop
        ? { body: "将把该账户写入本机 Claude Code 登录并启动 Claude Desktop。确定继续？", confirmText: "切换" }
        : { body: "将把该账户写入本机 Claude Code 登录（未检测到 Claude Desktop，跳过启动）。确定继续？", confirmText: "切换" });
      if (!agreed) {
        trayModal.close();
        return;
      }
      const steps = [];
      if (st.running) steps.push("关闭 Claude Desktop");
      steps.push("换取登录凭证并写入");
      if (hasDesktop) steps.push("启动 Claude Desktop");
      trayModal.toSteps(steps);
      if (st.running) {
        trayModal.stepStart();
        const c = await invoke("claude_client_close");
        if (!c.closed) {
          trayModal.stepFail();
          trayModal.finish(false, "未能完全关闭 Claude Desktop，请手动关闭后重试。");
          return;
        }
        trayModal.stepDone();
      }
      trayModal.stepStart();
      await invoke("claude_switch_local", { id });
      trayModal.stepDone();
      await load();
      if (hasDesktop) {
        trayModal.stepStart();
        const l = await invoke("claude_client_launch");
        trayModal.stepDone();
        trayModal.finish(true, l.launched ? "已切换账户并启动 Claude Desktop。" : "已切换，请手动启动 Claude Desktop。");
      } else {
        trayModal.finish(true, "已切换本机 Claude Code 登录。");
      }
      return;
    }
    trayModal.openBusy("切换本机 Cursor 登录", "正在检测本地 Cursor…");
    const st = await invoke("cursor_client_status");
    if (!st.exeConfigured) {
      trayModal.finish(false, "未找到 Cursor 可执行文件，请到主窗口设置。");
      return;
    }
    const agreed = await trayModal.toConfirm(st.running
      ? { body: "Cursor 正在运行，切换将先关闭它，未保存内容可能丢失。确定继续？", confirmText: "关闭并切换", danger: true }
      : { body: "将把该账户写入本机 Cursor 登录并启动 Cursor。确定继续？", confirmText: "切换" });
    if (!agreed) {
      trayModal.close();
      return;
    }
    trayModal.toSteps(st.running
      ? ["关闭 Cursor", "换取登录凭证并写入", "启动 Cursor"]
      : ["换取登录凭证并写入", "启动 Cursor"]);
    if (st.running) {
      trayModal.stepStart();
      const c = await invoke("cursor_client_close");
      if (!c.closed) {
        trayModal.stepFail();
        trayModal.finish(false, "未能完全关闭 Cursor，请手动关闭后重试。");
        return;
      }
      trayModal.stepDone();
    }
    trayModal.stepStart();
    await invoke("cursor_switch_local", { id });
    trayModal.stepDone();
    trayModal.stepStart();
    const l = await invoke("cursor_client_launch");
    trayModal.stepDone();
    trayModal.finish(true, l.launched ? "已切换账户并启动 Cursor。" : "已切换，请手动启动 Cursor。");
  } catch (error) {
    const code = resetError(error);
    // 写登录文件失败的错误码带冒号细节（codex_auth_write_failed:…），按前缀匹配
    const deniedWhy = {
      expired: "Refresh Token 已过期",
      reused: "Refresh Token 已被使用过（已被别处轮换）",
      revoked: "Refresh Token 已被吊销",
      invalid: "Refresh Token 已失效",
    };
    const text = code.startsWith("codex_auth_write_failed")
      ? "写入本机 ChatGPT 登录文件失败，请检查文件权限后重试。"
      : code.startsWith("claude_creds_write_failed")
      ? "写入本机 Claude Code 登录凭据失败，请检查文件权限后重试。"
      : code.startsWith("codex_refresh_denied")
      ? `${deniedWhy[code.split(":")[1]] || deniedWhy.invalid}，请在主窗口编辑账户更新凭据，或用「强制写入并启动」。`
      : SWITCH_ERRORS[code] || code;
    trayModal.stepFail();
    trayModal.finish(false, `切换失败：${text}`);
  } finally {
    switching = false;
    render();
  }
}

/* ---------- 初始化 ---------- */

setupDesktopGuards();
// 统一刷新：总览 tab = 刷新全部账户状态 + 忽略缓存重算今日用量；账户 tab = 刷新全部账户状态
// （账户状态刷新会广播 accounts-changed，主窗口据此按其当前跨度预取用量，各处数据一并更新）
el("#tray-refresh").addEventListener("click", () => {
  if (trayTab === "overview") {
    if (accounts.length) void refreshAll();
    void loadOverview({ force: true });
    return;
  }
  void refreshAll();
});
el("#tray-open").addEventListener("click", () => { void invoke("tray_open_main"); });
for (const btn of document.querySelectorAll(".tray-tab")) {
  btn.addEventListener("click", () => setTab(btn.dataset.tab));
}
// 总览统计日：左右箭头按天前后滑动
el("#tray-day-prev").addEventListener("click", () => shiftDay(-1));
el("#tray-day-next").addEventListener("click", () => shiftDay(1));
applyTab();
applyDayHead();
// 面板每次被托盘点击唤起（获得焦点）时重载缓存数据，总览按主动查看口径补拉（超过 5 分钟强制同步）；
// 只清状态条、重绘当前 tab，不触碰切换弹窗（进行中的弹窗须保持原状）
window.addEventListener("focus", () => {
  clearStatus();
  resetDayToToday();
  void load();
});
// 订阅后端广播：主窗口刷新 / 增删改账户时，开着的面板实时同步（获焦重载仍保留作兜底）
listen("accounts-changed", (event) => {
  const view = event.payload;
  accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
  const ids = new Set(accounts.map((a) => a.id));
  for (const id of [...remoteRefreshingIds]) {
    if (!ids.has(id)) remoteRefreshingIds.delete(id);
  }
  const u = Number(view && view.intervalMinutes);
  if (Number.isFinite(u)) setUsageCacheTtlMs(u > 0 ? u * 60_000 : DEFAULT_USAGE_TTL_MS);
  render();
  // 后台事件触发的重载：按缓存有效期判断，不按主动查看的 5 分钟口径
  if (trayTab === "overview") void loadOverview();
}).catch(() => {
  /* 非 Tauri 环境（浏览器直开调试）无事件桥，忽略 */
});
// 任何窗口发起的账户刷新开始 / 结束：同步行内「刷新中」动画、头部刷新按钮与总览进行中提示
listen("account-refreshing", (event) => {
  const payload = event.payload || {};
  const id = String(payload.id || "");
  if (!id) return;
  if (payload.active) remoteRefreshingIds.add(id);
  else remoteRefreshingIds.delete(id);
  render();
}).catch(() => {
  /* 非 Tauri 环境无事件桥 */
});
let usageCacheTimer = null;
function scheduleOverviewFromCache(key) {
  forgetUsageCacheFromEvent(key);
  if (trayTab !== "overview") return;
  clearTimeout(usageCacheTimer);
  usageCacheTimer = setTimeout(() => { void loadOverview(); }, 250);
}
window.addEventListener("storage", (event) => {
  if (!event.key || !event.key.startsWith(USAGE_CACHE_PREFIX)) return;
  scheduleOverviewFromCache(event.key);
});
listen(USAGE_CACHE_EVENT, (event) => {
  const payload = event.payload || {};
  // 事件会回送到写入窗口；不过滤自己的写入时，失效账户（缓存时间戳不再前进）
  // 会把「写缓存 → 当作外部更新再加载」打成总览循环刷新。
  if (payload.origin && payload.origin === USAGE_CACHE_ORIGIN) return;
  scheduleOverviewFromCache(payload.key);
}).catch(() => {
  /* 非 Tauri 环境无事件桥，仍靠 storage / 获焦 */
});
void load();
