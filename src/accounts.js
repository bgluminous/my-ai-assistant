import { errorCode, locale, translate } from "./i18n.js";
import { createCodexAuthEditor } from "./codex_auth_editor.js";
import { createCodexAuthAdder } from "./codex_auth_add.js";
import { loadAccountList, trackAccountOrder, orderHandle, initRowOrder } from "./account_order.js";
import { createAccountRefreshScheduler, refreshByProvider } from "./account_refresh.js";
import { switchLocalClients, bindClaudeDesktop, clientError, startSwitchResultCountdown } from "./linked_client_ui.js";
import { createModal } from "./modal.js";
import { addOpenCode, editOpenCode, switchOpenCode } from "./opencode_ui.js";
import {
  el,
  fmtInt,
  invoke,
  listen,
  fmtDateMs,
  resetError,
  toast,
  dismissToast,
  fillStatus,
  kindLabel,
  iconAction,
} from "./shared.js";
import {
  maskToken,
  relativeFromMs,
  relativeFromUnixSeconds,
  accountPlanInfo,
  accountDateInfo,
  quotaResetInfo, quotaWindowLabel,
  remainInfo,
  centsText,
  usdText,
  parseCreditsUsd,
  resetCreditsBrief,
  cursorIdentity,
} from "./account_format.js";
import { clearCursorLocalData, syncCursorUsageFilters, syncCodexUsage } from "./usage_data.js";
import { syncError } from "./usage_sync.js";

// 账户管理：Cursor / Codex / Claude / OpenCode 的增删改查、手动刷新与账户独立定时刷新。
// 账户数据由后端持久化，这里只维护一份内存镜像，所有写操作都以后端返回值为准；
// 刷新失败时保留旧数据，仅在对应行展示错误。
// 套餐名 / 月费、Token 打码、相对时间等纯格式化助手见 account_format.js（托盘面板共用）。

// 额度进度条分档颜色（按剩余百分比）：>50% 绿、15%~50% 橙、≤15% 红。
const QUOTA_WARN_REMAIN_PERCENT = 50;
const QUOTA_LOW_REMAIN_PERCENT = 15;

const TOKEN_PLACEHOLDERS = {
  get cursor() { return translate("account.userXxxEyjOrWorkoscursorsessiontoken"); },
  get codex() { return translate("accounts.tokensAccessTokenFromCodexAuthJson"); },
  get claude() { return translate("accounts.skAntOat01AccesstokenFromClaude"); },
};

let accounts = [];
const accountOrder = trackAccountOrder();
let rowOrder;
let intervalMinutes = 0;
let scheduledProviders = new Set();
let switching = false; // 切换账户流程进行中（全局互斥，期间禁用相关操作）
let switchingKind = null; // 后台刷新只暂停正在切换的供应商，其它队列继续运行。
let importing = false; // 本机导入 / 文件导入导出进行中（防重入，期间禁用相关按钮）
const groupRefreshing = new Set(); // 组内整体刷新进行中的组（"cursor" / "codex" / "claude"）
const refreshingIds = new Set(); // 本窗口发起、进行中的刷新
// 其它入口（托盘面板、定时刷新以外的窗口）发起的刷新：由后端 account-refreshing 事件同步，
// 只用于行内「刷新中」状态展示与防止重复刷新同一账户
const remoteRefreshingIds = new Set();
const rowErrors = new Map();

/** 该账户是否正在刷新（本窗口发起或其它窗口发起）。 */
function isRefreshing(id) {
  return refreshingIds.has(id) || remoteRefreshingIds.has(id);
}

let modalKind = "cursor";
let editingId = null;
let editingAccount = null;
let authEditor = null;
let authAdder = null;

function providerRefreshing(kind) {
  return scheduledProviders.has(kind) || groupRefreshing.has(kind)
    || accounts.some(account => account.kind === kind && isRefreshing(account.id));
}

function anyAccountRefreshing() {
  return scheduledProviders.size > 0 || groupRefreshing.size > 0 || refreshingIds.size > 0 || remoteRefreshingIds.size > 0;
}

/* ---------- 对外接口(供用量统计等模块联动) ---------- */

const changeListeners = new Set();
const sharedUsageListeners = new Set();
const automaticRefreshIds = new Set();

const refreshScheduler = createAccountRefreshScheduler({
  getOrder: displayOrderIds,
  isBlocked: () => importing,
  isProviderBlocked: kind => (switching && switchingKind === kind) || groupRefreshing.has(kind),
  isRefreshing,
  refreshAccount: async (id) => {
    automaticRefreshIds.add(id);
    try { await refreshOne(id); }
    finally { automaticRefreshIds.delete(id); }
  },
  refreshShared: async () => {
    const [sync] = await Promise.all([
      syncCodexUsage("interval"),
      ...[...sharedUsageListeners].map(listener => Promise.resolve().then(listener)),
    ]);
    if (sync.error) setStatus("warn", () => translate("accounts.gptUsageSyncFailed", syncError(sync.error)));
  },
  onRunningChange: (kinds) => {
    scheduledProviders = new Set(kinds);
    render();
  },
  onError: (error) => setStatus("warn", () => translate("accounts.scheduledRefreshFailed", resetError(error))),
});

function notifyChange() {
  const snapshot = accounts.slice();
  for (const fn of changeListeners) {
    try { fn(snapshot, { automaticIds: [...automaticRefreshIds] }); } catch { /* 监听方异常不影响本页 */ }
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

/** 共享本地日志随全局间隔更新，不随任意账户的独立间隔加速。 */
export function onSharedUsageRefresh(fn) {
  sharedUsageListeners.add(fn);
}

/* ---------- 格式化 ---------- */

function fmtNum(value) {
  const n = Number(value);
  if (value == null || !Number.isFinite(n)) return "—";
  return Number.isInteger(n) ? n.toLocaleString(locale()) : n.toFixed(2);
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

/** 所有账户的读取问题集中到状态列图标，保留完整原因而不增加行高。 */
function accountIssues(account) {
  const status = account.status || {};
  const issues = new Set();
  const denial = {
    expired: "accounts.theRefreshTokenHasExpired", reused: "trayPanel.theRefreshTokenWasAlreadyRotatedElsewhere",
    revoked: "trayPanel.theRefreshTokenWasRevoked", invalid: "accounts.theRefreshTokenIsInvalid",
  }[status.refreshErrorCode];
  const detail = status.refreshErrorDetail ?? status.refreshError?.match(/（([^（）]*)）$/)?.[1];
  const refreshError = denial ? translate(denial) + (detail ? " · " + detail : "") : status.refreshError;
  for (const error of [rowErrors.get(account.id), refreshError, status.error]) {
    if (error) issues.add(resetError(error));
  }
  if (account.kind === "opencode") {
    if (!account.opencodeAuthorized) issues.add(translate("accounts.authorizeThisWorkspaceInTheAccountEditor"));
    if (status.workspaceUsageAllowed === false) issues.add(translate("accounts.workspaceWideUsageRequiresAdministratorAccess"));
    for (const [key, label] of [["subscription", translate("accounts.plan")], ["quota", translate("accounts.quota")], ["balance", translate("accounts.balance")]]) {
      const field = status.fields?.[key];
      if (!field || ["ok", "not_subscribed"].includes(field.state)) continue;
      const reason = field.error ? resetError(field.error) : field.state === "forbidden" ? translate("accounts.accessDenied") : translate("accounts.unavailable");
      const cached = Number(field.at) > 0 ? translate("accounts.showingCachedDataFrom", fmtDateMs(Number(field.at))) : "";
      issues.add(`${label}：${reason}${cached}`);
    }
  }
  if (status.stale) issues.add(translate("accounts.someDataCouldNotBeRefreshedShowing"));
  return [...issues].join("\n");
}

function fillStateCell(cell, account) {
  const status = account.status || null;
  const alive = status && typeof status.alive === "boolean" ? status.alive : null;
  cell.classList.add("account-state-cell");

  const wrap = document.createElement("span");
  wrap.className = "account-state";
  const dot = document.createElement("span");
  dot.className = `dot ${alive === true ? "ok" : alive === false ? "bad" : "unknown"}`;
  const text = document.createElement("span");
  text.textContent = alive === true ? translate("accounts.valid") : alive === false ? translate("accounts.expired") : translate("accounts.unchecked");
  wrap.append(dot, text);
  cell.append(wrap);

  // 登录有效期与续期标签共用第二行，与套餐到期时间对齐。
  const line = document.createElement("div");
  line.className = "state-tag-line";
  const expMs = account.kind === "codex" ? Number(status?.exp) * 1000 : Number(status?.tokenExpiresAtMs);
  if (alive === true && Number.isFinite(expMs) && expMs > 0) {
    const info = remainInfo(expMs);
    if (!info.expired) {
      const expiry = document.createElement("span");
      expiry.className = "state-expiry";
      expiry.textContent = info.text;
      expiry.title = translate("accounts.tokenExpiresCredentialValiditySeparateFromThe", fmtDateMs(expMs));
      line.append(expiry);
    }
  }
  if (account.kind !== "cursor" && String(account.refreshToken || "").trim()) {
    const tag = document.createElement("span");
    tag.className = "tag renewable";
    tag.textContent = translate("accounts.renewable");
    line.append(tag);
  }
  if (line.childNodes.length) cell.append(line);

  const issues = accountIssues(account);
  if (issues) {
    const e = document.createElement("span");
    e.className = "account-issue";
    e.innerHTML = '<svg viewBox="0 0 16 16" fill="none" aria-hidden="true"><circle cx="8" cy="8" r="6.25" stroke="currentColor" stroke-width="1.5"/><path d="M8 4.5v4" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><circle cx="8" cy="11" r=".8" fill="currentColor"/></svg>';
    e.title = issues;
    e.setAttribute("role", "img");
    e.setAttribute("aria-label", issues);
    e.tabIndex = 0;
    cell.append(e);
  }
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
  const label = accountPlanInfo(account).label;
  const name = document.createElement("div");
  name.className = "plan-name";
  name.textContent = label || "—";
  name.title = name.textContent;
  cell.append(name);
  if (!status || status.alive === false) return;
  // Claude 的额度窗口重置时间见摘要列，接口不提供订阅起止日期
  if (account.kind === "claude") return;
  const end = accountDateInfo(account.kind === "codex" ? status.planActiveUntil : status.billingCycleEnd);
  if (!end) return;
  const expiry = expiryNode(end.ms);
  const startText = accountDateInfo(account.kind === "codex" ? status.planActiveStart : status.billingCycleStart)?.text;
  const renewHint = account.kind === "codex" || account.kind === "opencode"
    ? status.planWillRenew === true
      ? translate("accounts.renewsAutomatically")
      : status.planWillRenew === false
      ? translate("accounts.doesNotRenew")
      : ""
    : translate("accounts.renewsAndResetsQuota");
  expiry.title =
    account.kind === "codex"
      ? startText
        ? translate("accounts.subscriptionTo", startText, end.text, renewHint)
        : translate("accounts.subscribedUntil", end.text, renewHint)
      : startText
      ? translate("accounts.billingPeriodTo", startText, end.text, renewHint)
      : translate("accounts.billingPeriodEnds", end.text, renewHint);
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
  pct.title = translate("accounts.remaining", Math.round(remaining));
  wrap.append(name, track, pct);
  if (extra?.text) {
    const extraSpan = document.createElement("span");
    extraSpan.className = "quota-bar-extra";
    extraSpan.textContent = extra.text;
    extraSpan.title = extra.title ?? extra.text;
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
      const bar = usedPct != null ? quotaBar("Plan", usedPct, { text: detail }) : null;
      if (bar) bars.push(bar);
      else if (detail) meta.push(metaItem(translate("accounts.quota2", detail)));
    }
  }
  // Sand 周额度（Cursor 对 Grok Bot 额度的内部代号）：套餐包含时展示
  const sand = status.sand || null;
  if (sand && sand.included) {
    let usedPct = Number(sand.usagePercent);
    if (sand.hasAvailableUsage === false) usedPct = 100;
    const bar = quotaBar("Sand", usedPct, quotaResetInfo(sand.nextResetAt));
    if (bar) bars.push(bar);
  }
  return { bars, meta };
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
    limit.textContent = translate("accounts.limit", centsText(limitCents));
    cell.append(limit);
  }
}

/**
 * Credits 列（Codex，与 Cursor 超额列同位对齐）：主行美元余额，次行 credit 点数。
 * 无限额度主行显示「无限」；解析不出显示 —。
 */
function fillCreditsCell(cell, account) {
  if (account.kind === "opencode") {
    const status = account.status || {};
    cell.textContent = status.balanceUsd == null ? "—" : usdText(status.balanceUsd);
    return;
  }
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
  used.textContent = unlimited ? translate("accounts.unlimited") : usdText(usd);
  cell.append(used);
  if (units != null) {
    const extra = document.createElement("div");
    extra.className = "ondemand-limit";
    extra.textContent = translate("accounts.credits", units.toLocaleString(locale()));
    cell.append(extra);
  }
}

// 与 cursorSummaryNodes 结构对齐：bars 为额度窗口进度条（Token 到期倒计时见套餐列），
// meta 为额度条下方的小字：ChatGPT 的剩余额度重置次数（接口提供时才显示）
function codexSummaryNodes(status, anchorSec) {
  const bars = [];
  const meta = [];
  const windows = Array.isArray(status.windows) ? status.windows : [];
  for (const w of windows.slice(0, 3)) {
    if (!w) continue;
    const reset = quotaResetInfo(w.resetAt, w.resetsInSeconds, anchorSec);
    const bar = quotaBar(quotaWindowLabel(w, translate("accounts.quotaWindow")), w.usedPercent, reset);
    if (bar) bars.push(bar);
  }
  // 「剩余重置 2 次 · 最早 09-30 过期」：次数与最早过期时间用间隔点连成一条不换行的小字，
  // 最早一次已过期（数据陈旧）时整条标红提示刷新
  const resets = resetCreditsBrief(status);
  if (resets) {
    const text = resets.expiresText ? `${resets.text} · ${resets.expiresText}` : resets.text;
    const item = metaItem(text, resets.expired);
    item.title = resets.title;
    meta.push(item);
  }
  return { bars, meta };
}

function fillSummaryCell(cell, account) {
  const status = account.status || null;
  // Claude 的额度窗口结构与 Codex 一致（windows: label / usedPercent / resetAt），共用渲染
  const { bars, meta } = !status || status.alive === false
    ? { bars: [], meta: [] }
    : account.kind !== "cursor"
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

/* ---------- 账户表格行 ---------- */

// 操作列的图标按钮助手 iconAction 见 shared.js（用量页总览表同款）

/**
 * 「上次刷新」列：主行为账户状态刷新时间；ChatGPT 账户再加一行凭据组（access / refresh token）
 * 最后换新时间，精确时刻放悬停提示。定时重算相对时间时也走这里，整格重填。
 */
function fillTimeCell(cell, account) {
  cell.replaceChildren();
  const main = document.createElement("div");
  main.textContent = relativeFromUnixSeconds(account.lastRefreshAt);
  cell.append(main);
  const credAt = Number(account.codexAuthRefreshedAt);
  if (account.kind === "codex" && Number.isFinite(credAt) && credAt > 0) {
    const cred = document.createElement("div");
    cred.className = "cred-refreshed";
    cred.textContent = translate("accounts.credentials", relativeFromMs(credAt));
    cred.title = translate("accounts.accessAndRefreshTokensLastRenewed", fmtDateMs(credAt));
    cell.append(cred);
  }
}

function accountRow(account) {
  const tr = document.createElement("tr");
  tr.dataset.id = account.id;
  const busy = isRefreshing(account.id);

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
      (account.noteAuto === false ? account.note : "") || statusName || account.note || translate("accounts.unnamedAccount");
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
    note.textContent = account.note || translate("accounts.unnamedAccount");
    note.title = account.note || "";
    ident.append(note);
    token.textContent = maskToken(account.token);
  }
  const tag = document.createElement("span");
  tag.className = "tag kind-" + account.kind;
  tag.textContent = kindLabel(account.kind);
  const nameLine = document.createElement("div");
  nameLine.className = "account-name-line";
  ident.prepend(nameLine);
  nameLine.append(tag, note);
  accountCell.append(ident, token);
  const grip = document.createElement("td");
  grip.className = "account-grip";
  grip.append(orderHandle(note.textContent));

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
  fillTimeCell(timeCell, account);

  const actionsCell = document.createElement("td");
  actionsCell.className = "account-actions";
  const refreshBtn = iconAction("refresh", busy ? translate("accounts.refreshing") : translate("usage.refresh"));
  refreshBtn.classList.toggle("busy", busy);
  refreshBtn.disabled = busy || providerRefreshing(account.kind) || switching || importing;
  refreshBtn.addEventListener("click", () => { void refreshOne(account.id); });
  const editBtn = iconAction("edit", translate("accounts.edit"));
  editBtn.disabled = busy || switching;
  editBtn.addEventListener("click", () => account.kind === "opencode"
    ? editOpenCode(account, async () => { await loadInitial(false); void refreshOne(account.id); }) : openModal(account));
  // 删除使用显式确认弹窗，避免首次点击只变色而看起来没有响应。
  const deleteBtn = iconAction("delete", translate("accounts.delete"));
  deleteBtn.disabled = busy || switching;
  deleteBtn.addEventListener("click", () => { void onDeleteClick(account.id); });
  if (account.kind === "opencode") {
    const switchBtn = iconAction("switch", translate("accounts.switchOpencodeClients"));
    switchBtn.disabled = busy || switching;
    switchBtn.addEventListener("click", () => { void switchOpenCode(account.id); });
    actionsCell.append(switchBtn, refreshBtn, editBtn, deleteBtn);
  } else if (account.kind === "cursor") {
    // 仅允许切换已验证有效的账户，避免把失效 token 写进本地 Cursor
    const aliveOk = !!(account.status && account.status.alive === true);
    const switchBtn = iconAction("switch", aliveOk ? translate("accounts.switchToThisAccount") : translate("accounts.refreshToVerifyThisAccountBeforeSwitching"));
    switchBtn.disabled = busy || switching || !aliveOk;
    switchBtn.addEventListener("click", () => { void onSwitchAccount(account.id); });
    actionsCell.append(switchBtn, refreshBtn, editBtn, deleteBtn);
  } else {
    // Claude Desktop 使用独立登录快照，CLI 凭据条件交给后端逐端报告。
    const isClaude = account.kind === "claude";
    const aliveOk = !!(account.status && account.status.alive === true);
    const hasRt = !!String(account.refreshToken || "").trim();
    const switchBtn = iconAction(
      "switch",
      isClaude ? translate("accounts.switchLocalClaudeCliDesktopAccount") : !aliveOk
        ? translate("accounts.refreshToVerifyThisAccountBeforeSwitching")
        : !hasRt
        ? translate("accounts.aRefreshTokenIsRequiredToSwitch")
        : translate("accounts.switchLocalGptCodexAccount")
    );
    switchBtn.disabled = busy || switching || (!isClaude && (!aliveOk || !hasRt));
    switchBtn.addEventListener("click", () => {
      void (isClaude ? onSwitchClaudeAccount(account.id) : onSwitchCodexAccount(account.id));
    });
    actionsCell.append(switchBtn, refreshBtn, editBtn, deleteBtn);
  }

  const exportButton = iconAction("export", translate("dialog.exportAccount"));
  exportButton.classList.add("account-export");
  exportButton.disabled = importing || switching;
  exportButton.addEventListener("click", () => { void onExport(account.id); });
  actionsCell.append(exportButton);

  tr.append(grip, accountCell, stateCell, planCell, summaryCell, extraCell, timeCell, actionsCell);
  return tr;
}

function updateAccountCount() {
  const latest = accounts.reduce((time, account) => Math.max(time, Number(account.lastRefreshAt) || 0), 0);
  el("#accounts-count").textContent = accounts.length
    ? translate("accounts.count", fmtInt(accounts.length)) + (latest ? translate("accounts.lastRefresh") + relativeFromMs(latest * 1000) : "") : "";
}

function updateHeadingActions() {
  const blocked = switching || importing;
  const refresh = el("#accounts-refresh");
  refresh.disabled = blocked || anyAccountRefreshing() || !accounts.length;
  refresh.classList.toggle("busy", anyAccountRefreshing());
  el("#accounts-import").disabled = blocked || anyAccountRefreshing();
  el("#accounts-export").disabled = blocked || !accounts.length;
  el("#accounts-logout").disabled = blocked;
  el("#accounts-add").disabled = blocked;
}

function render() {
  if (!rowOrder || rowOrder.canRender()) {
    el("#accounts-body").replaceChildren(...accountOrder.rows(accounts).map(accountRow));
  }
  el("#accounts-table").hidden = !accounts.length;
  el("#accounts-empty").hidden = !!accounts.length;
  updateAccountCount();
  updateHeadingActions();
  if (!quotaIntroPlayed && document.querySelector(".quota-bar")) quotaIntroPlayed = true;
}

function updateRow(id) {
  if (rowOrder && !rowOrder.canRender()) return;
  const account = accounts.find(item => item.id === id);
  const old = el("#accounts-body").querySelector('tr[data-id="' + CSS.escape(id) + '"]');
  if (!old || !account) { render(); return; }
  old.replaceWith(accountRow(account));
  if (!quotaIntroPlayed && document.querySelector(".quota-bar")) quotaIntroPlayed = true;
  updateAccountCount();
  updateHeadingActions();
}

function applyView(view) {
  accountOrder.apply(view);
  accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
  syncCursorUsageFilters(accounts);
  const n = Number(view && view.intervalMinutes);
  if (Number.isFinite(n)) intervalMinutes = n;
  el("#accounts-interval").value = String(intervalMinutes);
  // 账户独立设置、增删和全量备份恢复都立即更新调度；未变化的任务保留原到期时间。
  rebuildTimer();
  // 清理已被移除账户的瞬态状态
  const ids = new Set(accounts.map((a) => a.id));
  for (const id of [...rowErrors.keys()]) {
    if (!ids.has(id)) rowErrors.delete(id);
  }
  for (const id of [...remoteRefreshingIds]) {
    if (!ids.has(id)) remoteRefreshingIds.delete(id);
  }
  notifyChange();
}

/* ---------- 后端数据变更同步 ---------- */

// 任何窗口触发的账户写入（托盘刷新 / 切换、Codex 自动续期等）后端都会广播
// accounts-changed 事件；密集写入（如全部刷新）用防抖合并，只应用最后一份视图。
let syncTimer = null;
let syncPendingView = null;

/**
 * 广播视图是否与本地状态完全一致。主窗口自己发起的写入（刷新 / 增删改）也会收到回声，
 * 而本地早已用命令返回值更新过；一致时跳过，避免同一次变化把联动模块（用量页）通知两遍。
 */
function sameAsCurrent(view) {
  const n = Number(view.intervalMinutes);
  if (Number.isFinite(n) && n !== intervalMinutes) return false;
  const list = Array.isArray(view.accounts) ? view.accounts : [];
  return JSON.stringify(list) === JSON.stringify(accounts);
}

function onBackendChanged(view) {
  if (!view) return;
  syncPendingView = view;
  if (syncTimer != null) return;
  syncTimer = setTimeout(() => {
    syncTimer = null;
    const v = syncPendingView;
    syncPendingView = null;
    if (sameAsCurrent(v)) {
      if (accountOrder.apply(v)) render();
      return;
    }
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
  // 本窗口或其它窗口正在刷新该账户时不重复排队（后端也会串行执行，重复只是白等一次）
  if (isRefreshing(id)) return null;
  if (!accounts.some((a) => a.id === id)) return null;
  refreshingIds.add(id);
  rowErrors.delete(id);
  updateRow(id);
  updateHeadingActions();
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
    rowErrors.set(id, errorCode(error));
    return null;
  } finally {
    refreshingIds.delete(id);
    refreshScheduler.completed(id);
    updateRow(id);
    updateHeadingActions();
    notifyChange();
  }
}

/** 供应商之间并行、组内顺序刷新，返回失败个数。后端负责跨窗口排队。 */
async function refreshIds(ids) {
  const wanted = new Set(ids);
  const ordered = displayOrderIds().filter(id => wanted.has(id));
  await refreshByProvider(ordered.map(id => accounts.find(account => account.id === id)), refreshOne);
  return ids.filter((id) => rowErrors.has(id)).length;
}

/**
 * 供其他模块（用量统计页「刷新」）触发的账户状态刷新：供应商并行，组内排队。
 * 重入安全：切换 / 导入流程中直接跳过，正在刷新中的账户由 refreshOne 自行去重。
 */
export async function refreshAccounts(ids) {
  if (switching || importing) return;
  // 同供应商内按界面显示顺序刷新，混合类型的一批（如备份导入）可以并行。
  const wanted = new Set(ids || []);
  const ordered = displayOrderIds().filter((id) => wanted.has(id));
  if (ordered.length) await refreshIds(ordered);
}

/** 跨供应商并行刷新，各供应商队列内按显示顺序执行。 */
async function refreshAllAccounts() {
  if (switching || importing || anyAccountRefreshing()) return;
  const rows = accountOrder.rows(accounts);
  if (!rows.length) return;
  const kinds = new Set(rows.map(a => a.kind));
  for (const item of kinds) groupRefreshing.add(item);
  clearStatus();
  render();
  try {
    const failed = await refreshIds(rows.map(a => a.id));
    if (failed) setStatus("warn", () => translate("accounts.refreshFinishedAccountsFailedSeeTheCorresponding", failed));
  } finally {
    for (const item of kinds) groupRefreshing.delete(item);
    render();
  }
}

function displayOrderIds() {
  return accountOrder.rows(accounts).map(a => a.id);
}

// 只有主窗口创建调度器；到期账户按供应商分队列，共享日志与 GPT 同步单独使用全局间隔。
function rebuildTimer() {
  refreshScheduler.update(accounts, intervalMinutes);
}

/** 全局默认间隔（分钟，0 = 关闭），用于继承的账户及共享用量任务；设置弹窗用于回显。 */
export function getRefreshIntervalMinutes() {
  return intervalMinutes;
}

/**
 * 保存定时刷新间隔并重建定时器，返回后端确认后的值；失败向上抛（由设置弹窗提示）。
 * applyView 内的 notifyChange 会通知用量统计页同步缓存有效期（与间隔一致）。
 */
export async function setRefreshInterval(minutes) {
  const view = await invoke("accounts_set_interval", { intervalMinutes: minutes });
  applyView(view);
  render();
  return intervalMinutes;
}

/* ---------- 删除（显式确认） ---------- */

/**
 * 删除账户。Cursor 账户的确认弹窗带「保留统计数据」勾选项（默认勾选）：保留时后端先做一次
 * 最终同步再把用量事件库标记为已删除留在本机，用量统计页总览继续计入并标「已删除」。
 * 同步失败且本机没有该账户任何用量数据时后端不删除并报 no_usage_data，这里再问一次
 * 是否放弃保留仍然删除。
 */
async function onDeleteClick(id) {
  if (switching || refreshingIds.has(id)) return;
  const account = accounts.find((item) => item.id === id);
  if (!account) return;
  const name = String(account.note || (account.status && account.status.name) || translate("accounts.unnamedAccount")).trim();
  const isCursor = account.kind === "cursor";
  const ok = await switchModal.toConfirm({
    title: translate("audit.accountDelete"),
    body: translate("accounts.deleteThisActionIsPermanent", name) + (account.kind === "opencode" ? translate("accounts.theLocalUsageArchiveForThisWorkspace") : ""),
    confirmText: translate("accounts.delete"),
    danger: true,
    option: isCursor ? { label: translate("accounts.keepUsageStatisticsInTheUsageTab"), checked: true } : null,
  });
  if (!ok) {
    switchModal.close();
    return;
  }
  const keepUsage = isCursor && switchModal.optionChecked();
  if (!keepUsage) {
    switchModal.close();
    await doDelete(id, false);
    return;
  }
  // 最终同步可能要联网拉取整份历史，期间弹窗保持忙碌态不可关闭
  switchModal.openBusy(translate("audit.accountDelete"), translate("accounts.syncingUsageForBeforeDeletion", name));
  let result;
  try {
    result = await invoke("accounts_delete", { id, keepUsage: true });
  } catch (error) {
    const code = errorCode(error);
    if (!code.startsWith("no_usage_data")) {
      switchModal.close();
      setStatus("bad", () => translate("accounts.deletionFailed", mapDeleteError(code)));
      render();
      return;
    }
    const reason = code.slice("no_usage_data".length).replace(/^:/, "");
    const proceed = await switchModal.toConfirm({
      title: translate("audit.accountDelete"),
      body: translate("accounts.usageForCouldNotBeFetchedThere", name, reason ? `（${mapDeleteError(reason)}）` : ""),
      confirmText: translate("accounts.deleteAnyway"),
      danger: true,
    });
    switchModal.close();
    if (proceed) await doDelete(id, false);
    return;
  }
  switchModal.close();
  applyView(result.view);
  setStatus(
    "ok",
    () => result.usage === "kept"
      ? translate("accounts.accountDeletedStatisticsAreAvailableInThe")
      : translate("accounts.accountDeletedItHadNoUsageRecords")
  );
  render();
}

async function doDelete(id, keepUsage) {
  try {
    const result = await invoke("accounts_delete", { id, keepUsage });
    applyView(result.view);
    setStatus("ok", () => translate("accounts.accountDeleted"));
  } catch (error) {
    const code = errorCode(error);
    if (code === "opencode_archive_cleanup_failed_account_deleted") setStatus("warn", mapDeleteError(code));
    else setStatus("bad", () => translate("accounts.deletionFailed", mapDeleteError(code)));
  }
  render();
}

/** 删除 / 最终同步阶段的错误码 -> 可读文案，未知错误码原样显示。 */
function mapDeleteError(code) {
  const M = {
    invalid_session_token: translate("accounts.theTokenHasExpiredUsageCannotBe"),
    empty_token: translate("accounts.accountTokenIsEmpty"),
    not_found: translate("accounts.accountNotFound"),
    opencode_archive_cleanup_failed_account_deleted: translate("accounts.accountDeletedTheOpencodeArchiveCouldNot"),
  };
  if (M[code]) return M[code];
  if (code.startsWith("cursor_http_")) return translate("accounts.cursorReturnedHttp", code.slice("cursor_http_".length));
  return resetError(code);
}

/**
 * 供其它页面复用的危险操作确认弹窗（如用量统计页删除已删除账户保留的统计数据）。
 * 弹窗正被切换 / 删除流程占用时直接返回 false。
 */
export async function confirmDialog({ title, body, confirmText, danger = true }) {
  if (switchModal.phase !== "hidden") return false;
  const ok = await switchModal.toConfirm({ title, body, confirmText, danger });
  switchModal.close();
  return ok;
}

/**
 * 供其它页面复用的单行文本输入弹窗（如用量统计页修改已删除账户的备注）。
 * input = { label, value, placeholder }。确认返回输入的文本（可为空串）；
 * 取消 / 关闭，或弹窗正被切换 / 删除流程占用时返回 null；2 秒内再次按 Esc 可关闭。
 */
export async function promptDialog({ title, body, confirmText, input }) {
  if (switchModal.phase !== "hidden") return null;
  const ok = await switchModal.toConfirm({ title, body, confirmText, danger: false, input });
  const value = ok ? switchModal.inputValue() : null;
  switchModal.close();
  return value;
}

/** 已删除账户只允许关闭并移除 API 历史；保存后不能再开启，备注仍可编辑。 */
export async function editDeletedAccountDialog({ label, note, ignoreApiModels, apiRemovalApproximate, apiRemovalError, save }) {
  if (switchModal.phase !== "hidden") return null;
  const readValue = () => ({
    note: switchModal.inputValue(),
    removeApiUsage: !ignoreApiModels && !apiRemovalError && !el("#switch-modal-api-input").checked,
  });
  const ok = await switchModal.toConfirm({
    title: translate("audit.usageDataEdit"),
    body: translate("accounts.editTheNoteAndRetainedHistoryFor", label),
    confirmText: translate("common.save"),
    danger: false,
    onConfirm: save ? () => save(readValue()) : null,
    input: { label: translate("account.note"), value: note, placeholder: translate("accounts.leaveBlankToUseTheAutomaticNote") },
    apiHistory: {
      checked: !ignoreApiModels,
      disabled: !!ignoreApiModels || !!apiRemovalError,
      warningTitle: ignoreApiModels ? translate("accounts.warningApiHistoryWasPermanentlyRemoved") : translate("accounts.warningClearingHistoryIsPermanent"),
      description: ignoreApiModels
        ? apiRemovalApproximate
          ? translate("accounts.apiHistoryWasRemovedBasedOnModel")
          : translate("accounts.apiQuotaAndPayAsYouGo")
        : (apiRemovalError && resetError(apiRemovalError)) || (apiRemovalApproximate
          ? translate("accounts.turningThisOffAndSavingPermanentlyRemoves")
          : translate("accounts.turningThisOffAndSavingPermanentlyRemovesIncluded")),
    },
  });
  const result = ok ? readValue() : null;
  switchModal.close();
  return result;
}

/**
 * 供其它页面复用的下拉选择弹窗（如用量统计页为已删除账户指定套餐档位）。
 * select = { label, value, options: [{ value, label }] }。确认返回所选项的 value；
 * 取消 / 关闭，或弹窗正被切换 / 删除流程占用时返回 null；2 秒内再次按 Esc 可关闭。
 */
export async function selectDialog({ title, body, confirmText, select, save }) {
  if (switchModal.phase !== "hidden") return null;
  const ok = await switchModal.toConfirm({ title, body, confirmText, danger: false, select,
    onConfirm: save ? () => save(switchModal.selectValue()) : null });
  const value = ok ? switchModal.selectValue() : null;
  switchModal.close();
  return value;
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
  confirmAction: null,
  cancelAutoClose: null,
  altHandler: null, // 结果阶段备选操作按钮的回调（finish 的 alt 参数）
  editable: false,
  controller: null,

  show() {
    this.controller ||= createModal(el("#switch-modal"), {
      mode: () => this.editable ? "edit" : "view",
      isBusy: () => this.phase === "busy" || this.phase === "steps",
      closeSelector: "#switch-cancel, #switch-close",
      onRequestClose: () => this.requestClose(),
      onClose: () => this.resetState(),
    });
    this.controller.reset();
    this.controller.open();
  },

  /** 打开弹窗并显示忙碌文案（如「正在检测本地 Cursor…」），期间不可关闭。 */
  openBusy(title, text) {
    this.stopAutoClose();
    el("#switch-modal-title").textContent = title;
    const body = el("#switch-modal-body");
    body.hidden = false;
    body.textContent = text;
    this.setOption(null);
    this.setInput(null);
    this.setSelect(null);
    this.setApiHistory(null);
    const steps = el("#switch-steps");
    steps.hidden = true;
    steps.replaceChildren();
    const status = el("#switch-modal-status");
    status.hidden = true;
    status.textContent = "";
    el("#switch-modal .modal-foot").hidden = true;
    this.phase = "busy";
    this.editable = false;
    this.stepIndex = -1;
    this.show();
  },

  /**
   * 切到确认阶段：显示正文与取消 / 确认按钮，返回用户选择（Esc / 关闭 / 取消 = false）。
   * option = { label, checked } 时在正文下方显示一个勾选项（如删除 Cursor 账户的「保留统计数据」），
   * 确认后由调用方经 optionChecked() 读取；input = { label, value, placeholder } 时显示一个
   * 单行文本输入（如修改已删除账户的备注），焦点落在输入框、回车即确认，确认后经 inputValue() 读取；
   * select = { label, value, options } 时显示一个下拉选择（如指定已删除账户的套餐档位），
   * 焦点落在下拉框，确认后经 selectValue() 读取。
   */
  toConfirm({ title, body, confirmText, danger, option, input, select, apiHistory, onConfirm }) {
    return new Promise((resolve) => {
      this.stopAutoClose();
      if (title) el("#switch-modal-title").textContent = title;
      const text = el("#switch-modal-body");
      text.hidden = false;
      text.textContent = body || "";
      this.setOption(option || null);
      this.setInput(input || null);
      this.setSelect(select || null);
      this.setApiHistory(apiHistory || null);
      el("#switch-steps").hidden = true;
      el("#switch-modal-status").hidden = true;
      el("#switch-modal .modal-foot").hidden = false;
      el("#switch-cancel").hidden = false;
      const ok = el("#switch-ok");
      ok.hidden = false;
      ok.textContent = confirmText || translate("common.confirm");
      ok.classList.toggle("danger", !!danger);
      this.phase = "confirm";
      this.editable = !!(input || select || apiHistory);
      this.confirmResolve = resolve;
      this.confirmAction = onConfirm || null;
      this.show();
      if (input) {
        const field = el("#switch-modal-input");
        field.focus();
        field.select();
      } else if (select) {
        el("#switch-modal-select").focus();
      } else {
        ok.focus();
      }
    });
  },

  /** 显示 / 隐藏确认阶段的下拉选择，并按 options 重建选项、选中 value（没有匹配项时选第一项）。 */
  setSelect(select) {
    const box = el("#switch-modal-select-field");
    const field = el("#switch-modal-select");
    if (!select) {
      box.hidden = true;
      field.replaceChildren();
      return;
    }
    el("#switch-modal-select-label").textContent = select.label || "";
    field.replaceChildren(
      ...(select.options || []).map(({ value, label }) => {
        const opt = document.createElement("option");
        opt.value = String(value ?? "");
        opt.textContent = label;
        return opt;
      })
    );
    field.value = String(select.value ?? "");
    if (field.selectedIndex < 0 && field.options.length) field.selectedIndex = 0;
    box.hidden = false;
  },

  /** 确认阶段下拉选择的当前值（未显示下拉时为空串）。 */
  selectValue() {
    return el("#switch-modal-select-field").hidden ? "" : el("#switch-modal-select").value;
  },

  /** 显示 / 隐藏确认阶段的单行文本输入。 */
  setInput(input) {
    const box = el("#switch-modal-input-field");
    const field = el("#switch-modal-input");
    if (!input) {
      box.hidden = true;
      field.value = "";
      field.placeholder = "";
      return;
    }
    el("#switch-modal-input-label").textContent = input.label || "";
    field.placeholder = input.placeholder || "";
    field.value = input.value || "";
    box.hidden = false;
  },

  /** 确认阶段文本输入的当前值（未显示输入框时为空串）。 */
  inputValue() {
    return el("#switch-modal-input-field").hidden ? "" : el("#switch-modal-input").value;
  },

  /** 显示 / 隐藏确认阶段的勾选项。 */
  setOption(option) {
    const box = el("#switch-modal-option");
    const input = el("#switch-modal-option-input");
    if (!option) {
      box.hidden = true;
      input.checked = false;
      return;
    }
    el("#switch-modal-option-text").textContent = option.label;
    input.checked = !!option.checked;
    box.hidden = false;
  },

  /** 确认阶段勾选项的当前状态（未显示勾选项时为 false）。 */
  optionChecked() {
    return !el("#switch-modal-option").hidden && el("#switch-modal-option-input").checked;
  },

  setApiHistory(option) {
    const box = el("#switch-modal-api-history");
    const input = el("#switch-modal-api-input");
    box.hidden = !option;
    el("#switch-modal").classList.toggle("has-api-history", !!option);
    input.checked = !!option?.checked;
    input.disabled = !!option?.disabled;
    el("#switch-modal-api-warning-title").textContent = option?.warningTitle || "";
    el("#switch-modal-api-help").textContent = option?.description || "";
  },

  /** 渲染步骤列表（全部待办）并进入执行阶段，期间不可关闭。 */
  toSteps(labels) {
    el("#switch-modal-body").hidden = true;
    this.setOption(null);
    this.setInput(null);
    this.setSelect(null);
    this.setApiHistory(null);
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
    this.editable = false;
    this.controller?.reset();
    this.stepIndex = -1;
  },

  /** 下一步进入执行中。 */
  stepStart() {
    this.stepIndex += 1;
    const step = el("#switch-steps").children[this.stepIndex];
    if (step) step.className = "switch-step running";
  },

  /**
   * 结算当前执行中的步骤（无执行中步骤时安全空操作，便于 catch 里无脑调用）。
   * detail 为可选的补充说明，追加在步骤文案之后（如「写入登录凭证 · 已换取新凭据」）。
   */
  settleStep(state, detail) {
    const step = el("#switch-steps").children[this.stepIndex];
    if (!step || !step.classList.contains("running")) return;
    step.className = `switch-step ${state}`;
    if (detail) {
      const text = step.lastElementChild;
      if (text) text.textContent = `${text.textContent} · ${detail}`;
    }
  },
  stepDone(detail) {
    this.settleStep("done", detail);
  },
  stepFail() {
    this.settleStep("fail");
  },

  /**
   * 展示最终结果，底部变单个「关闭」按钮，此后允许 Esc / 关闭。
   * 成功结果几秒后自动关闭（按钮上倒计时）；失败结果保留，等用户看完手动关闭。
   * alt = { label, onClick } 时在「关闭」左侧多一个备选操作按钮（如切换失败后的「强制写入并启动」）。
   */
  finish(ok, message, alt = null) {
    el("#switch-modal-body").hidden = true;
    this.setOption(null);
    this.setInput(null);
    this.setSelect(null);
    this.setApiHistory(null);
    const status = el("#switch-modal-status");
    status.hidden = false;
    status.className = `status ${ok ? "ok" : "bad"}`;
    status.textContent = message;
    el("#switch-modal .modal-foot").hidden = false;
    el("#switch-cancel").hidden = true;
    const altBtn = el("#switch-alt");
    altBtn.hidden = !alt;
    altBtn.textContent = alt ? alt.label : "";
    this.altHandler = alt ? alt.onClick : null;
    const btn = el("#switch-ok");
    btn.hidden = false;
    btn.textContent = translate("common.close");
    btn.classList.remove("danger");
    this.phase = "done";
    this.editable = false;
    this.controller?.reset();
    btn.focus();
    this.stopAutoClose();
    if (ok) this.cancelAutoClose = startSwitchResultCountdown(btn, () => this.close());
  },

  /** 备选操作按钮：只在结果阶段有效，点击后由调用方接管弹窗（通常重新进入步骤阶段）。 */
  onAlt() {
    if (this.phase !== "done" || typeof this.altHandler !== "function") return;
    const handler = this.altHandler;
    this.altHandler = null;
    el("#switch-alt").hidden = true;
    handler();
  },

  stopAutoClose() {
    this.cancelAutoClose?.();
    this.cancelAutoClose = null;
  },

  /** 隐藏弹窗并复位。 */
  close() {
    this.controller?.close();
  },

  resetState() {
    this.stopAutoClose();
    if (this.confirmResolve) {
      const resolve = this.confirmResolve;
      this.confirmResolve = null;
      resolve(false);
    }
    this.confirmAction = null;
    el("#switch-modal-body").hidden = false;
    this.setOption(null);
    this.setInput(null);
    this.setSelect(null);
    this.setApiHistory(null);
    el("#switch-steps").hidden = true;
    el("#switch-modal-status").hidden = true;
    el("#switch-alt").hidden = true;
    this.altHandler = null;
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
  async onOk() {
    if (this.phase === "confirm") {
      if (!this.confirmResolve) return;
      if (this.confirmAction) {
        const operation = this.controller.beginSave([el("#switch-ok")]);
        if (!operation) return;
        try {
          await this.confirmAction();
          if (!operation.current()) return;
        } catch (error) {
          if (operation.current()) fillStatus(el("#switch-modal-status"), "bad", () => translate("accounts.saveFailed", resetError(error)));
          return;
        } finally {
          operation.finish();
        }
      }
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
  const code = errorCode(err);
  // 写登录文件失败的错误码带冒号细节（codex_auth_write_failed:…），按前缀匹配
  if (code.startsWith("codex_auth_write_failed")) {
    return translate("accounts.couldNotWriteTheLocalChatgptLogin");
  }
  if (code.startsWith("claude_creds_write_failed")) {
    return translate("accounts.couldNotWriteLocalClaudeCodeCredentials");
  }
  // 退出登录前读不到本机登录文件（权限 / 占用），同样带冒号细节
  if (code.startsWith("codex_auth_read_failed")) {
    return translate("accounts.couldNotReadTheLocalChatgptLogin");
  }
  if (code.startsWith("claude_creds_read_failed")) {
    return translate("accounts.couldNotReadLocalClaudeCodeCredentials");
  }
  // ChatGPT 换票被拒：后缀为服务端错误码的归类（expired / reused / revoked / invalid）
  if (code.startsWith("codex_refresh_denied")) {
    const reason = code.split(":")[1] || "invalid";
    const why = {
      expired: translate("accounts.theRefreshTokenHasExpired"),
      reused: translate("accounts.theRefreshTokenWasAlreadyRotatedBy"),
      revoked: translate("accounts.theRefreshTokenWasRevokedBySign"),
      invalid: translate("accounts.theRefreshTokenIsInvalid"),
    }[reason] || translate("accounts.theRefreshTokenIsInvalid");
    return translate("accounts.editTheAccountToUpdateItsCredentials", why);
  }
  const M = {
    account_not_verified: translate("accounts.refreshTheAccountToVerifyItBefore"),
    cursor_running: translate("accounts.cursorIsStillRunningCloseItAnd"),
    cursor_exe_not_found: translate("accounts.cursorExecutableNotFoundSetItsPath"),
    cursor_exe_invalid: translate("accounts.theConfiguredCursorPathIsInvalidOr"),
    state_db_not_found: translate("accounts.cursorSStateVscdbWasNotFound"),
    unsupported_platform: translate("accounts.thisOperationIsUnavailableOnThisOperating"),
    not_cursor_account: translate("accounts.thisIsNotACursorAccount"),
    not_codex_account: translate("accounts.thisIsNotAChatgptAccount"),
    not_claude_account: translate("accounts.thisIsNotAClaudeAccount"),
    empty_token: translate("accounts.theAccountTokenIsEmpty"),
    invalid_session_token: translate("accounts.theUserTokenHasExpiredUpdateIt"),
    poll_timeout: translate("accounts.couldNotObtainLoginCredentialsTryAgain"),
    session_exchange_failed: translate("accounts.couldNotObtainLoginCredentialsCheckThe"),
    codex_running: translate("accounts.chatgptIsStillRunningCloseItAnd"),
    codex_exe_not_found: translate("accounts.chatgptExecutableNotFoundSetItsPath"),
    codex_exe_invalid: translate("accounts.theConfiguredChatgptPathIsInvalidOr"),
    codex_no_refresh_token: translate("accounts.thisAccountHasNoRefreshTokenFor"),
    codex_id_token_missing: translate("accounts.couldNotObtainTheIdTokenRequired"),
    codex_auth_missing: translate("accounts.noSavedAuthJsonIsAvailableRefresh"),
    codex_auth_incomplete: translate("accounts.theSavedAuthJsonIsMissingIts"),
    claude_running: translate("accounts.claudeDesktopIsStillRunningCloseIt"),
    claude_exe_not_found: translate("accounts.claudeDesktopWasNotFoundSetIts"),
    claude_exe_invalid: translate("accounts.theConfiguredClaudeDesktopPathIsInvalid"),
    claude_no_refresh_token: translate("accounts.thisAccountHasNoRefreshTokenFor"),
    claude_refresh_denied: translate("accounts.theRefreshTokenIsInvalidUpdateIt"),
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
  switchingKind = "cursor";
  render();
  switchModal.openBusy(translate("accounts.switchLocalCursorLogin"), translate("accounts.detectingLocalCursorInstallation"));
  try {
    const st = await invoke("cursor_client_status");
    if (!st.exeConfigured) {
      switchModal.finish(false, translate("accounts.setTheCursorExecutablePathInSettings"));
      return;
    }
    const running = !!st.running;
    const ok = await switchModal.toConfirm(
      running
        ? {
            body: translate("accounts.cursorIsRunningSwitchingAccountsWillClose"),
            confirmText: translate("accounts.closeAndSwitch"),
            danger: true,
          }
        : {
            body: translate("accounts.useThisAccountForTheLocalLogin"),
            confirmText: translate("tray.switch"),
          }
    );
    if (!ok) {
      switchModal.close();
      return;
    }
    switchModal.toSteps(running ? [translate("accounts.closeCursor"), translate("accounts.obtainAndSaveLoginCredentials"), translate("accounts.startCursor")] : [translate("accounts.obtainAndSaveLoginCredentials"), translate("accounts.startCursor")]);
    if (running) {
      switchModal.stepStart();
      const c = await invoke("cursor_client_close");
      if (!c.closed) {
        switchModal.stepFail();
        switchModal.finish(false, translate("accounts.cursorDidNotFullyCloseCloseIt"));
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
    switchModal.finish(true, l.launched ? translate("accounts.accountSwitchedAndCursorStarted") : translate("accounts.accountSwitchedStartCursorManually"));
  } catch (error) {
    switchModal.stepFail();
    switchModal.finish(false, translate("accounts.switchFailed", mapSwitchError(error)));
  } finally {
    switching = false;
    render();
  }
}

/** 切换结果里的凭据来源文案：换了新凭据 / 直接写入保存的副本；后端未标注时为空。 */
function codexSwitchModeText(result) {
  if (!result || typeof result.exchanged !== "boolean") return "";
  return result.exchanged ? translate("accounts.newCredentialsReceivedAccessAndRefreshTokens") : translate("accounts.usingTheSavedCredentialsWithoutRenewal");
}

// ChatGPT 切换：检测客户端 ->（必要时确认并关闭）-> 写入 auth.json（副本可用直接写，否则先换票）-> 启动，
// 确认、分步进度与结果全程在切换弹窗内展示。
async function onSwitchLinkedAccount(id) {
  if (switching) return;
  const account = accounts.find(a => a.id === id);
  if (!account) return;
  switching = true; switchingKind = account.kind; render();
  try { await switchLocalClients(account, async () => {
    const view = await loadAccountList(); applyView(view); render();
  }); } finally { switching = false; render(); }
}
async function onSwitchCodexAccount(id) { return onSwitchLinkedAccount(id); }
async function onSwitchClaudeAccount(id) { return onSwitchLinkedAccount(id); }

/* ---------- 退出本机登录（清除本机 Cursor / ChatGPT / Claude Code 登录态） ---------- */

// 各类型在退出登录流程里的差异点：客户端名（检测 / 关闭对象）、登录名（清除对象）、后端命令与说明文案
const LOGOUT_TARGETS = {
  cursor: {
    client: "Cursor",
    login: "Cursor",
    status: "cursor_client_status",
    close: "cursor_client_close",
    logout: "cursor_logout_local",
    get where() { return translate("accounts.theLoginInTheLocalCursorAuthentication"); },
  },
  codex: {
    client: "ChatGPT",
    login: "ChatGPT",
    status: "codex_client_status",
    close: "codex_client_close",
    logout: "codex_logout_local",
    get where() { return translate("accounts.accountCredentialsInLocalAuthJsonConfigured"); },
  },
  claude: {
    client: "Claude Desktop",
    login: "Claude Code",
    status: "claude_client_status",
    close: "claude_client_close",
    logout: "claude_logout_local",
    get where() { return translate("accounts.theAccountLoginInLocalClaudeCode"); },
  },
};

/**
 * 退出本机登录：检测客户端 →（运行中先确认并关闭）→ 清除本机登录态 → 结果。
 * 与切换共用同一弹窗与互斥标志（switching）；只动本机客户端的登录态，托管账户不受影响；
 * 完成后不再启动客户端（启动即进入登录页，是否启动由用户自行决定）。
 */
async function onLogoutLocal(kind) {
  if (switching || importing || providerRefreshing(kind)) return;
  const t = LOGOUT_TARGETS[kind];
  if (!t) return;
  switching = true;
  switchingKind = kind;
  render();
  switchModal.openBusy(translate("accounts.signOutOfLocal", t.login), translate("accounts.detectingLocal", t.client));
  try {
    const st = await invoke(t.status);
    const running = !!st.running;
    const ok = await switchModal.toConfirm(
      running
        ? {
            body: translate("accounts.isRunningSigningOutWillCloseIt", t.client, t.where),
            confirmText: translate("accounts.closeAndSignOut"),
            danger: true,
          }
        : {
            body: translate("accounts.clearYouWillNeedToSignIn", t.where),
            confirmText: translate("audit.cursorLogoutLocal"),
            danger: true,
          }
    );
    if (!ok) {
      switchModal.close();
      return;
    }
    switchModal.toSteps(running ? [translate("accounts.close", t.client), translate("accounts.clearLocalLogin")] : [translate("accounts.clearLocalLogin")]);
    if (running) {
      switchModal.stepStart();
      const c = await invoke(t.close);
      if (!c.closed) {
        switchModal.stepFail();
        switchModal.finish(false, translate("accounts.didNotFullyCloseCloseItManually", t.client));
        return;
      }
      switchModal.stepDone();
    }
    switchModal.stepStart();
    const r = await invoke(t.logout);
    // 本机原本就没登录：步骤照常完成，结果文案由后端给出（「当前未登录，无需退出」）
    switchModal.stepDone(r && r.wasLoggedIn === false ? translate("accounts.alreadySignedOutLocally") : "");
    switchModal.finish(true, (r?.message && translate(r.message)) || translate("accounts.signedOutOfLocal", t.login));
  } catch (error) {
    switchModal.stepFail();
    switchModal.finish(false, translate("accounts.signOutFailed", mapSwitchError(error)));
  } finally {
    switching = false;
    render();
  }
}

/* ---------- 从本机导入 ---------- */

// 读取本机已登录的指定类型凭据并加入托管列表（Cursor 读认证库，ChatGPT 读 auth.json，
// Claude 读 .credentials.json / macOS Keychain）；后端保证同一账号不重复导入，导入后逐个后台验证。
async function onImportLocal(kind) {
  if (importing || switching || anyAccountRefreshing()) return;
  const target = ["codex", "claude", "opencode"].includes(kind) ? kind : "cursor";
  const label = kindLabel(target);
  importing = true;
  setStatus("", () => translate("accounts.readingLocalLogin", label));
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
      const tail = exists > 0 ? translate("accounts.alreadyPresent", exists) : "";
      setStatus("ok", () => translate("accounts.importedAccounts", imported.length, labels, tail));
    } else if (exists > 0) {
      setStatus("", () => translate("accounts.theLocalAccountIsAlreadyListed", label));
    } else if (invalid > 0) {
      setStatus("warn", () => translate("accounts.skippedUnreadableLocalCredentials", invalid));
    } else {
      setStatus("warn", () => translate("accounts.noLocalLoginWasFound", label));
    }
    if (target === "opencode" && imported.length) setStatus("ok", () => translate("accounts.localWorkspaceLinkedAuthorizeItSeparatelyIn"));
    // 新导入的账户尚未验证，后台排队逐个刷新，不阻塞汇总提示
    void refreshIds(imported.map((item) => item.id));
  } catch (error) {
    setStatus("bad", () => translate("accounts.importFailed", resetError(error)));
  } finally {
    importing = false;
    render();
  }
}

/* ---------- JSON 文件导入 / 导出 ---------- */

async function onExport(id) {
  if (importing || switching || (id !== null && !id)) return;
  importing = true;
  setStatus("", () => translate("accounts.exportingAccounts"));
  render();
  try {
    const r = id === null ? await invoke("accounts_export_all") : await invoke("account_export_one", { id });
    if (r && r.cancelled) {
      clearStatus();
      return;
    }
    const count = Number(r && r.count) || 0;
    setStatus("ok", () => translate("accounts.exportedAccounts", count));
  } catch (error) {
    const msg = errorCode(error);
    if (msg === "no_accounts") setStatus("warn", () => translate("accounts.noAccountsToExport"));
    else if (msg === "not_found") setStatus("warn", () => translate("accounts.thisAccountWasDeleted"));
    else setStatus("bad", () => translate("accounts.exportFailed", resetError(msg)));
  } finally {
    importing = false;
    render();
  }
}

async function onImportFile() {
  if (importing || switching || anyAccountRefreshing()) return;
  importing = true;
  setStatus("", () => translate("accounts.importingAccounts"));
  render();
  try {
    const r = await invoke("accounts_import_file");
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
    const skippedText = [exists && translate("accounts.alreadyPresent2", exists), invalid && translate("accounts.invalidCredentials", invalid)].filter(Boolean).join("、");
    if (imported.length) {
      setStatus(invalid ? "warn" : "ok", () => translate("accounts.importedAccounts2", imported.length, skippedText ? translate("accounts.skipped") + skippedText + "。" : ""));
      void refreshIds(imported.map(item => item.id));
    } else if (skippedText) {
      setStatus(invalid ? "warn" : "", () => translate("accounts.skipped2", skippedText));
    } else {
      setStatus("warn", () => translate("accounts.noAccountsToImportInThisFile"));
    }
  } catch (error) {
    const msg = errorCode(error);
    if (msg === "invalid_format") setStatus("bad", () => translate("accounts.invalidAccountExportFile"));
    else setStatus("bad", () => translate("accounts.importFailed", resetError(msg)));
  } finally {
    importing = false;
    render();
  }
}

/* ---------- 添加 / 编辑弹窗 ---------- */

let accountModal;
function getAccountModal() {
  return accountModal ||= createModal(el("#account-modal"), { onClose: () => {
    authEditor?.dispose();
    authEditor = null;
    authAdder?.dispose();
    authAdder = null;
    editingId = null;
    editingAccount = null;
    el("#account-token").value = el("#account-refresh-token").value = "";
  } });
}

function setModalKind(kind) {
  modalKind = ["codex", "claude", "opencode"].includes(kind) ? kind : "cursor";
  el("#account-token").placeholder = TOKEN_PLACEHOLDERS[modalKind];
  el("#account-refresh-field").hidden = modalKind === "cursor";
  el("#account-api-field").hidden = modalKind !== "cursor" || editingId == null;
  el("#account-auto-refresh-field").hidden = modalKind === "cursor" || editingId == null;
  el("#account-desktop-binding-field").hidden = modalKind !== "claude" || editingId == null;
  el("#account-refresh-token").placeholder =
    modalKind === "claude"
      ? translate("accounts.refreshtokenFromClaudeCredentialsJsonUsedTo")
      : translate("account.tokensRefreshTokenFromCodexAuthJson");
  // OAuth 授权添加仅用于新增 Claude 账户（编辑场景改凭据走手动粘贴）
  el("#account-oauth-field").hidden = modalKind !== "claude" || editingId != null;
  el("#account-import-local").textContent = translate("accounts.importLocal", kindLabel(modalKind));
}

function openModal(account, presetKind) {
  if (getAccountModal().busy) return;
  authEditor?.dispose();
  authEditor = null;
  authAdder?.dispose();
  authAdder = null;
  editingAccount = account ? {
    note: (account.note || "").trim(), token: (account.token || "").trim(), refreshToken: (account.refreshToken || "").trim(),
    refreshIntervalMinutes: account.refreshIntervalMinutes ?? null, autoRefreshToken: account.autoRefreshToken !== false,
  } : null;
  editingId = account ? account.id : null;
  const kind = account ? account.kind : presetKind || "cursor";
  const codexEdit = account?.kind === "codex";
  el("#account-modal-title").textContent = translate("accounts.account", account ? translate("accounts.edit") : translate("pricing.add"), kindLabel(kind));
  el("#account-modal").classList.toggle("codex-auth", kind === "codex");
  el("#account-modal-description").textContent = kind === "codex"
    ? translate("accounts.credentialsAreStoredInThisAccountS")
    : translate("account.tokensAreStoredInYourLocalConfiguration");
  el(codexEdit ? "#account-interval-field" : "#account-api-field").before(el("#account-credential-fields"));
  el("#account-auth-mode").hidden = !codexEdit;
  el("#account-token-label").textContent = kind === "codex" ? "Access Token" : "Token";
  el("#account-note").value = account ? account.note || "" : "";
  el("#account-token").value = account ? account.token || "" : "";
  el("#account-refresh-token").value = account ? account.refreshToken || "" : "";
  if (codexEdit) authEditor = createCodexAuthEditor(account);
  else if (kind === "codex") authAdder = createCodexAuthAdder({ isSaving: () => getAccountModal().busy, clearStatus: clearModalStatus });
  el("#account-auto-refresh-token").checked = account?.autoRefreshToken !== false;
  el("#account-ignore-api-models").checked = !!(account && account.ignoreApiModels);
  el("#account-interval-field").hidden = !account;
  el("#account-refresh-interval").value = account?.refreshIntervalMinutes == null ? "" : String(account.refreshIntervalMinutes);
  el("#account-oauth-code").value = "";
  setModalKind(kind);
  // 「从本机导入」是添加场景的快捷入口，编辑时隐藏；「清除本地数据」只在编辑 Cursor 账户时提供
  el("#account-import-local").hidden = !!account;
  const clearBtn = el("#account-clear-usage");
  clearBtn.hidden = !(account && account.kind === "cursor");
  clearBtn.disabled = false;
  clearModalStatus();
  getAccountModal().open();
  if (modalKind === "claude" && editingId) void loadDesktopBinding(editingId);
  el("#account-note").focus();
}

async function loadDesktopBinding(id) {
  const session = getAccountModal().session;
  const label = el("#account-desktop-binding-status");
  const button = el("#account-desktop-bind");
  label.textContent = translate("accounts.detecting"); button.disabled = true;
  try {
    const status = await invoke("local_clients_status", { kind: "claude", id });
    if (!getAccountModal().isCurrent(session) || editingId !== id) return;
    const binding = status.desktop?.binding;
    label.textContent = status.desktop?.error ? clientError(status.desktop.error)
      : !status.desktop?.installed ? translate("accounts.claudeDesktopNotDetected")
      : binding?.state === "bound" ? translate("accounts.desktopLoginLinkedToThisAccount")
      : binding?.state === "pending" ? translate("accounts.unfinishedLinkTheOriginalLoginCanBe") : translate("accounts.desktopLoginNotLinked");
    button.textContent = binding?.state === "bound" ? translate("accounts.linkAgain") : binding?.state === "pending" ? translate("accounts.restoreOriginalLogin") : translate("account.linkDesktopLogin");
    button.disabled = !status.desktop?.installed || !!status.desktop?.error;
    button.onclick = async () => {
      if (button.disabled || getAccountModal().busy || editingId !== id) return;
      const operation = getAccountModal().beginSave();
      button.disabled = true;
      try {
        if (binding?.state === "pending") await invoke("claude_desktop_binding_cancel", { id: binding.operationId });
        else await bindClaudeDesktop(id);
      } catch (error) { if (operation.current()) label.textContent = clientError(error); }
      finally { operation.finish(); if (operation.current()) void loadDesktopBinding(id); }
    };
  } catch (error) { if (getAccountModal().isCurrent(session)) label.textContent = clientError(error); }
}

function closeModal() {
  getAccountModal().close();
}

/**
 * 编辑 Cursor 账户时的「清除本地数据」：确认后删除该账户在本机的用量事件库与统计缓存，
 * 账户与 Token 保留，下次刷新用量时重新全量拉取。结果显示在弹窗状态条，弹窗保持打开。
 */
async function onClearLocalUsage() {
  if (getAccountModal().busy) return;
  const session = getAccountModal().session;
  const id = editingId;
  const account = accounts.find((item) => item.id === id);
  if (!account || account.kind !== "cursor") return;
  const label = cursorIdentity(account).primary;
  const ok = await confirmDialog({
    title: translate("raw.clearLocalData"),
    body: translate("accounts.clearUsageEventsAndCachedStatisticsFor", label),
    confirmText: translate("accounts.clear"),
    danger: true,
  });
  // 确认期间弹窗可能已被关掉（Esc / 切换到别的账户），不再落地
  if (!ok || !getAccountModal().isCurrent(session) || editingId !== id) return;
  const btn = el("#account-clear-usage");
  const operation = getAccountModal().beginSave([btn]);
  if (!operation) return;
  try {
    const result = await clearCursorLocalData(id);
    if (!operation.current()) return;
    setModalStatus("ok", translate("accounts.clearedLocalUsageEventsTheNextRefresh", result.events));
  } catch (error) {
    if (!operation.current()) return;
    const code = errorCode(error);
    setModalStatus("bad", code === "no_usage_data" ? translate("accounts.noLocalUsageDataForThisAccount") : translate("accounts.clearFailed", resetError(code)));
  } finally {
    operation.finish();
  }
}

async function onSave() {
  if (getAccountModal().busy || authEditor?.busy || authAdder?.busy) return;
  const token = el("#account-token").value.trim();
  if (!token && !authEditor?.full && !authAdder?.full) {
    setModalStatus("bad", modalKind === "claude" ? translate("accounts.enterATokenOrUseOauthAbove") : translate("accounts.enterAToken"));
    return;
  }
  const note = el("#account-note").value.trim();
  const refreshToken = modalKind !== "cursor" ? el("#account-refresh-token").value.trim() || null : null;
  const ignoreApiModels = modalKind === "cursor" && el("#account-ignore-api-models").checked;
  const interval = el("#account-refresh-interval").value;
  const refreshIntervalMinutes = interval === "" ? null : Number(interval);
  const autoRefreshToken = modalKind === "cursor" ? null : el("#account-auto-refresh-token").checked;
  const updateCredentials = !editingAccount || token !== editingAccount.token || (refreshToken || "") !== editingAccount.refreshToken;
  const isEdit = editingId != null;
  const id = editingId;
  const existingIds = isEdit ? null : new Set(accounts.map((account) => account.id));
  const btn = el("#account-save");
  const operation = getAccountModal().beginSave([
    btn, el("#account-oauth-finish"), el("#account-import-local"),
    el("#account-mode-token"), el("#account-mode-auth"), el("#account-auth-reload"),
    el("#account-auth-pick"), ...el("#account-add-mode").querySelectorAll("[role=tab]"),
  ]);
  if (!operation) return;
  authAdder?.clear();
  setModalStatus("", translate("accounts.saving"));
  try {
    const addedCredentials = authAdder?.credentials();
    let update = { id, note, token, refreshToken, ignoreApiModels, refreshIntervalMinutes, autoRefreshToken, updateCredentials };
    if (authEditor) {
      authEditor.clear();
      const original = editingAccount;
      const credentials = await authEditor.changes();
      if (!operation.current()) return;
      const noteChanged = note !== original.note;
      const intervalChanged = refreshIntervalMinutes !== original.refreshIntervalMinutes;
      const renewalChanged = autoRefreshToken !== original.autoRefreshToken;
      if (!credentials && !noteChanged && !intervalChanged && !renewalChanged) {
        closeModal();
        return;
      }
      update = { id, updateCredentials: false, updateInterval: intervalChanged, ...credentials };
      if (noteChanged) update.note = note;
      if (intervalChanged) update.refreshIntervalMinutes = refreshIntervalMinutes;
      if (renewalChanged) update.autoRefreshToken = autoRefreshToken;
    }
    const view = isEdit
      ? await invoke("accounts_update", update)
      : await invoke("accounts_add", { account: { kind: modalKind, note, ...(addedCredentials || { token, refreshToken }) } });
    applyView(view);
    const added = existingIds ? accounts.find((account) => !existingIds.has(account.id)) : null;
    render();
    if (operation.current()) closeModal();
    setStatus("ok", () => isEdit ? translate("accounts.accountUpdated") : translate("accounts.accountAdded"));
    // 手动新增与本机/文件导入保持一致：保存后后台拉取一次完整状态，失败留在对应账户行提示。
    if (added) void refreshOne(added.id);
  } catch (error) {
    if (!operation.current()) return;
    if (authEditor?.showError(error)) { clearModalStatus(); return; }
    if (authAdder?.showError(error)) { clearModalStatus(); return; }
    const msg = errorCode(error);
    if (msg === "duplicate_account") setModalStatus("bad", translate("accounts.thisAccountAlreadyExists"));
    else setModalStatus("bad", translate("accounts.saveFailed", resetError(msg)));
  } finally {
    operation.finish();
  }
}

/* ---------- Claude OAuth 授权添加 ---------- */

// 打开浏览器授权页：后端生成 PKCE 上下文并尝试唤起系统浏览器，失败时提示手动打开。
async function onOauthOpen() {
  if (getAccountModal().busy) return;
  const session = getAccountModal().session;
  const btn = el("#account-oauth-open");
  btn.disabled = true;
  setModalStatus("", translate("accounts.openingTheAuthorizationPage"));
  try {
    const r = await invoke("claude_oauth_begin");
    if (!getAccountModal().isCurrent(session)) return;
    if (r && r.opened) {
      setModalStatus("", translate("accounts.signInAndAuthorizeInTheBrowser"));
    } else {
      // 唤起失败（如无默认浏览器）：把授权地址复制到剪贴板兜底
      try { await navigator.clipboard.writeText(r.url); } catch { /* ignore */ }
      setModalStatus("warn", translate("accounts.theAuthorizationUrlWasCopiedPasteIt"));
    }
  } catch (error) {
    if (getAccountModal().isCurrent(session)) setModalStatus("bad", translate("accounts.couldNotStartAuthorization", resetError(error)));
  } finally {
    btn.disabled = false;
  }
}

/** OAuth 完成阶段的错误码 -> 用户可读文案。 */
function mapOauthError(err) {
  const code = errorCode(err);
  if (code === "oauth_not_started") return translate("accounts.selectAuthorizeInBrowserFirst");
  if (code === "oauth_code_empty") return translate("accounts.pasteTheCodeFromYourBrowser");
  if (code === "duplicate_account") return translate("accounts.thisAccountAlreadyExists");
  if (code.startsWith("oauth_exchange_denied")) {
    return translate("accounts.invalidOrExpiredCodeStartBrowserAuthorization");
  }
  return resetError(code);
}

// 用授权码换取凭据并直接落库；成功后关弹窗并后台刷新新账户。
async function onOauthFinish() {
  if (getAccountModal().busy) return;
  const code = el("#account-oauth-code").value.trim();
  if (!code) {
    setModalStatus("bad", translate("accounts.pasteTheCodeFromYourBrowser"));
    return;
  }
  const btn = el("#account-oauth-finish");
  const operation = getAccountModal().beginSave([btn, el("#account-save"), el("#account-import-local")]);
  if (!operation) return;
  setModalStatus("", translate("accounts.obtainingLoginCredentials"));
  try {
    const r = await invoke("claude_oauth_finish", { code });
    applyView(r.view);
    render();
    if (operation.current()) closeModal();
    setStatus("ok", () => translate("accounts.accountAdded2", r.label));
    void refreshOne(r.id);
  } catch (error) {
    if (operation.current()) setModalStatus("bad", translate("accounts.authorizationFailed", mapOauthError(error)));
  } finally {
    operation.finish();
  }
}

/* ---------- 初始化 ---------- */

/** 账户数据加载错误码 -> 用户可读文案，未知错误原样显示。 */
function mapLoadError(err) {
  const msg = errorCode(err);
  if (msg.includes("settings_parse_failed")) {
    return translate("accounts.theSettingsFileIsDamagedTheOriginal");
  }
  if (msg.includes("settings_read_failed")) {
    return translate("accounts.settingsAreTemporarilyUnreadableTryAgainLater");
  }
  return resetError(msg);
}

async function loadInitial(allowRetry = true) {
  try {
    const view = await loadAccountList();
    applyView(view);
    render();
  } catch (error) {
    // 文件被短暂占用属于瞬态故障，自动重试一次再报错
    if (allowRetry) {
      setStatus("", () => translate("accounts.accountDataIsTemporarilyUnavailableRetrying"));
      setTimeout(() => { void loadInitial(false); }, 1500);
      return;
    }
    setStatus("bad", () => translate("accounts.couldNotLoadAccounts", mapLoadError(error)));
  }
}

export function initAccounts() {
  window.addEventListener("uilanguagechange", render);
  rowOrder = initRowOrder(el("#accounts-body"), {
    ids: displayOrderIds, render,
    save: accountOrder.move,
    onError: () => setStatus("bad", () => translate("accounts.accountOrderCouldNotBeSavedTry")),
  });
  const pickerRoot = el("#account-type-modal");
  const picker = createModal(pickerRoot, { mode: () => "view" });
  const types = [...pickerRoot.querySelectorAll("[data-kind]")];
  let pickerMode;
  const openPicker = (mode = "add") => {
    if (switching || importing || !pickerRoot.hidden) return;
    pickerMode = mode;
    el("#account-type-title").textContent = mode === "logout" ? translate("accounts.signOutLocally") : translate("account.chooseAnAccountType");
    for (const button of types) {
      button.hidden = mode === "logout" && !LOGOUT_TARGETS[button.dataset.kind];
      button.disabled = mode === "logout" && providerRefreshing(button.dataset.kind);
    }
    picker.open();
    types.find(button => !button.hidden && !button.disabled)?.focus();
  };
  el("#accounts-add").addEventListener("click", () => openPicker());
  el("#accounts-logout").addEventListener("click", () => openPicker("logout"));
  for (const button of types) {
    button.addEventListener("click", () => {
      if (button.hidden || button.disabled) return;
      const kind = button.dataset.kind;
      picker.close();
      if (pickerMode === "logout") { void onLogoutLocal(kind); return; }
      if (kind === "opencode") addOpenCode(async ids => {
        await loadInitial(false);
        if (ids.length) void refreshIds(ids);
      });
      else openModal(null, kind);
    });
  }
  async function consumeTrayAdd() {
    if (!await invoke("tray_take_add_request")) return;
    el('[data-panel="accounts-panel"]').click();
    el("#accounts-add").focus();
    openPicker();
  }
  listen("tray-add-account", () => { void consumeTrayAdd().catch(() => {}); })
    .then(consumeTrayAdd).catch(() => {});
  el("#accounts-refresh").addEventListener("click", () => { void refreshAllAccounts(); });
  el("#accounts-import").addEventListener("click", () => { void onImportFile(); });
  el("#accounts-export").addEventListener("click", () => { void onExport(null); });
  for (const [id, mode] of [["account-mode-token", "token"], ["account-mode-auth", "auth"], ["account-auth-reload", "auth"]]) {
    el("#" + id).addEventListener("click", () => {
      if (!getAccountModal().busy) void authEditor?.switchMode(mode, id === "account-auth-reload");
    });
  }

  el("#account-save").addEventListener("click", () => { void onSave(); });
  el("#account-import-local").addEventListener("click", () => {
    if (getAccountModal().busy || authAdder?.busy) return;
    const kind = modalKind;
    closeModal();
    void onImportLocal(kind);
  });
  el("#account-clear-usage").addEventListener("click", () => { void onClearLocalUsage(); });
  el("#account-oauth-open").addEventListener("click", () => { void onOauthOpen(); });
  el("#account-oauth-finish").addEventListener("click", () => { void onOauthFinish(); });

  // 切换流程弹窗：事件一次性绑定，按 switchModal 当前阶段分发
  el("#switch-ok").addEventListener("click", () => switchModal.onOk());
  el("#switch-alt").addEventListener("click", () => switchModal.onAlt());
  // 确认阶段的文本输入：回车等同于点主按钮
  el("#switch-modal-input").addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    switchModal.onOk();
  });

  // 订阅后端广播：托盘面板等其他入口改动账户数据后，本表实时同步
  listen("accounts-changed", (event) => onBackendChanged(event.payload)).catch(() => {
    /* 非 Tauri 环境（浏览器直开调试）无事件桥，忽略 */
  });
  // 任何窗口发起的账户刷新开始 / 结束：同步该行的「刷新中」动画（本窗口自己发起的也会收到，无副作用）
  listen("account-refreshing", (event) => {
    const payload = event.payload || {};
    const id = String(payload.id || "");
    if (!id) return;
    if (payload.active) remoteRefreshingIds.add(id);
    else {
      remoteRefreshingIds.delete(id);
      if (!refreshingIds.has(id)) refreshScheduler.completed(id);
    }
    if (accounts.some((a) => a.id === id)) updateRow(id);
    updateHeadingActions();
  }).catch(() => {
    /* 非 Tauri 环境无事件桥 */
  });

  // 「上次刷新」相对时间随时间流逝定期重算：只改时间文本与组标题小字，
  // 不整表重绘（避免打断进度条动画与删除确认态）
  setInterval(() => {
    if (!accounts.length) return;
    for (const account of accounts) {
      const cell = el("#accounts-body").querySelector(
        `tr[data-id="${CSS.escape(account.id)}"] .account-time`
      );
      if (cell) fillTimeCell(cell, account);
    }
    updateAccountCount();
  }, 30_000);

  render();
  void loadInitial();
}
