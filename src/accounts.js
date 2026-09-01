import { el, invoke, listen, fmtDateMs, resetError, toast, dismissToast, fillStatus } from "./shared.js";

// 账户管理：Cursor / Codex 账户的增删改查、单个 / 全部刷新与定时刷新。
// 账户数据由后端持久化，这里只维护一份内存镜像，所有写操作都以后端返回值为准；
// 刷新失败时保留旧数据，仅在对应行展示错误。

// 额度进度条分档颜色（按剩余百分比）：>50% 绿、15%~50% 橙、≤15% 红。
const QUOTA_WARN_REMAIN_PERCENT = 50;
const QUOTA_LOW_REMAIN_PERCENT = 15;

const TOKEN_PLACEHOLDERS = {
  cursor: "user_xxx::eyJ... 或 WorkosCursorSessionToken",
  codex: "~/.codex/auth.json 里的 tokens.access_token",
};

const MEMBERSHIP_LABELS = {
  free: "Free",
  pro: "Pro",
  pro_plus: "Pro+",
  ultra: "Ultra",
  enterprise: "Enterprise",
  business: "Business",
  team: "Team",
};

// Codex 套餐名映射（chatgpt_plan_type -> 展示名），未知值原样显示
const CODEX_PLAN_LABELS = {
  plus: "Plus",
  pro: "Pro",
  team: "Team",
  enterprise: "Enterprise",
  business: "Business",
  free: "Free",
  edu: "Edu",
};

// Cursor 套餐月费（美元 / 月），用量统计页用于对比等价 API 费用；未收录的套餐（如企业定制）视为未知
const CURSOR_PLAN_USD = {
  free: 0,
  pro: 20,
  pro_plus: 60,
  ultra: 200,
  business: 40,
  team: 40,
};

/** Cursor 套餐月费（USD/月）；未知套餐返回 null。 */
export function planMonthlyUsd(membershipType) {
  const key = String(membershipType ?? "").trim().toLowerCase();
  return Object.prototype.hasOwnProperty.call(CURSOR_PLAN_USD, key) ? CURSOR_PLAN_USD[key] : null;
}

let accounts = [];
let intervalMinutes = 0;
let usageIntervalMinutes = 0;
let timerId = null;
let refreshAllRunning = false;
let switching = false; // 切换账户流程进行中（全局互斥，期间禁用相关操作）
let importing = false; // 本机导入 / 文件导入导出进行中（防重入，期间禁用相关按钮）
const groupRefreshing = new Set(); // 组内整体刷新进行中的组（"cursor" / "codex"）
const refreshingIds = new Set();
const rowErrors = new Map();

let modalKind = "cursor";
let editingId = null;

/* ---------- 对外接口(供用量统计等模块联动) ---------- */

const changeListeners = new Set();

function notifyChange() {
  const snapshot = accounts.slice();
  for (const fn of changeListeners) {
    try { fn(snapshot); } catch { /* 监听方异常不影响本页 */ }
  }
}

/** 当前账户列表的浅拷贝快照。 */
export function getAccounts() {
  return accounts.slice();
}

/** 订阅账户列表变化(增删改、刷新完成)。 */
export function onAccountsChanged(fn) {
  changeListeners.add(fn);
}

/** 用量统计页的自动更新间隔(分钟,0 = 关闭),与账户状态刷新间隔相互独立。 */
export function getUsageIntervalMinutes() {
  return usageIntervalMinutes;
}

/** 保存用量统计自动更新间隔,返回后端确认后的值。 */
export async function setUsageInterval(minutes) {
  const view = await invoke("accounts_set_usage_interval", { intervalMinutes: minutes });
  applyView(view);
  render();
  return usageIntervalMinutes;
}

/* ---------- 格式化 ---------- */

export function maskToken(token) {
  const t = String(token || "").trim();
  if (!t) return "—";
  if (t.length > 20) return `${t.slice(0, 12)}…${t.slice(-4)}`;
  return `${t.slice(0, 4)}…`;
}

function fmtNum(value) {
  const n = Number(value);
  if (value == null || !Number.isFinite(n)) return "—";
  return Number.isInteger(n) ? n.toLocaleString("zh-CN") : n.toFixed(2);
}

function relativeFromMs(ms) {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return new Date(ms).toLocaleDateString("zh-CN");
}

/** 账户上次刷新时间的相对文案（如 5 分钟前），托盘面板也在用。 */
export function relativeFromUnixSeconds(seconds) {
  const n = Number(seconds);
  if (seconds == null || !Number.isFinite(n) || n <= 0) return "—";
  return relativeFromMs(n * 1000);
}

export function membershipLabel(value) {
  const key = String(value ?? "").toLowerCase();
  if (!key) return "";
  return MEMBERSHIP_LABELS[key] || String(value);
}

export function codexPlanLabel(value) {
  const key = String(value ?? "").trim().toLowerCase();
  if (!key) return "";
  return CODEX_PLAN_LABELS[key] || String(value);
}

/* ---------- 状态提示 ---------- */

function setStatus(kind, text) {
  toast(kind, text, { key: "accounts" });
}
function clearStatus() {
  dismissToast("accounts");
}
function setModalStatus(kind, text) {
  fillStatus(el("#account-modal-status"), kind, text);
}
function clearModalStatus() {
  const box = el("#account-modal-status");
  box.hidden = true;
  box.textContent = "";
}

/* ---------- 渲染 ---------- */

function fillStateCell(cell, account) {
  const status = account.status || null;
  const alive = status && typeof status.alive === "boolean" ? status.alive : null;

  const wrap = document.createElement("span");
  wrap.className = "account-state";
  const dot = document.createElement("span");
  dot.className = `dot ${alive === true ? "ok" : alive === false ? "bad" : "unknown"}`;
  const text = document.createElement("span");
  let stateText = alive === true ? "有效" : alive === false ? "已失效" : "未检测";
  // Codex 有效时在状态后附上登录凭据（Token）剩余时长，如「有效（7d）」
  if (account.kind === "codex" && alive === true) {
    const exp = Number(status && status.exp);
    if (Number.isFinite(exp) && exp > 0) {
      const info = remainInfo(exp * 1000);
      if (!info.expired) {
        stateText += `（${info.text}）`;
        text.title = `Token 到期：${fmtDateMs(exp * 1000)}（登录凭据有效期，非套餐周期）`;
      }
    }
  }
  text.textContent = stateText;
  wrap.append(dot, text);
  cell.append(wrap);

  // Codex 有 Refresh Token 时可自动续期，作为状态补充展示在第二行
  if (account.kind === "codex" && String(account.refreshToken || "").trim()) {
    const line = document.createElement("div");
    line.className = "state-tag-line";
    const tag = document.createElement("span");
    tag.className = "tag renewable";
    tag.textContent = "可续期";
    line.append(tag);
    cell.append(line);
  }

  // 错误收敛为短语，完整信息放悬停提示（避免撑开固定宽度的状态列）
  const error = rowErrors.get(account.id) || (status && status.refreshError) || "";
  if (error) {
    const e = document.createElement("span");
    e.className = "account-error";
    e.textContent = "刷新失败";
    e.title = String(error);
    cell.append(e);
  }
}

/** 到期剩余时长的紧凑文本（Nd / Nh / <1h）；已过期返回 expired=true。托盘面板也在用。 */
export function remainInfo(endMs) {
  const remainMs = endMs - Date.now();
  if (remainMs <= 0) return { text: "已到期", expired: true };
  const days = Math.floor(remainMs / 86_400_000);
  const hours = Math.floor(remainMs / 3_600_000);
  return { text: days >= 1 ? `${days}d` : hours >= 1 ? `${hours}h` : "<1h", expired: false };
}

/** 到期倒计时节点（已过期标红「已到期」），详情由调用方放 title。 */
function expiryNode(endMs) {
  const expiry = document.createElement("div");
  expiry.className = "plan-expiry";
  const info = remainInfo(endMs);
  if (info.expired) expiry.classList.add("expired");
  expiry.textContent = info.text;
  return expiry;
}

/**
 * 套餐列：第一行套餐名（Cursor 显示 membership，Codex 显示 chatgpt_plan_type，无数据为 —），
 * 第二行为套餐周期的到期倒计时（Cursor = 本期计费周期，Codex = 订阅有效期），详情放悬停提示。
 * 登录凭据（Token）有效期不属于套餐信息，展示在状态列。
 */
function fillPlanCell(cell, account) {
  const status = account.status || null;
  const label = !status
    ? ""
    : account.kind === "codex"
    ? codexPlanLabel(status.plan)
    : membershipLabel(status.membershipType);
  const name = document.createElement("div");
  name.textContent = label || "—";
  cell.append(name);
  if (!status || status.alive === false) return;
  const endIso = account.kind === "codex" ? status.planActiveUntil : status.billingCycleEnd;
  const endText = isoDateText(endIso);
  if (!endText) return;
  const expiry = expiryNode(new Date(endIso).getTime());
  const startText = isoDateText(account.kind === "codex" ? status.planActiveStart : status.billingCycleStart);
  const renewHint = account.kind === "codex"
    ? status.planWillRenew === true
      ? "（到期自动续期）"
      : status.planWillRenew === false
      ? "（到期不续期）"
      : ""
    : "（到期自动续期，额度重置）";
  expiry.title =
    account.kind === "codex"
      ? startText
        ? `订阅有效期：${startText} ~ ${endText}${renewHint}`
        : `订阅至 ${endText}${renewHint}`
      : startText
      ? `本期计费周期：${startText} ~ ${endText}${renewHint}`
      : `本期计费周期截止 ${endText}${renewHint}`;
  cell.append(expiry);
}

/** 剩余百分比 -> 分档（ok / warn / low），决定进度条颜色。 */
function quotaTier(remaining) {
  if (remaining <= QUOTA_LOW_REMAIN_PERCENT) return "low";
  if (remaining <= QUOTA_WARN_REMAIN_PERCENT) return "warn";
  return "ok";
}

// 生长动画只在首次渲染时播放：任何状态变化（如单个刷新）都会整表重绘，
// 若每次重放会导致所有进度条闪动。
let quotaIntroPlayed = false;

/**
 * 通用额度进度条（对齐 my-elink 的语义）：传入已用百分比，按「剩余百分比」填充，
 * 标签 / 进度 / 剩余值单行排布，颜色按剩余量分档（绿 / 橙 / 红）；usedPercent 无效时返回 null。
 * 值拆两段：百分比定宽（永远完整可见），附加信息（重置时间等）弹性省略。
 */
function quotaBar(label, usedPercent, extra) {
  const used = Number(usedPercent);
  if (!Number.isFinite(used)) return null;
  const remaining = Math.min(100, Math.max(0, 100 - used));
  const wrap = document.createElement("div");
  wrap.className = `quota-bar q-${quotaTier(remaining)}`;
  const name = document.createElement("span");
  name.className = "quota-bar-label";
  name.textContent = label;
  name.title = label; // 单行内可能被截断，悬停看完整文案
  const track = document.createElement("div");
  track.className = "quota-bar-track";
  const fill = document.createElement("span");
  if (quotaIntroPlayed) fill.classList.add("no-intro");
  fill.style.width = `${remaining}%`;
  track.append(fill);
  const pct = document.createElement("span");
  pct.className = "quota-bar-pct";
  pct.textContent = `${Math.round(remaining)}%`;
  pct.title = `剩余 ${Math.round(remaining)}%`;
  wrap.append(name, track, pct);
  if (extra) {
    const extraSpan = document.createElement("span");
    extraSpan.className = "quota-bar-extra";
    extraSpan.textContent = extra;
    extraSpan.title = extra;
    wrap.append(extraSpan);
  }
  return wrap;
}

/** 摘要列补充信息的行内小字项，多个项合并在一行展示。 */
function metaItem(text, bad) {
  const span = document.createElement("span");
  span.className = `summary-meta-item${bad ? " expired" : ""}`;
  span.textContent = text;
  return span;
}

function shortDate(iso) {
  const d = new Date(iso);
  if (!Number.isFinite(d.getTime())) return "";
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** ISO 日期字符串 -> 本地日期文本（省略时间，如 2026/9/21），无效返回空串。 */
function isoDateText(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  return Number.isFinite(d.getTime()) ? d.toLocaleDateString("zh-CN") : "";
}

// 摘要列拆成两部分：bars 为额度进度条（两列网格排布），meta 为一行小字补充信息
function cursorSummaryNodes(status) {
  const bars = [];
  const meta = [];
  const plan = status.plan || null;
  if (plan) {
    // 主池优先 Auto / API 两条（与 my-elink 一致），percent 字段缺失时按 used/limit/remaining 兜底
    const autoBar = quotaBar("Auto", plan.autoPercentUsed);
    const apiBar = quotaBar("API", plan.apiPercentUsed);
    if (autoBar) bars.push(autoBar);
    if (apiBar) bars.push(apiBar);
    if (!autoBar && !apiBar) {
      const used = Number(plan.used);
      const limit = Number(plan.limit);
      const remaining = Number(plan.remaining);
      let usedPct = null;
      if (limit > 0 && Number.isFinite(remaining)) usedPct = 100 - (remaining / limit) * 100;
      else if (limit > 0 && Number.isFinite(used)) usedPct = (used / limit) * 100;
      else if (Number.isFinite(remaining) && Number.isFinite(used) && remaining + used > 0) {
        usedPct = (used / (remaining + used)) * 100;
      }
      const detail = plan.used != null || plan.limit != null ? `${fmtNum(plan.used)} / ${fmtNum(plan.limit)}` : "";
      const bar = usedPct != null ? quotaBar("Plan", usedPct, detail) : null;
      if (bar) bars.push(bar);
      else if (detail) meta.push(metaItem(`额度 ${detail}`));
    }
  }
  // Grok Bot 周额度（sand）：套餐包含时展示
  const sand = status.sand || null;
  if (sand && sand.included) {
    let usedPct = Number(sand.usagePercent);
    if (sand.hasAvailableUsage === false) usedPct = 100;
    const reset = sand.nextResetAt ? `重置 ${shortDate(sand.nextResetAt)}` : "";
    const bar = quotaBar("Grok", usedPct, reset);
    if (bar) bars.push(bar);
  }
  return { bars, meta };
}

/** 美分 -> 美元文本（千分位 + 两位小数），无效值返回 —。 */
function centsText(cents) {
  const n = Number(cents);
  if (!Number.isFinite(n)) return "—";
  return usdText(n / 100);
}

/** 美元金额文本（千分位 + 两位小数）。 */
function usdText(usd) {
  const n = Number(usd);
  if (!Number.isFinite(n)) return "—";
  return `$${n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** 超额列（Cursor）：超出套餐的按需消费金额 + 可选的上限小字；未开启或无数据为 —。 */
function fillOnDemandCell(cell, account) {
  const status = account.status || null;
  const onDemand = status && status.alive !== false ? status.onDemand || null : null;
  if (!onDemand || !onDemand.enabled) {
    cell.textContent = "—";
    return;
  }
  const used = document.createElement("div");
  used.className = "ondemand-used";
  used.textContent = centsText(onDemand.used);
  cell.append(used);
  const limitCents = Number(onDemand.limit);
  if (Number.isFinite(limitCents) && limitCents > 0) {
    const limit = document.createElement("div");
    limit.className = "ondemand-limit";
    limit.textContent = `上限 ${centsText(limitCents)}`;
    cell.append(limit);
  }
}

/**
 * ChatGPT extra usage：wham/usage 的 credits.balance 是 credit 点数（可带小数），
 * 不是美元。官网余额 = floor(点数) × $0.04（本机实测 2838.51523 → $113.52）。
 */
const CODEX_CREDIT_USD = 0.04;

function creditsNumber(value) {
  if (typeof value === "number" || typeof value === "string") {
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

/**
 * 从 credits 取出点数与美元余额。结构为
 * `{ has_credits, unlimited, balance }`；兼容 available / remaining 别名。
 */
function parseCreditsUsd(credits) {
  if (credits == null) return { unlimited: false, units: null, usd: null };
  let raw = creditsNumber(credits);
  let unlimited = false;
  if (raw == null && typeof credits === "object") {
    unlimited = credits.unlimited === true;
    for (const key of ["balance", "available", "remaining"]) {
      raw = creditsNumber(credits[key]);
      if (raw != null) break;
    }
  }
  if (raw == null) return { unlimited, units: null, usd: null };
  const units = Math.floor(raw);
  return { unlimited, units, usd: units * CODEX_CREDIT_USD };
}

/**
 * 托盘第二行：Cursor 超额紧凑文案。未开启或账户无效返回 null。
 * 有上限时 title 为「上限 $x.xx」，供悬停展示。
 */
export function onDemandBrief(status) {
  const onDemand = status && status.alive !== false ? status.onDemand || null : null;
  if (!onDemand || !onDemand.enabled) return null;
  const limitCents = Number(onDemand.limit);
  return {
    text: `超额 ${centsText(onDemand.used)}`,
    title: Number.isFinite(limitCents) && limitCents > 0 ? `上限 ${centsText(limitCents)}` : "",
  };
}

/**
 * 托盘第二行：Codex Credits 余额紧凑文案。解析不出返回 null。
 * 无限显示「余额 无限」；点数放进 title。
 */
export function creditsBrief(status) {
  const credits = status && status.alive !== false ? status.credits : null;
  if (credits == null) return null;
  const { unlimited, units, usd } = parseCreditsUsd(credits);
  if (!unlimited && usd == null) return null;
  return {
    text: unlimited ? "余额 无限" : `余额 ${usdText(usd)}`,
    title: units != null ? `${units.toLocaleString("en-US")} 点` : "",
  };
}

/**
 * Credits 列（Codex，与 Cursor 超额列同位对齐）：主行美元余额，次行 credit 点数。
 * 无限额度主行显示「无限」；解析不出显示 —。
 */
function fillCreditsCell(cell, account) {
  const status = account.status || null;
  const credits = status && status.alive !== false ? status.credits : null;
  if (credits == null) {
    cell.textContent = "—";
    return;
  }
  const { unlimited, units, usd } = parseCreditsUsd(credits);
  if (!unlimited && usd == null) {
    cell.textContent = "—";
    return;
  }
  const used = document.createElement("div");
  used.className = "ondemand-used";
  used.textContent = unlimited ? "无限" : usdText(usd);
  cell.append(used);
  if (units != null) {
    const extra = document.createElement("div");
    extra.className = "ondemand-limit";
    extra.textContent = `${units.toLocaleString("en-US")} 点`;
    cell.append(extra);
  }
}

/**
 * 额度窗口的重置时刻（毫秒）：resetAt 绝对值优先；否则以锚点（账户刷新时间，unix 秒）
 * + resetsInSeconds 推算——resetsInSeconds 是拉取当时的相对值，锚定 Date.now() 会随
 * 数据陈旧而漂移。无锚点时才退回 Date.now()。取不到返回 null。
 */
function windowResetAtMs(w, anchorSec) {
  const abs = Number(w && w.resetAt);
  if (abs > 0) return abs * 1000;
  const rel = Number(w && w.resetsInSeconds);
  if (!(rel > 0)) return null;
  const anchorMs = Number(anchorSec) > 0 ? Number(anchorSec) * 1000 : Date.now();
  return anchorMs + rel * 1000;
}

/** Codex 额度窗口的重置时间：天级窗口显示日期，小时级窗口显示倒计时。 */
function windowResetText(w, anchorSec) {
  const seconds = Number(w.limitWindowSeconds);
  const resetAtMs = windowResetAtMs(w, anchorSec);
  if (!resetAtMs) return "";
  if (Number.isFinite(seconds) && seconds >= 86400) {
    const d = new Date(resetAtMs);
    return `重置 ${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  }
  const inSec = Math.max(0, Math.round((resetAtMs - Date.now()) / 1000));
  const h = Math.floor(inSec / 3600);
  const m = Math.floor((inSec % 3600) / 60);
  // 紧凑倒计时（如 3h24m 后重置），额度条单行空间有限
  return `重置 ${h > 0 ? `${h}h` : ""}${m}m`;
}

// 与 cursorSummaryNodes 结构对齐：bars 为额度窗口进度条（Token 到期倒计时见套餐列）
function codexSummaryNodes(status, anchorSec) {
  const bars = [];
  const windows = Array.isArray(status.windows) ? status.windows : [];
  for (const w of windows.slice(0, 3)) {
    if (!w) continue;
    const bar = quotaBar(w.label || "额度窗口", w.usedPercent, windowResetText(w, anchorSec));
    if (bar) bars.push(bar);
  }
  return { bars, meta: [] };
}

function fillSummaryCell(cell, account) {
  const status = account.status || null;
  const { bars, meta } = !status || status.alive === false
    ? { bars: [], meta: [] }
    : account.kind === "codex"
    ? codexSummaryNodes(status, account.lastRefreshAt)
    : cursorSummaryNodes(status);
  if (bars.length) {
    const grid = document.createElement("div");
    grid.className = "quota-grid";
    grid.append(...bars);
    cell.append(grid);
  }
  if (meta.length) {
    const row = document.createElement("div");
    row.className = "summary-meta";
    row.append(...meta);
    cell.append(row);
  }
  if (!cell.childNodes.length) cell.textContent = "—";
}

/* ---------- 操作列图标按钮 ---------- */

// feather 风格线性图标，与页面其余 SVG 一致（stroke = currentColor）
const ACTION_ICONS = {
  // 人像 + 对勾：把该账户设为本机 Cursor 的登录账户
  switch:
    '<path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"/><circle cx="8.5" cy="7" r="4"/><polyline points="17 11 19 13 23 9"/>',
  refresh:
    '<polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>',
  edit:
    '<path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/>',
  delete:
    '<polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/>',
};

/** 表格操作列的紧凑图标按钮：图标 + 悬停提示（title / aria-label）。 */
function iconAction(icon, label) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "icon-action";
  btn.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${ACTION_ICONS[icon]}</svg>`;
  btn.title = label;
  btn.setAttribute("aria-label", label);
  return btn;
}

function accountRow(account) {
  const tr = document.createElement("tr");
  tr.dataset.id = account.id;
  const busy = refreshingIds.has(account.id);

  // 账户列：列宽固定，各行超长省略号截断，悬停看全文。
  // Cursor 为三行（主显示 / 邮箱 / 打码 token），Codex 为两行（备注 / 打码 token）。
  const accountCell = document.createElement("td");
  accountCell.className = "account-cell";
  const ident = document.createElement("div");
  ident.className = "account-ident";
  const note = document.createElement("span");
  note.className = "account-note";
  const token = document.createElement("code");
  token.className = "masked-token";
  if (account.kind === "cursor") {
    // 主显示：手填备注 > 刷新返回的用户名 > 自动备注
    const statusName = String((account.status && account.status.name) || "").trim();
    const primary =
      (account.noteAuto === false ? account.note : "") || statusName || account.note || "未命名账户";
    note.textContent = primary;
    note.title = primary;
    ident.append(note);
    const email = String((account.status && account.status.email) || "").trim();
    if (email && email !== primary) {
      const emailSpan = document.createElement("span");
      emailSpan.className = "account-email";
      emailSpan.textContent = email;
      emailSpan.title = email;
      ident.append(emailSpan);
    }
    // 与 Codex 一致，仅展示保留两端、中间省略的打码 token
    token.textContent = maskToken(account.token);
    token.title = token.textContent;
  } else {
    note.textContent = account.note || "未命名账户";
    note.title = account.note || "";
    ident.append(note);
    token.textContent = maskToken(account.token);
  }
  accountCell.append(ident, token);

  const stateCell = document.createElement("td");
  fillStateCell(stateCell, account);

  const planCell = document.createElement("td");
  fillPlanCell(planCell, account);

  const summaryCell = document.createElement("td");
  fillSummaryCell(summaryCell, account);

  // 超额 / Credits 列：Cursor 显示超额消费，Codex 显示 Credits 余额（两表同位对齐）
  const extraCell = document.createElement("td");
  if (account.kind === "cursor") fillOnDemandCell(extraCell, account);
  else fillCreditsCell(extraCell, account);

  const timeCell = document.createElement("td");
  timeCell.className = "account-time";
  timeCell.textContent = relativeFromUnixSeconds(account.lastRefreshAt);

  const actionsCell = document.createElement("td");
  actionsCell.className = "account-actions";
  const refreshBtn = iconAction("refresh", busy ? "刷新中…" : "刷新");
  refreshBtn.classList.toggle("busy", busy);
  refreshBtn.disabled = busy || refreshAllRunning || switching;
  refreshBtn.addEventListener("click", () => { void refreshOne(account.id); });
  const editBtn = iconAction("edit", "编辑");
  editBtn.disabled = busy || switching;
  editBtn.addEventListener("click", () => openModal(account));
  // 删除使用显式确认弹窗，避免首次点击只变色而看起来没有响应。
  const deleteBtn = iconAction("delete", "删除");
  deleteBtn.disabled = busy || switching;
  deleteBtn.addEventListener("click", () => { void onDeleteClick(account.id); });
  if (account.kind === "cursor") {
    // 仅允许切换已验证有效的账户，避免把失效 token 写进本地 Cursor
    const aliveOk = !!(account.status && account.status.alive === true);
    const switchBtn = iconAction("switch", aliveOk ? "切换到该账户" : "请先刷新验证账户后再切换");
    switchBtn.disabled = busy || switching || !aliveOk;
    switchBtn.addEventListener("click", () => { void onSwitchAccount(account.id); });
    actionsCell.append(switchBtn, refreshBtn, editBtn, deleteBtn);
  } else {
    // Codex 切换靠 Refresh Token 换新凭据，未验证或没有 Refresh Token 的账户不可切
    const aliveOk = !!(account.status && account.status.alive === true);
    const hasRt = !!String(account.refreshToken || "").trim();
    const switchBtn = iconAction(
      "switch",
      !aliveOk
        ? "请先刷新验证账户后再切换"
        : !hasRt
        ? "该账户没有 Refresh Token，无法切换本机登录"
        : "切换本机 ChatGPT 登录（会关闭正在运行的 ChatGPT）"
    );
    switchBtn.disabled = busy || switching || !aliveOk || !hasRt;
    switchBtn.addEventListener("click", () => { void onSwitchCodexAccount(account.id); });
    actionsCell.append(switchBtn, refreshBtn, editBtn, deleteBtn);
  }

  tr.append(accountCell, stateCell, planCell, summaryCell, extraCell, timeCell, actionsCell);
  return tr;
}

/** 组标题旁的统计小字：数量 + 组内最近一次刷新的相对时间，组空时留空。 */
function updateGroupMeta(kind) {
  const rows = accounts.filter((a) => a.kind === kind);
  let ts = 0;
  for (const account of rows) {
    const v = Number(account && account.lastRefreshAt);
    if (Number.isFinite(v) && v > 0) ts = Math.max(ts, v * 1000);
  }
  const text = ts ? `共 ${rows.length} 个 · 上次刷新：${relativeFromMs(ts)}` : `共 ${rows.length} 个`;
  el(`#accounts-count-${kind}`).textContent = rows.length ? text : "";
}

function kindDisplay(kind) {
  return kind === "codex" ? "ChatGPT" : "Cursor";
}

/** 卡片标题栏的刷新 / 导入 / 导出 / 添加按钮：互斥流程中禁用，组刷新进行中时刷新按钮转圈。 */
function updateHeadingActions() {
  const blocked = switching || importing || refreshAllRunning;
  for (const kind of ["cursor", "codex"]) {
    const running = groupRefreshing.has(kind) || refreshAllRunning;
    const refreshBtn = el(`#accounts-refresh-${kind}`);
    refreshBtn.disabled = blocked || groupRefreshing.has(kind);
    refreshBtn.classList.toggle("busy", running);
    el(`#accounts-import-${kind}`).disabled = blocked;
    const hasRows = accounts.some((a) => a.kind === kind);
    el(`#accounts-export-${kind}`).disabled = blocked || !hasRows;
    el(`#accounts-add-${kind}`).disabled = blocked;
  }
}

function renderGroup(kind, bodyId, emptyId) {
  const rows = accounts.filter((a) => a.kind === kind);
  el(bodyId).replaceChildren(...rows.map(accountRow));
  el(emptyId).hidden = rows.length > 0;
  updateGroupMeta(kind);
}

function render() {
  renderGroup("cursor", "#accounts-body-cursor", "#accounts-empty-cursor");
  renderGroup("codex", "#accounts-body-codex", "#accounts-empty-codex");
  updateHeadingActions();
  // 首批进度条渲染完成后，后续重绘不再重放生长动画
  if (!quotaIntroPlayed && document.querySelector(".quota-bar")) quotaIntroPlayed = true;
}

/** 只重建单个账户行（如刷新前后），避免整表重绘打断其它行的动画；行不存在时退回全量渲染。 */
function updateRow(id) {
  const acc = accounts.find((a) => a.id === id);
  if (!acc) {
    render();
    return;
  }
  const bodyId = acc.kind === "codex" ? "#accounts-body-codex" : "#accounts-body-cursor";
  const old = el(bodyId).querySelector(`tr[data-id="${CSS.escape(id)}"]`);
  if (!old) {
    render();
    return;
  }
  old.replaceWith(accountRow(acc));
  if (!quotaIntroPlayed && document.querySelector(".quota-bar")) quotaIntroPlayed = true;
  updateGroupMeta(acc.kind);
  updateHeadingActions();
}

function applyView(view) {
  accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
  const n = Number(view && view.intervalMinutes);
  if (Number.isFinite(n)) intervalMinutes = n;
  const u = Number(view && view.usageIntervalMinutes);
  if (Number.isFinite(u)) usageIntervalMinutes = u;
  el("#accounts-interval").value = String(intervalMinutes);
  // 清理已被移除账户的瞬态状态
  const ids = new Set(accounts.map((a) => a.id));
  for (const id of [...rowErrors.keys()]) {
    if (!ids.has(id)) rowErrors.delete(id);
  }
  notifyChange();
}

/* ---------- 后端数据变更同步 ---------- */

// 任何窗口触发的账户写入（托盘刷新 / 切换、Codex 自动续期等）后端都会广播
// accounts-changed 事件；密集写入（如全部刷新）用防抖合并，只应用最后一份视图。
let syncTimer = null;
let syncPendingView = null;

function onBackendChanged(view) {
  if (!view) return;
  syncPendingView = view;
  if (syncTimer != null) return;
  syncTimer = setTimeout(() => {
    syncTimer = null;
    const v = syncPendingView;
    syncPendingView = null;
    // 其他入口刷新成功（刷新时间变化）的账户，本地残留的行错误已过时，清掉避免矛盾展示
    const prevAt = new Map(accounts.map((a) => [a.id, a.lastRefreshAt]));
    for (const a of Array.isArray(v.accounts) ? v.accounts : []) {
      if (rowErrors.has(a.id) && a.lastRefreshAt !== prevAt.get(a.id)) rowErrors.delete(a.id);
    }
    applyView(v);
    render();
  }, 150);
}

/* ---------- 刷新 ---------- */

async function refreshOne(id) {
  if (refreshingIds.has(id)) return null;
  if (!accounts.some((a) => a.id === id)) return null;
  refreshingIds.add(id);
  rowErrors.delete(id);
  updateRow(id);
  try {
    const updated = await invoke("account_refresh", { id });
    // codex 可能轮换 token / refreshToken，必须整体替换该账户
    const index = accounts.findIndex((a) => a.id === id);
    if (index >= 0 && updated && typeof updated === "object") {
      accounts[index] = updated;
      return updated;
    }
    return null;
  } catch (error) {
    rowErrors.set(id, resetError(error));
    return null;
  } finally {
    refreshingIds.delete(id);
    updateRow(id);
    notifyChange();
  }
}

/** 排队刷新一批账户（每次一个，逐个执行），返回失败个数。 */
async function refreshIds(ids) {
  for (const id of ids) await refreshOne(id);
  return ids.filter((id) => rowErrors.has(id)).length;
}

/** 刷新单组（卡片标题栏的刷新按钮）：组内账户排队逐个刷新。 */
async function refreshGroup(kind) {
  if (groupRefreshing.has(kind) || refreshAllRunning || switching || importing) return;
  const ids = accounts.filter((a) => a.kind === kind).map((a) => a.id);
  if (!ids.length) return;
  groupRefreshing.add(kind);
  clearStatus();
  updateHeadingActions();
  const failed = await refreshIds(ids);
  groupRefreshing.delete(kind);
  if (failed > 0) setStatus("warn", `刷新完成，${failed} 个账户失败，详见对应行。`);
  updateGroupMeta(kind);
  updateHeadingActions();
}

/** 全部账户排队逐个刷新（定时刷新与启动自动刷新用）；切换/导入进行中跳过本次。 */
async function refreshAll() {
  if (refreshAllRunning || switching || importing) return;
  const ids = accounts.map((a) => a.id);
  if (!ids.length) return;
  refreshAllRunning = true;
  clearStatus();
  render();
  const failed = await refreshIds(ids);
  refreshAllRunning = false;
  if (failed > 0) setStatus("warn", `刷新完成，${failed} 个账户失败，详见对应行。`);
  render();
}

function rebuildTimer() {
  if (timerId != null) {
    clearInterval(timerId);
    timerId = null;
  }
  if (intervalMinutes > 0) {
    timerId = setInterval(() => {
      // 上一轮仍在进行时跳过本次定时触发
      if (!refreshAllRunning) void refreshAll();
    }, intervalMinutes * 60_000);
  }
}

/** 账户定时刷新间隔（分钟，0 = 关闭），设置弹窗用于回显。 */
export function getRefreshIntervalMinutes() {
  return intervalMinutes;
}

/** 保存账户定时刷新间隔并重建定时器，返回后端确认后的值；失败向上抛（由设置弹窗提示）。 */
export async function setRefreshInterval(minutes) {
  const view = await invoke("accounts_set_interval", { intervalMinutes: minutes });
  const n = Number(view && view.intervalMinutes);
  intervalMinutes = Number.isFinite(n) ? n : minutes;
  el("#accounts-interval").value = String(intervalMinutes);
  rebuildTimer();
  return intervalMinutes;
}

/* ---------- 删除（显式确认） ---------- */

async function onDeleteClick(id) {
  if (switching || refreshingIds.has(id)) return;
  const account = accounts.find((item) => item.id === id);
  if (!account) return;
  const name = String(account.note || (account.status && account.status.name) || "未命名账户").trim();
  const ok = await switchModal.toConfirm({
    title: "删除账户",
    body: `确定删除“${name}”吗？此操作无法撤销。`,
    confirmText: "删除",
    danger: true,
  });
  switchModal.close();
  if (ok) await doDelete(id);
}

async function doDelete(id) {
  try {
    const view = await invoke("accounts_delete", { id });
    applyView(view);
    setStatus("ok", "账户已删除。");
  } catch (error) {
    setStatus("bad", `删除失败：${resetError(error)}`);
  }
  render();
}

/* ---------- 切换账户（写入本地 Cursor / Codex 登录态） ---------- */

/**
 * 账户操作弹窗（#switch-modal）：删除确认，以及切换账户的确认 → 分步进度 → 结果。
 * busy 阶段（openBusy / toSteps 之后、finish 之前）忽略 Esc 与关闭按钮，防止流程中途被关；
 * 事件由 initAccounts 一次性绑定，按当前阶段分发。
 */
const switchModal = {
  phase: "hidden", // hidden | busy | confirm | steps | done
  stepIndex: -1,
  confirmResolve: null,

  /** 打开弹窗并显示忙碌文案（如「正在检测本地 Cursor…」），期间不可关闭。 */
  openBusy(title, text) {
    el("#switch-modal-title").textContent = title;
    const body = el("#switch-modal-body");
    body.hidden = false;
    body.textContent = text;
    const steps = el("#switch-steps");
    steps.hidden = true;
    steps.replaceChildren();
    const status = el("#switch-modal-status");
    status.hidden = true;
    status.textContent = "";
    el("#switch-modal .modal-foot").hidden = true;
    this.phase = "busy";
    this.stepIndex = -1;
    el("#switch-modal").hidden = false;
  },

  /** 切到确认阶段：显示正文与取消 / 确认按钮，返回用户选择（Esc / 关闭 / 取消 = false）。 */
  toConfirm({ title, body, confirmText, danger }) {
    return new Promise((resolve) => {
      if (title) el("#switch-modal-title").textContent = title;
      const text = el("#switch-modal-body");
      text.hidden = false;
      text.textContent = body || "";
      el("#switch-steps").hidden = true;
      el("#switch-modal-status").hidden = true;
      el("#switch-modal .modal-foot").hidden = false;
      el("#switch-cancel").hidden = false;
      const ok = el("#switch-ok");
      ok.hidden = false;
      ok.textContent = confirmText || "确认";
      ok.classList.toggle("danger", !!danger);
      this.phase = "confirm";
      this.confirmResolve = resolve;
      el("#switch-modal").hidden = false;
      ok.focus();
    });
  },

  /** 渲染步骤列表（全部待办）并进入执行阶段，期间不可关闭。 */
  toSteps(labels) {
    el("#switch-modal-body").hidden = true;
    el("#switch-modal .modal-foot").hidden = true;
    const box = el("#switch-steps");
    box.replaceChildren(
      ...labels.map((label) => {
        const item = document.createElement("div");
        item.className = "switch-step pending";
        const icon = document.createElement("span");
        icon.className = "switch-step-icon";
        const text = document.createElement("span");
        text.textContent = label;
        item.append(icon, text);
        return item;
      })
    );
    box.hidden = false;
    this.phase = "steps";
    this.stepIndex = -1;
  },

  /** 下一步进入执行中。 */
  stepStart() {
    this.stepIndex += 1;
    const step = el("#switch-steps").children[this.stepIndex];
    if (step) step.className = "switch-step running";
  },

  /** 结算当前执行中的步骤（无执行中步骤时安全空操作，便于 catch 里无脑调用）。 */
  settleStep(state) {
    const step = el("#switch-steps").children[this.stepIndex];
    if (step && step.classList.contains("running")) step.className = `switch-step ${state}`;
  },
  stepDone() {
    this.settleStep("done");
  },
  stepFail() {
    this.settleStep("fail");
  },

  /** 展示最终结果，底部变单个「关闭」按钮，此后允许 Esc / 关闭。 */
  finish(ok, message) {
    el("#switch-modal-body").hidden = true;
    const status = el("#switch-modal-status");
    status.hidden = false;
    status.className = `status ${ok ? "ok" : "bad"}`;
    status.textContent = message;
    el("#switch-modal .modal-foot").hidden = false;
    el("#switch-cancel").hidden = true;
    const btn = el("#switch-ok");
    btn.hidden = false;
    btn.textContent = "关闭";
    btn.classList.remove("danger");
    this.phase = "done";
    btn.focus();
  },

  /** 隐藏弹窗并复位。 */
  close() {
    if (this.confirmResolve) {
      const resolve = this.confirmResolve;
      this.confirmResolve = null;
      resolve(false);
    }
    el("#switch-modal").hidden = true;
    el("#switch-modal-body").hidden = false;
    el("#switch-steps").hidden = true;
    el("#switch-modal-status").hidden = true;
    el("#switch-ok").classList.remove("danger");
    this.phase = "hidden";
    this.stepIndex = -1;
  },

  /** Esc / 关闭按钮 / 取消的统一入口：busy 阶段忽略，确认阶段视为取消（由调用方 close）。 */
  requestClose() {
    if (this.phase === "busy" || this.phase === "steps") return;
    if (this.phase === "confirm") {
      const resolve = this.confirmResolve;
      this.confirmResolve = null;
      if (resolve) resolve(false);
      return;
    }
    if (this.phase === "done") this.close();
  },

  /** 主按钮点击：确认阶段 = 确认，结果阶段 = 关闭。 */
  onOk() {
    if (this.phase === "confirm") {
      const resolve = this.confirmResolve;
      this.confirmResolve = null;
      if (resolve) resolve(true);
      return;
    }
    if (this.phase === "done") this.close();
  },
};

/** 后端切换流程错误码 -> 用户可读文案，未知错误码原样显示。 */
function mapSwitchError(err) {
  const code = resetError(err);
  // 写登录文件失败的错误码带冒号细节（codex_auth_write_failed:…），按前缀匹配
  if (code.startsWith("codex_auth_write_failed")) {
    return "写入本机 ChatGPT 登录文件失败，请检查文件权限后重试。";
  }
  const M = {
    account_not_verified: "该账户尚未验证有效，请先刷新后再切换。",
    cursor_running: "Cursor 仍在运行，请关闭后重试。",
    cursor_exe_not_found: "未找到 Cursor 可执行文件，请在设置中配置路径。",
    cursor_exe_invalid: "配置的 Cursor 路径无效或文件不存在。",
    state_db_not_found: "未找到 Cursor 认证库(state.vscdb)，请确认已安装并至少启动过一次 Cursor。",
    unsupported_platform: "当前系统不支持该操作。",
    not_cursor_account: "该账户不是 Cursor 账户。",
    not_codex_account: "该账户不是 ChatGPT 账户。",
    empty_token: "账户 Token 为空。",
    invalid_session_token: "User Token 已失效，请在账户管理更新后重试。",
    poll_timeout: "未能换取登录凭证，请重试。",
    session_exchange_failed: "换取登录凭证失败，请检查网络或稍后重试。",
    codex_running: "ChatGPT 仍在运行，请关闭后重试。",
    codex_exe_not_found: "未找到 ChatGPT 可执行文件，请在设置中配置路径。",
    codex_exe_invalid: "配置的 ChatGPT 路径无效或文件不存在。",
    codex_no_refresh_token: "该账户没有 Refresh Token，无法切换本机登录。",
    codex_refresh_denied: "Refresh Token 已失效，请编辑账户更新凭据后重试。",
    codex_id_token_missing: "未能获取登录所需的 id_token，请稍后重试。",
  };
  return M[code] || code;
}

// 编排完整切换流程：检测状态 ->（必要时确认并关闭 Cursor）-> 写入登录态 -> 启动 Cursor，
// 确认、分步进度与结果全程在切换弹窗内展示。
async function onSwitchAccount(id) {
  if (switching) return;
  const acc = accounts.find((a) => a.id === id);
  if (!acc || !(acc.status && acc.status.alive === true)) return; // 未验证不切
  switching = true;
  render();
  switchModal.openBusy("切换本机 Cursor 登录", "正在检测本地 Cursor…");
  try {
    const st = await invoke("cursor_client_status", { id });
    if (!st.exeConfigured) {
      switchModal.finish(false, "未找到 Cursor 可执行文件，请点右上角「设置」配置 Cursor 路径后重试。");
      return;
    }
    const running = !!st.running;
    const ok = await switchModal.toConfirm(
      running
        ? {
            body: "检测到 Cursor 正在运行。切换需要先关闭它，未保存的内容可能会丢失。确定继续？",
            confirmText: "关闭并切换",
            danger: true,
          }
        : {
            body: "将把该账户写入本机 Cursor 登录并启动 Cursor。确定继续？",
            confirmText: "切换",
          }
    );
    if (!ok) {
      switchModal.close();
      return;
    }
    switchModal.toSteps(running ? ["关闭 Cursor", "换取登录凭证并写入", "启动 Cursor"] : ["换取登录凭证并写入", "启动 Cursor"]);
    if (running) {
      switchModal.stepStart();
      const c = await invoke("cursor_client_close");
      if (!c.closed) {
        switchModal.stepFail();
        switchModal.finish(false, "未能完全关闭 Cursor，请手动关闭后重试。");
        return;
      }
      switchModal.stepDone();
    }
    switchModal.stepStart();
    await invoke("cursor_switch_local", { id });
    switchModal.stepDone();
    switchModal.stepStart();
    const l = await invoke("cursor_client_launch");
    // 登录已写入成功，自动启动失败不算步骤失败，只在结果里提示手动启动
    switchModal.stepDone();
    switchModal.finish(true, l.launched ? "已切换账户并启动 Cursor。" : "已切换账户，但自动启动失败，请手动启动 Cursor。");
  } catch (error) {
    switchModal.stepFail();
    switchModal.finish(false, `切换失败：${mapSwitchError(error)}`);
  } finally {
    switching = false;
    render();
  }
}

// ChatGPT 切换：检测客户端 ->（必要时确认并关闭）-> 换票写入 auth.json -> 启动，
// 确认、分步进度与结果全程在切换弹窗内展示。
async function onSwitchCodexAccount(id) {
  if (switching) return;
  const acc = accounts.find((a) => a.id === id);
  const hasRt = !!(acc && String(acc.refreshToken || "").trim());
  if (!acc || !(acc.status && acc.status.alive === true) || !hasRt) return;
  switching = true;
  render();
  switchModal.openBusy("切换本机 ChatGPT 登录", "正在检测本地 ChatGPT…");
  try {
    const st = await invoke("codex_client_status", { id });
    if (!st.exeConfigured) {
      switchModal.finish(false, "未找到 ChatGPT，请点右上角「设置」配置路径后重试。");
      return;
    }
    const running = !!st.running;
    const ok = await switchModal.toConfirm(
      running
        ? {
            body: "检测到 ChatGPT 正在运行。切换需要先关闭它，未保存的内容可能会丢失。确定继续？",
            confirmText: "关闭并切换",
            danger: true,
          }
        : {
            body: "将把该账户写入本机 ChatGPT 登录并启动 ChatGPT。确定继续？",
            confirmText: "切换",
          }
    );
    if (!ok) {
      switchModal.close();
      return;
    }
    switchModal.toSteps(running ? ["关闭 ChatGPT", "换取登录凭证并写入", "启动 ChatGPT"] : ["换取登录凭证并写入", "启动 ChatGPT"]);
    if (running) {
      switchModal.stepStart();
      const c = await invoke("codex_client_close");
      if (!c.closed) {
        switchModal.stepFail();
        switchModal.finish(false, "未能完全关闭 ChatGPT，请手动关闭后重试。");
        return;
      }
      switchModal.stepDone();
    }
    switchModal.stepStart();
    await invoke("codex_switch_local", { id });
    switchModal.stepDone();
    const view = await invoke("accounts_list");
    applyView(view);
    render();
    switchModal.stepStart();
    const l = await invoke("codex_client_launch");
    switchModal.stepDone();
    switchModal.finish(true, l.launched ? "已切换账户并启动 ChatGPT。" : "已切换账户，但自动启动失败，请手动启动 ChatGPT。");
  } catch (error) {
    switchModal.stepFail();
    switchModal.finish(false, `切换失败：${mapSwitchError(error)}`);
  } finally {
    switching = false;
    render();
  }
}

/* ---------- 从本机导入 ---------- */

// 读取本机已登录的指定类型凭据并加入托管列表（Cursor 读认证库，ChatGPT 读 auth.json）；
// 后端保证同一账号不重复导入，导入后逐个后台验证。
async function onImportLocal(kind) {
  if (importing || switching || refreshAllRunning) return;
  const target = kind === "codex" ? "codex" : "cursor";
  const label = kindDisplay(target);
  importing = true;
  setStatus("", `正在读取本机 ${label} 登录…`);
  render();
  try {
    const r = await invoke("accounts_import_local", { kind: target });
    applyView(r.view);
    render();
    const imported = Array.isArray(r.imported) ? r.imported : [];
    const skipped = Array.isArray(r.skipped) ? r.skipped : [];
    const exists = skipped.filter((s) => s && s.reason === "exists").length;
    const invalid = skipped.filter((s) => s && s.reason === "invalid").length;
    if (imported.length) {
      const labels = imported.map((item) => item.label).join("、");
      const tail = exists > 0 ? `；${exists} 个已存在跳过` : "";
      setStatus("ok", `已导入 ${imported.length} 个账户：${labels}${tail}`);
    } else if (exists > 0) {
      setStatus("", `本机登录的 ${label} 账号已在列表中。`);
    } else if (invalid > 0) {
      setStatus("warn", `${invalid} 个本机凭据无法解析，已跳过。`);
    } else {
      setStatus("warn", `未检测到本机已登录的 ${label} 账号。`);
    }
    // 新导入的账户尚未验证，后台排队逐个刷新，不阻塞汇总提示
    void refreshIds(imported.map((item) => item.id));
  } catch (error) {
    setStatus("bad", `导入失败：${resetError(error)}`);
  } finally {
    importing = false;
    render();
  }
}

/* ---------- JSON 文件导入 / 导出 ---------- */

async function onExport(kind) {
  if (importing || switching || refreshAllRunning) return;
  const target = kind === "codex" ? "codex" : "cursor";
  const label = kindDisplay(target);
  importing = true;
  setStatus("", `正在导出 ${label} 账户…`);
  render();
  try {
    const r = await invoke("accounts_export", { kind: target });
    if (r && r.cancelled) {
      clearStatus();
      return;
    }
    const count = Number(r && r.count) || 0;
    setStatus("ok", `已导出 ${count} 个 ${label} 账户。`);
  } catch (error) {
    const msg = resetError(error);
    if (msg === "no_accounts") setStatus("warn", `没有可导出的 ${label} 账户。`);
    else setStatus("bad", `导出失败：${msg}`);
  } finally {
    importing = false;
    render();
  }
}

async function onImportFile(kind) {
  if (importing || switching || refreshAllRunning) return;
  const target = kind === "codex" ? "codex" : "cursor";
  const label = kindDisplay(target);
  importing = true;
  setStatus("", `正在导入 ${label} 账户…`);
  render();
  try {
    const r = await invoke("accounts_import_file", { kind: target });
    if (r && r.cancelled) {
      clearStatus();
      return;
    }
    applyView(r.view);
    render();
    const imported = Array.isArray(r.imported) ? r.imported : [];
    const skipped = Array.isArray(r.skipped) ? r.skipped : [];
    const exists = skipped.filter((s) => s && s.reason === "exists").length;
    const invalid = skipped.filter((s) => s && s.reason === "invalid").length;
    if (imported.length) {
      const labels = imported.map((item) => item.label).join("、");
      const tail = exists > 0 ? `；${exists} 个已存在跳过` : "";
      setStatus("ok", `已导入 ${imported.length} 个账户：${labels}${tail}`);
      void refreshIds(imported.map((item) => item.id));
    } else if (exists > 0) {
      setStatus("", "文件中的账号均已在列表中。");
    } else if (invalid > 0) {
      setStatus("warn", `${invalid} 个账户无法解析，已跳过。`);
    } else {
      setStatus("warn", "文件中没有可导入的账户。");
    }
  } catch (error) {
    const msg = resetError(error);
    if (msg === "kind_mismatch") setStatus("bad", `该文件不是 ${label} 账户，无法导入到当前列表。`);
    else if (msg === "invalid_format") setStatus("bad", "不是有效的账户导出文件。");
    else setStatus("bad", `导入失败：${msg}`);
  } finally {
    importing = false;
    render();
  }
}

/* ---------- 添加 / 编辑弹窗 ---------- */

function setModalKind(kind) {
  modalKind = kind === "codex" ? "codex" : "cursor";
  for (const seg of el("#account-kind").querySelectorAll(".seg")) {
    seg.classList.toggle("active", seg.dataset.kind === modalKind);
  }
  el("#account-token").placeholder = TOKEN_PLACEHOLDERS[modalKind];
  el("#account-refresh-field").hidden = modalKind !== "codex";
  el("#account-import-local").textContent = `从本机导入 ${kindDisplay(modalKind)}`;
}

function openModal(account, presetKind) {
  editingId = account ? account.id : null;
  el("#account-modal-title").textContent = account ? "编辑账户" : "添加账户";
  el("#account-note").value = account ? account.note || "" : "";
  el("#account-token").value = account ? account.token || "" : "";
  el("#account-refresh-token").value = account ? account.refreshToken || "" : "";
  setModalKind(account ? account.kind : presetKind || "cursor");
  // 编辑时不允许切换账户类型
  for (const seg of el("#account-kind").querySelectorAll(".seg")) seg.disabled = !!account;
  // 「从本机导入」是添加场景的快捷入口，编辑时隐藏
  el("#account-import-local").hidden = !!account;
  clearModalStatus();
  el("#account-modal").hidden = false;
  el("#account-note").focus();
}

function closeModal() {
  el("#account-modal").hidden = true;
  editingId = null;
}

async function onSave() {
  const token = el("#account-token").value.trim();
  if (!token) {
    setModalStatus("bad", "请填写 Token。");
    return;
  }
  const note = el("#account-note").value.trim();
  const refreshToken = modalKind === "codex" ? el("#account-refresh-token").value.trim() || null : null;
  const isEdit = editingId != null;
  const id = editingId;
  const existingIds = isEdit ? null : new Set(accounts.map((account) => account.id));
  const btn = el("#account-save");
  btn.disabled = true;
  setModalStatus("", "保存中…");
  try {
    const view = isEdit
      ? await invoke("accounts_update", { id, note, token, refreshToken })
      : await invoke("accounts_add", { account: { kind: modalKind, note, token, refreshToken } });
    applyView(view);
    const added = existingIds ? accounts.find((account) => !existingIds.has(account.id)) : null;
    render();
    closeModal();
    setStatus("ok", isEdit ? "账户已更新。" : "账户已添加。");
    // 手动新增与本机/文件导入保持一致：保存后后台拉取一次完整状态，失败留在对应账户行提示。
    if (added) void refreshOne(added.id);
  } catch (error) {
    const msg = resetError(error);
    if (msg === "duplicate_account") setModalStatus("bad", "该账号已存在，请勿重复添加。");
    else setModalStatus("bad", `保存失败：${msg}`);
  } finally {
    btn.disabled = false;
  }
}

/* ---------- 初始化 ---------- */

/** 账户数据加载错误码 -> 用户可读文案，未知错误原样显示。 */
function mapLoadError(err) {
  const msg = resetError(err);
  if (msg.includes("settings_parse_failed") || msg.includes("accounts_parse_failed")) {
    return "设置文件损坏，原文件已备份为 settings.json.bad，未被覆盖；请检查后重启应用。";
  }
  if (msg.includes("settings_read_failed") || msg.includes("accounts_read_failed")) {
    return "设置文件暂时无法读取（可能被安全软件短暂占用），请稍后重试或重启应用。";
  }
  return msg;
}

async function loadInitial(allowRetry = true) {
  try {
    const view = await invoke("accounts_list");
    applyView(view);
    render();
    if (intervalMinutes > 0) {
      // 先展示缓存状态，3 秒后自动整体刷新一轮
      setTimeout(() => { void refreshAll(); }, 3000);
      rebuildTimer();
    }
  } catch (error) {
    // 文件被短暂占用属于瞬态故障，自动重试一次再报错
    if (allowRetry) {
      setStatus("", "账户数据暂时不可读，正在重试…");
      setTimeout(() => { void loadInitial(false); }, 1500);
      return;
    }
    setStatus("bad", `加载账户失败：${mapLoadError(error)}`);
  }
}

export function initAccounts() {
  el("#accounts-add-cursor").addEventListener("click", () => openModal(null, "cursor"));
  el("#accounts-add-codex").addEventListener("click", () => openModal(null, "codex"));
  el("#accounts-refresh-cursor").addEventListener("click", () => { void refreshGroup("cursor"); });
  el("#accounts-refresh-codex").addEventListener("click", () => { void refreshGroup("codex"); });
  el("#accounts-import-cursor").addEventListener("click", () => { void onImportFile("cursor"); });
  el("#accounts-import-codex").addEventListener("click", () => { void onImportFile("codex"); });
  el("#accounts-export-cursor").addEventListener("click", () => { void onExport("cursor"); });
  el("#accounts-export-codex").addEventListener("click", () => { void onExport("codex"); });

  const modal = el("#account-modal");
  for (const node of modal.querySelectorAll("[data-close]")) {
    node.addEventListener("click", closeModal);
  }
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) closeModal();
  });
  for (const seg of el("#account-kind").querySelectorAll(".seg")) {
    seg.addEventListener("click", () => setModalKind(seg.dataset.kind));
  }
  el("#account-save").addEventListener("click", () => { void onSave(); });
  el("#account-import-local").addEventListener("click", () => {
    const kind = modalKind;
    closeModal();
    void onImportLocal(kind);
  });

  // 切换流程弹窗：事件一次性绑定，按 switchModal 当前阶段分发
  el("#switch-ok").addEventListener("click", () => switchModal.onOk());
  el("#switch-cancel").addEventListener("click", () => switchModal.requestClose());
  el("#switch-close").addEventListener("click", () => switchModal.requestClose());
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !el("#switch-modal").hidden) switchModal.requestClose();
  });

  // 订阅后端广播：托盘面板等其他入口改动账户数据后，本表实时同步
  listen("accounts-changed", (event) => onBackendChanged(event.payload)).catch(() => {
    /* 非 Tauri 环境（浏览器直开调试）无事件桥，忽略 */
  });

  // 「上次刷新」相对时间随时间流逝定期重算：只改时间文本与组标题小字，
  // 不整表重绘（避免打断进度条动画与删除确认态）
  setInterval(() => {
    if (!accounts.length) return;
    for (const account of accounts) {
      const bodyId = account.kind === "codex" ? "#accounts-body-codex" : "#accounts-body-cursor";
      const cell = el(bodyId).querySelector(`tr[data-id="${CSS.escape(account.id)}"] .account-time`);
      if (cell) cell.textContent = relativeFromUnixSeconds(account.lastRefreshAt);
    }
    updateGroupMeta("cursor");
    updateGroupMeta("codex");
  }, 30_000);

  render();
  void loadInitial();
}
