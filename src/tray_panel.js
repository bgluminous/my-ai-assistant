import { el, invoke, listen, resetError, fmtInt, fmtTokens, fmtUsd, colorFor, chartAnimMs, setChartHoverHit, bindChartHoverLeave, setupDesktopGuards } from "./shared.js";
import { membershipLabel, codexPlanLabel, relativeFromUnixSeconds, remainInfo, onDemandBrief, creditsBrief, maskToken } from "./accounts.js";
import {
  USAGE_CACHE_PREFIX,
  USAGE_CACHE_EVENT,
  DEFAULT_USAGE_TTL_MS,
  setUsageCacheTtlMs,
  isUsageCacheFresh,
  localYmd,
  todayStartMs,
  todayRangeKey,
  recentYmds,
  peekAggForDay,
  peekAggSeries,
  peekScanForDay,
  peekScanSeries,
  fetchCursorAggregate,
  fetchCodexScan,
  sliceDay,
  overlayDay,
  modelsOnDay,
  forgetUsageCacheFromEvent,
} from "./usage_data.js";

// 托盘面板：托盘图标单击弹出的简易面板，分三个 tab：
// 总览（今日 token 用量合计）/ Cursor / Codex（账户列表，查看状态额度、单账户刷新 / 一键切换、全部刷新），
// 增删改等完整功能在主窗口。窗口失焦即隐藏（Rust 侧处理），每次获得焦点时重载数据。

let accounts = [];
let refreshing = false;
let switching = false;
let overviewLoading = false;
const refreshingIds = new Set();

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

/** 托盘紧凑账户名：优先可读身份；自动生成的 Cursor user_id 采用两端保留的省略格式。 */
function trayAccountLabel(account) {
  const note = String((account && account.note) || "").trim();
  if (!account || account.kind !== "cursor") return note || "未命名账户";
  const status = account.status || null;
  const statusName = String((status && status.name) || "").trim();
  const statusEmail = String((status && status.email) || "").trim();
  const label = (account.noteAuto === false ? note : "") || statusName || statusEmail || note;
  const looksLikeCredential = label.startsWith("user_") || label.includes("::") || label.split(".").length === 3;
  return looksLikeCredential ? maskToken(label) : label || "未命名账户";
}

function syncHeaderRefresh() {
  const btn = el("#tray-refresh");
  const accountBusy = refreshing || switching || refreshingIds.size > 0;
  if (trayTab === "overview") {
    btn.disabled = overviewLoading;
    btn.classList.toggle("busy", overviewLoading);
    return;
  }
  btn.disabled = accountBusy || !accounts.length;
  btn.classList.toggle("busy", refreshing);
}

/* ---------- Tab 状态 ---------- */

const TAB_KEY = "trayTab";

function readTab() {
  try {
    const v = localStorage.getItem(TAB_KEY);
    return v === "cursor" || v === "codex" || v === "overview" ? v : "overview";
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
  if (tab === "overview") void loadOverview(false);
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
  if (account.kind === "codex") {
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
    // Grok Bot 周额度（sand）：套餐包含时展示，额度耗尽按 100% 已用处理（与主窗口一致）
    const sand = status.sand || null;
    if (sand && sand.included) {
      let usedPct = Number(sand.usagePercent);
      if (sand.hasAvailableUsage === false) usedPct = 100;
      const grok = miniBar("Grok", usedPct);
      if (grok) bars.push(grok);
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
  const label = trayAccountLabel(account);
  note.textContent = label;
  note.title = label;
  noteLine.append(note);
  main.append(noteLine);

  // 副行：套餐 · 有效期 · 超额/余额 · 上次刷新时间 ·（已失效）
  const sub = document.createElement("div");
  sub.className = "tray-sub";
  const status = account.status || null;
  const plan = account.kind === "codex"
    ? codexPlanLabel(status && status.plan)
    : membershipLabel(status && status.membershipType);
  const parts = [];
  if (plan) parts.push(plan);
  // 有效期与主窗口套餐列口径一致：Cursor = 本期计费周期截止，Codex = 套餐订阅到期
  const endIso = status && status.alive === true
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
  const extra = account.kind === "codex" ? creditsBrief(status) : onDemandBrief(status);
  if (extra) {
    parts.push(extra.text);
    if (extra.title) titles.push(extra.title);
  }
  parts.push(`刷新于 ${relativeFromUnixSeconds(account.lastRefreshAt)}`);
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
  const rowBusy = refreshing || switching || refreshingIds.has(account.id);

  const refreshBusy = refreshingIds.has(account.id);
  const refreshBtn = iconAction("refresh", refreshBusy ? "刷新中…" : "刷新");
  refreshBtn.classList.toggle("busy", refreshBusy);
  refreshBtn.disabled = rowBusy;
  refreshBtn.addEventListener("click", () => { void refreshOne(account.id); });

  // 仅已验证有效的账户可切换（Codex 还需 Refresh Token 换新凭据）；确认在弹窗内进行
  const aliveOk = alive === true;
  const switchBtn = iconAction("switch", "切换");
  if (account.kind === "codex") {
    const hasRt = !!String(account.refreshToken || "").trim();
    switchBtn.disabled = rowBusy || !aliveOk || !hasRt;
    switchBtn.title = !aliveOk
      ? "请先刷新验证该账户"
      : !hasRt
      ? "该账户没有 Refresh Token，无法切换"
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

function render() {
  // 总览 tab 下列表隐藏，无需重建行（切回账户 tab 时会重新渲染）
  if (trayTab !== "overview") {
    const list = el("#tray-list");
    list.replaceChildren();
    const subset = trayTab === "codex"
      ? accounts.filter((a) => a.kind === "codex")
      : accounts.filter((a) => a.kind !== "codex");
    if (!subset.length) {
      const empty = document.createElement("div");
      empty.className = "tray-empty";
      empty.textContent = trayTab === "codex" ? "暂无 ChatGPT 账户，请到主窗口添加。" : "暂无 Cursor 账户，请到主窗口添加。";
      list.append(empty);
    } else {
      for (const account of subset) list.append(accountRow(account));
    }
  }
  syncHeaderRefresh();
}

/* ---------- 数据 ---------- */

async function load() {
  try {
    const view = await invoke("accounts_list");
    accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
    const u = Number(view && view.usageIntervalMinutes);
    setUsageCacheTtlMs(u > 0 ? u * 60_000 : DEFAULT_USAGE_TTL_MS);
    render();
    if (trayTab === "overview") void loadOverview(false);
  } catch (error) {
    setStatus("bad", `加载失败：${resetError(error)}`);
  }
}

async function refreshOne(id) {
  if (refreshing || switching || refreshingIds.has(id)) return false;
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
  // 排队逐个刷新（每次一个）；id 先快照，刷新期间列表可能被广播更新
  const ids = accounts.map((a) => a.id);
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

/* ---------- 总览：今日 token 用量（与主窗口用量页共用 usage_data 缓存） ---------- */

function overviewTip(text) {
  teardownOverview();
  const tip = document.createElement("div");
  tip.className = "tray-empty";
  tip.textContent = text;
  el("#tray-overview").replaceChildren(tip);
}

/* ---------- 总览图表（Chart.js 全局脚本，缺失时静默跳过） ---------- */

let sourcePie = null;
let modelBar = null;
let dailyStack = null;
let overviewDom = null; // 常驻 DOM，刷新时原地改数字 / 更新图表，避免整页重建闪烁

function destroyCharts() {
  if (sourcePie) { sourcePie.destroy(); sourcePie = null; }
  if (modelBar) { modelBar.destroy(); modelBar = null; }
  if (dailyStack) { dailyStack.destroy(); dailyStack = null; }
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

/** 图表小节：标题 + 定高画布容器，返回 { wrap, canvas, box }。 */
function chartSection(title, height) {
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
  wrap.append(label, box);
  return { wrap, canvas, box };
}

/** 坐标轴刻度用的紧凑 token 数（独立于单位偏好，轴上空间有限）。 */
function compactTokens(n) {
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${(n / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`;
  if (abs >= 1e6) return `${(n / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e3) return `${(n / 1e3).toFixed(abs >= 1e4 ? 0 : 1)}K`;
  return String(Math.round(n));
}

function colorForName(name) {
  const s = String(name || "");
  let h = 0;
  for (let i = 0; i < s.length; i += 1) h = (h * 33 + s.charCodeAt(i)) >>> 0;
  return colorFor(h);
}

function shortModel(name) {
  const s = String(name || "");
  if (s.length <= 20) return s;
  return `${s.slice(0, 9)}…${s.slice(-8)}`;
}

function mergeModelShares(lists) {
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
  const all = [...map.values()]
    .filter((m) => m.tokens > 0)
    .sort((a, b) => b.tokens - a.tokens || b.usd - a.usd);
  if (all.length <= 5) return all;
  const top = all.slice(0, 5);
  const rest = all.slice(5);
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

/** 来源占比饼图（环形）：各来源今日 token 数；悬停高亮该扇区、其余变暗。 */
function makeSourcePie(canvas, shares) {
  const colors = shares.map((_, i) => colorFor(i));
  const motion = chartMotion();
  const chart = new Chart(canvas, {
    type: "doughnut",
    data: {
      labels: shares.map((r) => r.name),
      datasets: [
        {
          data: shares.map((r) => r.tokens),
          backgroundColor: colors,
          hoverBackgroundColor: colors,
          borderColor: cssVar("--chart-border"),
          borderWidth: 2,
          hoverOffset: 8,
        },
      ],
    },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: motion.duration, easing: motion.easing, animateRotate: true, animateScale: true },
      animations: motion.animations,
      cutout: "58%",
      interaction: { mode: "nearest", intersect: true },
      onHover: onChartHover,
      plugins: {
        legend: {
          position: "right",
          labels: { color: cssVar("--chart-tick-strong"), boxWidth: 9, boxHeight: 9, font: { size: 10.5 } },
          onHover(event, item, legend) {
            if (event.native && event.native.target) event.native.target.style.cursor = "pointer";
            setChartHoverHit(legend.chart, { datasetIndex: item.datasetIndex ?? 0, index: item.index ?? -1 });
          },
          onLeave(_event, _item, legend) {
            setChartHoverHit(legend.chart, null);
          },
        },
        tooltip: {
          position: "nearest",
          callbacks: {
            label(ctx) {
              const total = ctx.dataset.data.reduce((s, v) => s + (Number(v) || 0), 0);
              const pct = total > 0 ? Math.round((ctx.parsed / total) * 100) : 0;
              return ` ${fmtTokens(ctx.parsed)}（${pct}%）`;
            },
            afterLabel(ctx) {
              const row = ctx.chart.$shares && ctx.chart.$shares[ctx.dataIndex];
              return row && Number.isFinite(row.usd) ? `等价 ${fmtUsd(row.usd)}` : "";
            },
          },
        },
      },
    },
  });
  chart.$baseColors = [colors];
  chart.$hoverKey = "";
  chart.$hoverOpts = hoverOpts(0);
  chart.$shares = shares;
  bindChartHoverLeave(chart);
  return chart;
}

/** 今日模型横向柱：合并各来源当天模型，Top 5 + 其他。 */
function makeModelBar(canvas, shares) {
  const colors = shares.map((s) => colorForName(s.model));
  const motion = chartMotion();
  const chart = new Chart(canvas, {
    type: "bar",
    data: {
      labels: shares.map((s) => s.model),
      datasets: [
        {
          data: shares.map((s) => s.tokens),
          backgroundColor: colors,
          hoverBackgroundColor: colors,
          borderRadius: 3,
          maxBarThickness: 16,
        },
      ],
    },
    options: {
      indexAxis: "y",
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: motion.duration, easing: motion.easing },
      animations: motion.animations,
      interaction: { mode: "nearest", intersect: true },
      onHover: onChartHover,
      plugins: {
        legend: { display: false },
        tooltip: {
          position: "nearest",
          callbacks: {
            title(items) {
              return items.length ? String(items[0].label) : "";
            },
            label(ctx) {
              return ` ${fmtTokens(ctx.parsed.x)}`;
            },
            afterLabel(ctx) {
              const row = ctx.chart.$shares && ctx.chart.$shares[ctx.dataIndex];
              return row && Number.isFinite(row.usd) ? `等价 ${fmtUsd(row.usd)}` : "";
            },
          },
        },
      },
      scales: {
        x: {
          beginAtZero: true,
          ticks: {
            color: cssVar("--chart-tick"),
            font: { size: 9.5 },
            maxTicksLimit: 4,
            callback: (v) => compactTokens(v),
          },
          grid: { color: cssVar("--chart-grid") },
        },
        y: {
          ticks: {
            color: cssVar("--chart-tick-strong"),
            font: { size: 10 },
            callback(value) {
              return shortModel(this.getLabelForValue(value));
            },
          },
          grid: { display: false },
        },
      },
    },
  });
  chart.$baseColors = [colors];
  chart.$hoverKey = "";
  chart.$hoverOpts = hoverOpts(0);
  chart.$shares = shares;
  bindChartHoverLeave(chart);
  return chart;
}

/** 近 7 日 Token 堆叠柱：每账户一段，数据与主窗口用量页同源。 */
function makeDailyStack(canvas, labels, sources) {
  const motion = chartMotion();
  const baseColors = sources.map((_, i) => colorFor(i));
  const datasets = sources.map((s, i) => {
    const byDate = new Map((s.daily || []).map((d) => [d.date, d]));
    return {
      label: s.label,
      data: labels.map((ymd) => {
        const d = byDate.get(ymd);
        return d && d.tokens > 0 ? d.tokens : null;
      }),
      backgroundColor: baseColors[i],
      hoverBackgroundColor: baseColors[i],
      borderRadius: 2,
      maxBarThickness: 18,
      stack: "daily",
      skipNull: true,
    };
  });
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
              const ymd = items[0].chart.data.labels[items[0].dataIndex];
              const p = String(ymd).split("-");
              return p.length === 3 ? `${Number(p[1])}月${Number(p[2])}日` : ymd;
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
              return `当日合计 ${fmtTokens(sum)}`;
            },
          },
        },
      },
      scales: {
        x: {
          stacked: true,
          ticks: {
            color: cssVar("--chart-tick"),
            font: { size: 9.5 },
            maxRotation: 0,
            callback(value) {
              const ymd = this.getLabelForValue(value);
              const p = String(ymd).split("-");
              return p.length === 3 ? `${Number(p[1])}/${Number(p[2])}` : ymd;
            },
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
  chart.$baseColors = baseColors;
  chart.$hoverKey = "";
  chart.$hoverOpts = hoverOpts(2);
  bindChartHoverLeave(chart);
  return chart;
}

function syncSourcePie(shares) {
  const wrap = overviewDom.pie.wrap;
  if (!hasChartLib() || !shares.length) {
    if (sourcePie) { sourcePie.destroy(); sourcePie = null; }
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  const colors = shares.map((_, i) => colorFor(i));
  const labels = shares.map((r) => r.name);
  const data = shares.map((r) => r.tokens);
  if (sourcePie) {
    sourcePie.data.labels = labels;
    sourcePie.data.datasets[0].data = data;
    sourcePie.data.datasets[0].backgroundColor = colors;
    sourcePie.data.datasets[0].hoverBackgroundColor = colors;
    sourcePie.$baseColors = [colors];
    sourcePie.$shares = shares;
    sourcePie.$hoverKey = "";
    sourcePie.update();
    return;
  }
  sourcePie = makeSourcePie(overviewDom.pie.canvas, shares);
}

function syncModelBar(shares) {
  const wrap = overviewDom.model.wrap;
  if (!hasChartLib() || !shares.length) {
    if (modelBar) { modelBar.destroy(); modelBar = null; }
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  overviewDom.model.box.style.height = `${Math.max(80, shares.length * 24 + 28)}px`;
  const colors = shares.map((s) => colorForName(s.model));
  const labels = shares.map((s) => s.model);
  const data = shares.map((s) => s.tokens);
  if (modelBar) {
    modelBar.data.labels = labels;
    modelBar.data.datasets[0].data = data;
    modelBar.data.datasets[0].backgroundColor = colors;
    modelBar.data.datasets[0].hoverBackgroundColor = colors;
    modelBar.$baseColors = [colors];
    modelBar.$shares = shares;
    modelBar.$hoverKey = "";
    modelBar.update();
    return;
  }
  modelBar = makeModelBar(overviewDom.model.canvas, shares);
}

function syncDailyStack(sources) {
  const wrap = overviewDom.bar.wrap;
  const labels = recentYmds(7);
  const list = (sources || []).filter((s) => s && s.label);
  const hasAny = list.some((s) => (s.daily || []).some((d) => d && d.tokens > 0));
  if (!hasChartLib() || !list.length || !hasAny) {
    if (dailyStack) { dailyStack.destroy(); dailyStack = null; }
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  const baseColors = list.map((_, i) => colorFor(i));
  const datasets = list.map((s, i) => {
    const byDate = new Map((s.daily || []).map((d) => [d.date, d]));
    return {
      label: s.label,
      data: labels.map((ymd) => {
        const d = byDate.get(ymd);
        return d && d.tokens > 0 ? d.tokens : null;
      }),
      backgroundColor: baseColors[i],
      hoverBackgroundColor: baseColors[i],
      borderRadius: 2,
      maxBarThickness: 18,
      stack: "daily",
      skipNull: true,
    };
  });
  if (dailyStack) {
    dailyStack.data.labels = labels;
    dailyStack.data.datasets = datasets;
    dailyStack.$baseColors = baseColors;
    dailyStack.$hoverKey = "";
    dailyStack.update();
    return;
  }
  dailyStack = makeDailyStack(overviewDom.bar.canvas, labels, list);
}

function ensureOverviewDom() {
  if (overviewDom) return;
  const root = el("#tray-overview");
  const label = document.createElement("div");
  label.className = "tray-ov-label";
  label.textContent = "今日已用";
  const total = document.createElement("div");
  total.className = "tray-ov-total";
  const tokens = document.createElement("span");
  tokens.className = "tray-ov-tokens";
  const usd = document.createElement("span");
  usd.className = "tray-ov-usd";
  total.append(tokens, usd);
  const pie = chartSection("今日来源", 136);
  pie.wrap.hidden = true;
  pie.box.classList.add("tray-chart-pie");
  const model = chartSection("今日模型", 128);
  model.wrap.hidden = true;
  const bar = chartSection("近 7 日 Token", 128);
  bar.wrap.hidden = true;
  const rows = document.createElement("div");
  rows.className = "tray-ov-rows";
  const foot = document.createElement("div");
  foot.className = "tray-ov-foot";
  foot.hidden = true;
  root.replaceChildren(label, total, pie.wrap, model.wrap, bar.wrap, rows, foot);
  overviewDom = { tokens, usd, rows, foot, pie, model, bar };
}

function renderOverview(data, statAtMs) {
  const hasChart = (data.dailySources || []).some((s) => (s.daily || []).some((d) => d && d.tokens > 0))
    || (data.modelShares || []).some((m) => m && m.tokens > 0);
  if (!data.rows.length && !hasChart) {
    overviewTip("暂无用量数据");
    return;
  }
  ensureOverviewDom();
  overviewDom.tokens.textContent = fmtTokens(data.totalTokens);
  overviewDom.tokens.title = fmtInt(data.totalTokens);
  overviewDom.usd.textContent = fmtUsd(data.totalUsd);

  const shares = data.rows.filter((r) => r.tokens > 0);
  syncSourcePie(shares);
  syncModelBar(data.modelShares || []);
  syncDailyStack(data.dailySources || []);

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

  if (Number.isFinite(statAtMs) && statAtMs > 0) {
    overviewDom.foot.hidden = false;
    overviewDom.foot.textContent = `统计于 ${relativeFromUnixSeconds(statAtMs / 1000)}`;
  } else {
    overviewDom.foot.hidden = true;
  }
}

/** 从共享缓存拼出托盘总览：今日数字用当天切片，近 7 日柱用更长范围的 daily。 */
function buildOverviewData(ymd) {
  const cursorAccounts = accounts.filter((a) => a.kind === "cursor");
  const rows = [];
  const dailySources = [];
  const modelLists = [];
  const ats = [];
  let totalTokens = 0;
  let totalUsd = 0;

  for (const a of cursorAccounts) {
    const label = trayAccountLabel(a);
    const dayHit = peekAggForDay(a.id, ymd);
    const seriesHit = peekAggSeries(a.id, ymd);
    if (!dayHit && !seriesHit) continue;
    const hit = dayHit || seriesHit;
    if (Number.isFinite(hit.entry.at)) ats.push(hit.entry.at);
    const sliceAgg = (dayHit || seriesHit).entry.agg;
    const sliceKey = (dayHit || seriesHit).rangeKey;
    const slice = sliceDay(sliceAgg, ymd, sliceKey);
    totalTokens += slice.tokens;
    totalUsd += slice.usd;
    rows.push({ id: a.id, name: label, tokens: slice.tokens, usd: slice.usd });
    const seriesAgg = (seriesHit || dayHit).entry.agg;
    dailySources.push({
      label,
      daily: overlayDay(seriesAgg.daily || [], ymd, slice),
      showActual: true,
    });
    modelLists.push(modelsOnDay(sliceAgg, ymd, sliceKey));
  }

  const scanDay = peekScanForDay(ymd, "");
  const scanSeries = peekScanSeries(ymd, "");
  if (scanDay || scanSeries) {
    const hit = scanDay || scanSeries;
    if (Number.isFinite(hit.entry.at)) ats.push(hit.entry.at);
    const agg = hit.entry.scan.aggregate;
    const slice = sliceDay(agg, ymd, hit.rangeKey);
    const seriesAgg = ((scanSeries || scanDay).entry.scan || {}).aggregate || agg;
    if (slice.tokens > 0 || (seriesAgg.daily && seriesAgg.daily.length) || (agg.models && agg.models.length)) {
      totalTokens += slice.tokens;
      totalUsd += slice.usd;
      rows.push({ id: "local", name: "本地 ChatGPT", tokens: slice.tokens, usd: slice.usd });
      dailySources.push({
        label: "本地用量分析",
        daily: overlayDay(seriesAgg.daily || [], ymd, slice),
        showActual: false,
      });
      modelLists.push(modelsOnDay(agg, ymd, hit.rangeKey));
    }
  }

  return {
    data: { totalTokens, totalUsd, rows, dailySources, modelShares: mergeModelShares(modelLists) },
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
  if (fetchErrors.has("local")) {
    const err = fetchErrors.get("local");
    const row = data.rows.find((r) => r.id === "local");
    if (row) row.error = err;
    else data.rows.push({ id: "local", name: "本地 ChatGPT", tokens: 0, usd: 0, error: err, empty: true });
  }
  return data;
}

function needsTodayFetch(peeked, force) {
  return force || !peeked || !isUsageCacheFresh(peeked.entry.at);
}

let overviewTail = Promise.resolve();

function loadOverview(force) {
  const run = () => loadOverviewInner(!!force);
  overviewTail = overviewTail.then(run, run);
  return overviewTail;
}

/** 统计今日用量：先读主窗口写入的 7/30/90 天缓存，缺的再拉今日并写回同一套 localStorage。 */
async function loadOverviewInner(force) {
  const ymd = localYmd();
  const dayStart = todayStartMs();
  const todayKey = todayRangeKey(ymd);
  const cursorAccounts = accounts.filter((a) => a.kind === "cursor");

  const cached = buildOverviewData(ymd);

  const jobs = [];
  const fetchErrors = new Map();
  for (const a of cursorAccounts) {
    if (!needsTodayFetch(peekAggForDay(a.id, ymd), force)) continue;
    jobs.push(
      fetchCursorAggregate(a, todayKey, { start: dayStart, end: Date.now(), force }).catch((error) => {
        fetchErrors.set(a.id, resetError(error));
      })
    );
  }
  if (needsTodayFetch(peekScanForDay(ymd, ""), force)) {
    jobs.push(
      fetchCodexScan({ sinceMs: dayStart, force }).catch((error) => {
        fetchErrors.set("local", resetError(error));
      })
    );
  }

  if (cached.data.rows.length) renderOverview(cached.data, cached.at);
  else if (!jobs.length) overviewTip("暂无用量数据");

  if (!jobs.length) return;
  overviewLoading = true;
  syncHeaderRefresh();
  if (!cached.data.rows.length && !overviewDom) overviewTip("正在统计今日用量…");
  try {
    await Promise.allSettled(jobs);
    const next = buildOverviewData(ymd);
    applyOverviewErrors(next.data, fetchErrors, cursorAccounts);
    renderOverview(next.data, next.at || Date.now());
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
  codex_refresh_denied: "Refresh Token 已失效，请编辑账户更新凭据后重试。",
  codex_id_token_missing: "未能获取登录所需的 id_token，请稍后重试。",
};

// 切换弹窗：busy（检测中）→ confirm（可取消）→ steps（进行中，禁止关闭）→ result（仅「关闭」）
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

  function settleConfirm(value) {
    const resolve = confirmResolve;
    confirmResolve = null;
    if (resolve) resolve(value);
  }

  function openBusy(titleText, text) {
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

  function stepDone() { setIcon("done"); }

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
  }

  function close() {
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
      const st = await invoke("codex_client_status", { id });
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
        ? ["关闭 ChatGPT", "换取登录凭证并写入", "启动 ChatGPT"]
        : ["换取登录凭证并写入", "启动 ChatGPT"]);
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
      await invoke("codex_switch_local", { id });
      trayModal.stepDone();
      await load();
      trayModal.stepStart();
      const l = await invoke("codex_client_launch");
      trayModal.stepDone();
      trayModal.finish(true, l.launched ? "已切换账户并启动 ChatGPT。" : "已切换，请手动启动 ChatGPT。");
      return;
    }
    trayModal.openBusy("切换本机 Cursor 登录", "正在检测本地 Cursor…");
    const st = await invoke("cursor_client_status", { id });
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
    const text = code.startsWith("codex_auth_write_failed")
      ? "写入本机 ChatGPT 登录文件失败，请检查文件权限后重试。"
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
// 全部刷新按当前 tab 分流：总览 = 忽略缓存重新统计今日用量；账户 tab = 刷新全部账户状态
el("#tray-refresh").addEventListener("click", () => {
  if (trayTab === "overview") void loadOverview(true);
  else void refreshAll();
});
el("#tray-open").addEventListener("click", () => { void invoke("tray_open_main"); });
for (const btn of document.querySelectorAll(".tray-tab")) {
  btn.addEventListener("click", () => setTab(btn.dataset.tab));
}
applyTab();
// 面板每次被托盘点击唤起（获得焦点）时重载缓存数据；
// 只清状态条、重绘当前 tab，不触碰切换弹窗（进行中的弹窗须保持原状）
window.addEventListener("focus", () => {
  clearStatus();
  void load();
});
// 订阅后端广播：主窗口刷新 / 增删改账户时，开着的面板实时同步（获焦重载仍保留作兜底）
listen("accounts-changed", (event) => {
  const view = event.payload;
  accounts = view && Array.isArray(view.accounts) ? view.accounts : [];
  const u = Number(view && view.usageIntervalMinutes);
  if (Number.isFinite(u)) setUsageCacheTtlMs(u > 0 ? u * 60_000 : DEFAULT_USAGE_TTL_MS);
  render();
  if (trayTab === "overview") void loadOverview(false);
}).catch(() => {
  /* 非 Tauri 环境（浏览器直开调试）无事件桥，忽略 */
});
let usageCacheTimer = null;
function scheduleOverviewFromCache(key) {
  forgetUsageCacheFromEvent(key);
  if (trayTab !== "overview") return;
  clearTimeout(usageCacheTimer);
  usageCacheTimer = setTimeout(() => { void loadOverview(false); }, 250);
}
window.addEventListener("storage", (event) => {
  if (!event.key || !event.key.startsWith(USAGE_CACHE_PREFIX)) return;
  scheduleOverviewFromCache(event.key);
});
listen(USAGE_CACHE_EVENT, (event) => {
  const key = event.payload && event.payload.key;
  scheduleOverviewFromCache(key);
}).catch(() => {
  /* 非 Tauri 环境无事件桥，仍靠 storage / 获焦 */
});
void load();
