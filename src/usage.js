import { el, fmtInt, fmtTokens, fmtUsd, colorFor, chartAnimMs, setChartHoverHit, bindChartHoverLeave, resetError, toast, dismissToast } from "./shared.js";
import {
  getCachedAgg as cacheGetAgg,
  getCachedScan as cacheGetScan,
  fetchCursorAggregate,
  fetchCodexScan,
  purgeAccountCache,
  purgeMissingAccounts,
  clearUsageMemoryCache,
  setUsageCacheTtlMs,
  DEFAULT_USAGE_TTL_MS,
} from "./usage_data.js";
import {
  getAccounts,
  onAccountsChanged,
  membershipLabel,
  planMonthlyUsd,
  getUsageIntervalMinutes,
  setUsageInterval,
} from "./accounts.js";

// 用量统计：Cursor 账单数据源自「账户管理」中保存的 Cursor 账户，自动拉取，无需手动输入。
// Codex 账户不在本页展示（额度信息见「账户管理」）；Codex 账单只能来自本地会话日志，
// 由本地扫描（CODEX_HOME）折算，在总览中作为「本地用量分析」行参与合并。
// 视图：全部总览（各 Cursor 账户 + 本地 Codex 合并）/ 单个 Cursor 账户 / 本地 Codex 用量分析。
//
// 缓存策略：与托盘总览共用 usage_data.js（内存 + localStorage，键前缀 usage-cache:v2:）。
// 打开视图时先用缓存（含过期缓存）立即渲染，再在后台拉取最新数据原地刷新（stale-while-revalidate）。

const DEFAULT_TTL_MS = DEFAULT_USAGE_TTL_MS; // 未开启自动更新时的结果缓存有效期
const OVERVIEW_CONCURRENCY = 2;

let selection = "all"; // "all" | "local" | Cursor 账户 id
let loadSeq = 0; // 加载序号，防止过期的异步结果覆盖新视图
let loading = false;
let panelVisible = false;
let renderedFor = ""; // 结果区当前展示的 selection+range（隐藏时为空）
let renderedAt = 0;
let usageInterval = 0; // 统计自动更新间隔（分钟，0 = 关闭），独立于账户状态刷新
let usageTimerId = null;
let accountIdsSig = "";
const overviewResults = new Map(); // accountId -> { state, agg?, error? }

/* ---------- 图表主题 ---------- */

let barChart = null;
let doughnutChart = null;
let dailyChart = null;

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
  if (barChart) {
    barChart.options.scales.x.ticks.color = t.tick;
    barChart.options.scales.x.grid.color = t.grid;
    barChart.options.scales.y.ticks.color = t.tickStrong;
    barChart.update("none");
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

function escapeHtml(text) {
  return String(text).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

function rangeDays() {
  return Number(el("#usage-range").value);
}
function rangeBounds() {
  const days = rangeDays();
  if (days > 0) {
    const end = Date.now();
    return { start: end - days * 86400 * 1000, end };
  }
  return { start: null, end: null };
}
function rangeText() {
  const d = rangeDays();
  return d > 0 ? `近 ${d} 天` : "全部";
}

function maskToken(token) {
  const t = String(token || "").trim();
  if (!t) return "—";
  return t.length > 12 ? `${t.slice(0, 8)}…` : t;
}
function accountLabel(account) {
  return account.note || `${account.kind === "codex" ? "ChatGPT" : "Cursor"} ${maskToken(account.token)}`;
}
function currentAccount() {
  return getAccounts().find((a) => a.id === selection) || null;
}
function selectionKey() {
  if (selection === "local") {
    return `local:${el("#usage-codex-days").value}:${el("#usage-codex-home").value.trim()}`;
  }
  return `${selection}:${rangeDays()}`;
}
/**
 * 标记结果区已渲染。at 为数据的实际获取时间（缓存数据传缓存时间），
 * renderedAt 采用数据时间，这样过期缓存渲染后下次进入页面仍会自动重新拉取。
 */
function markUpdated(at) {
  renderedFor = selectionKey();
  renderedAt = Number.isFinite(at) && at > 0 ? at : Date.now();
  const stale = Date.now() - renderedAt >= effectiveTtlMs();
  const time = new Date(renderedAt).toLocaleTimeString("zh-CN", { hour12: false });
  el("#usage-updated").textContent = `数据更新于 ${time}${stale ? "（缓存）" : ""}`;
}

/** 结果缓存有效期：开启自动更新后与更新间隔保持一致。 */
function effectiveTtlMs() {
  return usageInterval > 0 ? usageInterval * 60_000 : DEFAULT_TTL_MS;
}

function rebuildUsageTimer() {
  if (usageTimerId != null) {
    clearInterval(usageTimerId);
    usageTimerId = null;
  }
  if (usageInterval > 0) {
    usageTimerId = setInterval(() => {
      // 页面可见时强制重新统计当前视图；不可见时仅作废缓存，下次打开自动重拉
      if (panelVisible && !loading) {
        void loadCurrent(true);
      } else {
        clearUsageMemoryCache();
        renderedAt = 0;
      }
    }, usageInterval * 60_000);
  }
}

/** 与持久化设置同步（初次加载 / 其他入口修改时）。 */
function syncUsageInterval(minutes) {
  const n = Number(minutes);
  if (!Number.isFinite(n) || n === usageInterval) return;
  usageInterval = n;
  el("#usage-interval").value = String(n);
  setUsageCacheTtlMs(usageInterval > 0 ? usageInterval * 60_000 : DEFAULT_TTL_MS);
  rebuildUsageTimer();
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
 * opts.dailySources 为按日堆叠柱的各来源（总览按账户分段；缺省则用 agg.daily 单列）。
 */
function renderAggregate(agg, { showActual, metaText, plan, dailySources }) {
  const sources =
    dailySources != null
      ? dailySources
      : [{ label: "用量", daily: agg.daily || [], showActual: !!showActual }];
  lastAggRender = { agg, opts: { showActual, metaText, plan, dailySources: sources } };
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

function pad2(n) {
  return String(n).padStart(2, "0");
}
function localYmd(d) {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
function addLocalDays(d, n) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
}
function selectedRangeDays() {
  if (selection === "local") {
    const n = Number(el("#usage-codex-days").value);
    return Number.isFinite(n) && n > 0 ? n : 0;
  }
  return rangeDays();
}
function dailyAxisLabels(sources) {
  const days = selectedRangeDays();
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
  let cur = (() => {
    const [y, m, d] = min.split("-").map(Number);
    return new Date(y, m - 1, d);
  })();
  while (cur.getTime() <= today0.getTime() && labels.length < 3660) {
    labels.push(localYmd(cur));
    cur = addLocalDays(cur, 1);
  }
  return labels;
}
function tickDate(ymd) {
  const p = String(ymd).split("-");
  if (p.length !== 3) return ymd;
  return `${Number(p[1])}/${Number(p[2])}`;
}
function titleDate(ymd) {
  const p = String(ymd).split("-");
  if (p.length !== 3) return ymd;
  return `${p[0]}年${Number(p[1])}月${Number(p[2])}日`;
}

function updateDailyHover(chart, hit) {
  setChartHoverHit(chart, hit, { stroke: chartTheme().tickStrong, hitBorder: 2 });
}

function collectDailySources() {
  const sources = [];
  for (const a of getAccounts().filter((x) => x.kind === "cursor")) {
    const r = overviewResults.get(a.id);
    if (!r || !r.agg) continue;
    sources.push({
      label: a.note || maskToken(a.token),
      daily: r.agg.daily || [],
      showActual: true,
    });
  }
  const local = overviewResults.get("local");
  const scanAgg = local && local.scan ? local.scan.aggregate : null;
  if (scanAgg) {
    sources.push({
      label: "本地用量分析",
      daily: scanAgg.daily || [],
      showActual: false,
    });
  }
  return sources;
}

function renderDailyChart(sources) {
  const t = chartTheme();
  const list = Array.isArray(sources) ? sources : [];
  const labels = dailyAxisLabels(list);
  const baseColors = list.map((_, i) => colorFor(i));
  const meta = [];
  const showActual = [];
  const datasets = list.map((s, i) => {
    const byDate = new Map((s.daily || []).map((d) => [d.date, d]));
    meta.push(labels.map((ymd) => byDate.get(ymd) || null));
    showActual.push(!!s.showActual);
    return {
      label: s.label,
      data: labels.map((ymd) => {
        const d = byDate.get(ymd);
        return d && d.tokens > 0 ? d.tokens : null;
      }),
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
    options: {
      responsive: true,
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
              return titleDate(items[0].chart.data.labels[items[0].dataIndex]);
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
              return `当日合计 ${fmtTokens(sum)}`;
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

// 图表实例常驻，重复渲染时原地更新数据（渐进合并时不闪烁）
function renderCharts(agg, dailySources) {
  renderDailyChart(dailySources);
  const t = chartTheme();
  const top = agg.models.filter((m) => m.equivalentUsd > 0).slice(0, 12);
  const labels = top.map((m) => m.model);
  const data = top.map((m) => Number(m.equivalentUsd.toFixed(4)));
  const colors = top.map((_, i) => colorFor(i));

  if (barChart) {
    barChart.data.labels = labels;
    barChart.data.datasets[0].data = data;
    barChart.data.datasets[0].backgroundColor = colors;
    barChart.update();
  } else {
    barChart = new Chart(el("#chart-bar"), {
      type: "bar",
      data: { labels, datasets: [{ label: "等价费用 (USD)", data, backgroundColor: colors, borderRadius: 4 }] },
      options: {
        indexAxis: "y",
        responsive: true,
        plugins: { legend: { display: false } },
        scales: {
          x: { ticks: { color: t.tick }, grid: { color: t.grid } },
          y: { ticks: { color: t.tickStrong }, grid: { display: false } },
        },
      },
    });
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
      options: {
        responsive: true,
        plugins: {
          legend: { position: "bottom", labels: { color: t.tickStrong } },
          tooltip: { callbacks: { label: (ctx) => `${ctx.label}: ${fmtTokens(ctx.parsed)}` } },
        },
      },
    });
  }
}

/* ---------- 数据源选择（chips） ---------- */

function rebuildChips() {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  if (selection !== "all" && selection !== "local" && !cursorAccounts.some((a) => a.id === selection)) {
    selection = "all";
  }
  const chips = [{ value: "all", label: "全部总览", kind: null }];
  for (const a of cursorAccounts) chips.push({ value: a.id, label: accountLabel(a), kind: a.kind });
  chips.push({ value: "local", label: "本地用量分析", kind: "codex" });

  el("#usage-sources").replaceChildren(
    ...chips.map((c) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `chip-select${selection === c.value ? " active" : ""}`;
      if (c.kind) {
        const tag = document.createElement("span");
        tag.className = `tag kind-${c.kind === "codex" ? "codex" : "cursor"}`;
        tag.textContent = c.kind === "codex" ? "ChatGPT" : "Cursor";
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
  el("#usage-range-wrap").hidden = selection === "local";
  el("#usage-local-form").hidden = selection !== "local";
  el("#usage-overview").hidden = selection !== "all";
  el("#usage-results").hidden = true;
  renderedFor = ""; // 结果区已被隐藏，需要重新渲染
  clearStatus();
}

function selectSource(value) {
  if (selection === value) {
    void loadCurrent(false);
    return;
  }
  selection = value;
  rebuildChips();
  applyVisibility();
  void loadCurrent(false);
}

/* ---------- 数据加载与持久化缓存（与托盘总览共用 usage_data.js） ---------- */

function rangeKey() {
  return String(rangeDays());
}

function scanRangeKey(days) {
  return days == null || days === 0 ? "all" : String(days);
}

function getCachedAgg(account) {
  return cacheGetAgg(account.id, rangeKey());
}

function getCachedScan(days, home) {
  return cacheGetScan(scanRangeKey(days), home);
}

function fetchAggregate(account, force) {
  const { start, end } = rangeBounds();
  return fetchCursorAggregate(account, rangeKey(), { start, end, force });
}

function fetchScan(days, home, force) {
  return fetchCodexScan({ days: days == null || days === 0 ? null : days, home, force });
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
  const accounts = getAccounts().filter((a) => a.kind === "cursor");
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

  // 一行一个来源：各 Cursor 账户 + 本地 Codex 分析；showActual=false 的来源实扣列恒为 —。
  // 统计失败但有缓存数据的来源仍显示旧数字（状态列展示错误）。
  const sourceRow = ({ kind, label, stateText, stateBad, agg, showActual, planText, ratioText }) => {
    const tr = document.createElement("tr");
    const nameTd = document.createElement("td");
    const ident = document.createElement("div");
    ident.className = "account-ident";
    const tag = document.createElement("span");
    tag.className = `tag kind-${kind}`;
    tag.textContent = kind === "codex" ? "ChatGPT" : "Cursor";
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
        planTd,
        numTd(fmtTokens(agg.totalTokens), fmtInt(agg.totalTokens)),
        numTd(showActual ? fmtUsd(agg.totalActualUsd) : "—"),
        numTd(fmtUsd(agg.totalEquivalentUsd)),
        numTd(ratioText || "—")
      );
    } else {
      tr.append(nameTd, stateTd, planTd, numTd("—"), numTd("—"), numTd("—"), numTd("—"));
    }
    body.append(tr);
  };

  for (const a of accounts) {
    const result = overviewResults.get(a.id);
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
    // 套餐列：套餐名 + 月费；倍数列：等价费用 ÷ 月费（月费未知或为 0 时为 —）
    const membership = a.status ? a.status.membershipType : null;
    const planLabel = membership ? membershipLabel(membership) : "";
    const price = planMonthlyUsd(membership);
    const planText = planLabel ? (price != null ? `${planLabel} · $${price}` : planLabel) : "—";
    let ratioText = "—";
    if (agg && price > 0) ratioText = `${(agg.totalEquivalentUsd / price).toFixed(1)}×`;
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
      agg,
      showActual: true,
      planText,
      ratioText,
    });
  }

  // 本地 Codex 分析行：无论有无 Cursor 账户都展示；本地日志无账户归属，无套餐可比
  const local = overviewResults.get("local");
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
    kind: "codex",
    label: "本地用量分析",
    stateText: localState,
    stateBad: localBad,
    agg: localAgg,
    showActual: false,
    planText: "—",
    ratioText: "—",
  });

  if (hasData) {
    const tr = document.createElement("tr");
    tr.className = "total-row";
    const label = document.createElement("td");
    label.textContent = "合计";
    const spacer = document.createElement("td");
    const planTd = document.createElement("td");
    planTd.textContent = totals.planKnown ? `$${totals.planUsd}/月` : "—";
    // 合计倍数只按「套餐月费已知的 Cursor 账户」口径计算，与套餐列保持一致
    const totalRatio = totals.planUsd > 0 ? `${(totals.planEquiv / totals.planUsd).toFixed(1)}×` : "—";
    tr.append(
      label,
      spacer,
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
  const local = overviewResults.get("local");
  const scanAgg = local && local.scan ? local.scan.aggregate : null;
  if (scanAgg) {
    aggs.push(scanAgg);
    if (Number.isFinite(local.at)) ats.push(local.at);
  }
  if (!aggs.length) return null;

  const merged = mergeAggregates(aggs);
  const sources = [];
  if (cursorCount) sources.push(`${cursorCount} 个 Cursor 账户`);
  if (scanAgg) sources.push("本地 ChatGPT");
  renderAggregate(merged, {
    showActual: true,
    plan: planKnown ? { monthlyUsd: planUsd, equivalentUsd: planEquiv } : null,
    metaText: `全部总览 · ${rangeText()} · ${sources.join(" + ")} 合并 · 等价费用按官方 API 价折算，实扣为 Cursor 实际计费；倍数 = 等价费用 ÷ 套餐月费（仅计入 Cursor 账户，选近 30 天时最具参考性）。`,
    dailySources: collectDailySources(),
  });
  // 合并视图的数据时间取最早的来源时间（保守口径）
  markUpdated(ats.length ? Math.min(...ats) : Date.now());
  return merged;
}

async function loadOverview(force, seq) {
  const cursorAccounts = getAccounts().filter((a) => a.kind === "cursor");
  el("#usage-overview").hidden = false;
  const previous = new Map(overviewResults);
  overviewResults.clear();

  // 时间范围沿用顶部选择器，目录用默认 CODEX_HOME
  const days = rangeDays() > 0 ? rangeDays() : null;

  // 1) 缓存种子：先用缓存（含过期缓存）填充各来源并立即渲染卡片 / 图表 / 明细
  for (const a of cursorAccounts) {
    const cached = getCachedAgg(a);
    const prev = previous.get(a.id);
    const agg = (cached && cached.agg) || (prev && prev.agg) || null;
    const at = cached ? cached.at : prev && prev.at;
    overviewResults.set(a.id, agg ? { state: "pending", agg, at } : { state: "pending" });
  }
  const cachedScan = getCachedScan(days, null);
  const prevLocal = previous.get("local");
  const scan = (cachedScan && cachedScan.scan) || (prevLocal && prevLocal.scan) || null;
  const scanAt = cachedScan ? cachedScan.at : prevLocal && prevLocal.at;
  overviewResults.set("local", scan ? { state: "pending", scan, at: scanAt } : { state: "pending" });
  renderOverviewTable();
  const seeded = renderOverviewMerged() != null;
  setStatus("", seeded ? "已显示缓存数据，正在更新各来源用量…" : "正在统计各来源用量…（数据多时可能需要几十秒）");

  // 2) 本地 Codex 扫描与各 Cursor 账户并行拉取，每个来源完成后立即合并重绘
  const scanJob = fetchScan(days, null, force)
    .then((entry) => {
      overviewResults.set("local", { state: "ok", scan: entry.scan, at: entry.at });
    })
    .catch((error) => {
      const prev = overviewResults.get("local") || {};
      const fallback = error && error.usageCache;
      overviewResults.set("local", {
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
    });

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
    scanJob,
  ]);
  if (seq !== loadSeq) return;

  renderOverviewTable();
  const merged = renderOverviewMerged();
  const failed = cursorAccounts.filter((a) => {
    const r = overviewResults.get(a.id);
    return r && r.state === "error";
  }).length;
  const local = overviewResults.get("local");

  if (merged) {
    const warns = [];
    if (failed) warns.push(`${failed} 个 Cursor 账户统计失败`);
    if (local && local.state === "error") warns.push("本地 ChatGPT 扫描失败");
    if (warns.length) setStatus("warn", `${warns.join("；")}，明细见上表；失败来源如有上次数据则继续显示。`);
    else if (merged.models.length === 0) setStatus("warn", "该时间范围内没有用量记录。");
    else if (!cursorAccounts.length) setStatus("", "尚未添加 Cursor 账户，总览目前仅含本地 ChatGPT 用量。");
    else clearStatus();
  } else {
    setStatus("bad", "全部来源统计失败，请检查 Token 与本地 ChatGPT 目录。");
  }
}

async function loadCursorAccount(account, force, seq) {
  const renderIt = (agg, at) => {
    const price = planMonthlyUsd(account.status ? account.status.membershipType : null);
    renderAggregate(agg, {
      showActual: true,
      plan: price != null ? { monthlyUsd: price, equivalentUsd: agg.totalEquivalentUsd } : null,
      metaText: `${accountLabel(account)} · ${rangeText()} · 共 ${agg.models.length} 个模型 · 等价费用按官方 API 价折算，实扣为 Cursor 实际计费；倍数 = 等价费用 ÷ 套餐月费。`,
      dailySources: [{ label: accountLabel(account), daily: agg.daily || [], showActual: true }],
    });
    markUpdated(at);
    if (agg.models.length === 0) setStatus("warn", "该时间范围内没有用量记录。");
  };

  // 先渲染缓存（含过期缓存），再后台拉取最新数据
  const cached = getCachedAgg(account);
  if (cached) renderIt(cached.agg, cached.at);
  const fresh = cached && Date.now() - cached.at < effectiveTtlMs();
  if (!fresh || force) {
    setStatus("", cached ? "已显示缓存数据，正在更新…" : "正在拉取该账户的用量…（数据多时可能需要几十秒）");
  }
  try {
    const entry = await fetchAggregate(account, force);
    if (seq !== loadSeq) return;
    clearStatus();
    renderIt(entry.agg, entry.at);
  } catch (error) {
    if (seq !== loadSeq) return;
    const fallback = (error && error.usageCache) || cached;
    if (fallback && fallback.agg) renderIt(fallback.agg, fallback.at);
    setStatus("bad", fallback ? `更新失败（仍显示上次数据）：${resetError(error)}` : `统计失败：${resetError(error)}`);
  }
}

function renderScan(scan, days, at) {
  const rootsText = scan.roots.length ? scan.roots.join("、") : "未找到会话目录";
  renderAggregate(scan.aggregate, {
    showActual: false,
    plan: null,
    metaText: `本地 ChatGPT 用量分析 · ${days ? `近 ${days} 天` : "全部"} · 扫描 ${scan.filesScanned} 个文件 / ${scan.sessions} 个会话 · 目录：${rootsText}`,
    dailySources: [{ label: "本地用量分析", daily: scan.aggregate.daily || [], showActual: false }],
  });
  markUpdated(at);
  if (scan.aggregate.models.length === 0) {
    setStatus("warn", `未在本地会话日志中找到用量。目录：${rootsText}`);
  }
}

async function loadLocal(force, seq) {
  const daysRaw = Number(el("#usage-codex-days").value);
  const days = Number.isFinite(daysRaw) && daysRaw > 0 ? daysRaw : null;
  const home = el("#usage-codex-home").value.trim() || null;
  // 先渲染缓存（含过期缓存），再后台重新扫描
  const cached = getCachedScan(days, home);
  if (cached) renderScan(cached.scan, days, cached.at);
  const fresh = cached && Date.now() - cached.at < effectiveTtlMs();
  if (!fresh || force) {
    setStatus("", cached ? "已显示缓存数据，正在重新扫描…" : "正在扫描本地 ChatGPT 会话日志…");
  }
  const btn = el("#usage-codex-scan");
  btn.disabled = true;
  try {
    const entry = await fetchScan(days, home, force);
    if (seq !== loadSeq) return;
    clearStatus();
    renderScan(entry.scan, days, entry.at);
  } catch (error) {
    if (seq !== loadSeq) return;
    const fallback = (error && error.usageCache) || cached;
    if (fallback && fallback.scan) renderScan(fallback.scan, days, fallback.at);
    setStatus("bad", fallback ? `扫描失败（仍显示上次数据）：${resetError(error)}` : `扫描失败：${resetError(error)}`);
  } finally {
    btn.disabled = false;
  }
}

async function loadCurrent(force) {
  // 先作废所有在途加载，避免旧结果渲染到已切换的视图上
  const seq = ++loadSeq;
  // 结果区仍展示着当前选择且未过期时无需重载
  if (!force && selectionKey() === renderedFor && Date.now() - renderedAt < effectiveTtlMs()) return;
  loading = true;
  try {
    if (selection === "all") {
      await loadOverview(force, seq);
    } else if (selection === "local") {
      await loadLocal(force, seq);
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
  } finally {
    if (seq === loadSeq) loading = false;
  }
}

/* ---------- 初始化 ---------- */

export function initUsage() {
  el("#usage-refresh").addEventListener("click", () => {
    void loadCurrent(true);
  });
  el("#usage-range").addEventListener("change", () => {
    void loadCurrent(false);
  });
  el("#usage-codex-scan").addEventListener("click", () => {
    if (selection === "local") void loadCurrent(true);
  });
  el("#usage-interval").addEventListener("change", async () => {
    const select = el("#usage-interval");
    const minutes = Number(select.value);
    const previous = usageInterval;
    try {
      const saved = await setUsageInterval(minutes);
      usageInterval = Number.isFinite(Number(saved)) ? Number(saved) : minutes;
      select.value = String(usageInterval);
      rebuildUsageTimer();
      setStatus("ok", usageInterval > 0 ? `统计将每 ${usageInterval} 分钟自动更新。` : "已关闭统计自动更新。");
    } catch (error) {
      select.value = String(previous);
      setStatus("bad", `设置自动更新失败：${resetError(error)}`);
    }
  });

  onAccountsChanged((list) => {
    syncUsageInterval(getUsageIntervalMinutes());
    const sig = list.map((a) => a.id).join(",");
    if (sig !== accountIdsSig) {
      accountIdsSig = sig;
      renderedAt = 0; // 账户增删后结果区视为过期
      // 清理已删除账户的聚合缓存（内存 + localStorage）
      purgeMissingAccounts(list);
    }
    for (const a of list) {
      // 刚更换过凭据（status 与刷新时间都被清空）的账户旧缓存作废
      if (!a.status && !a.lastRefreshAt) purgeAccountCache(a.id);
    }
    const stillExists =
      selection === "all" || selection === "local" || list.some((a) => a.id === selection && a.kind === "cursor");
    rebuildChips();
    if (!stillExists) {
      applyVisibility();
      if (panelVisible) void loadCurrent(false);
      return;
    }
    // 账户状态更新（如定时刷新）时，同步总览表中的额度 / 套餐信息
    if (panelVisible && selection === "all" && !loading) renderOverviewTable();
  });

  window.addEventListener("panelshown", (event) => {
    const shown = !!(event.detail && event.detail.id === "usage-panel");
    panelVisible = shown;
    if (shown && !loading) void loadCurrent(false);
  });

  // 数字单位切换后，用现有数据即时重绘结果区与总览表
  window.addEventListener("unitchange", () => {
    if (lastAggRender && !el("#usage-results").hidden) {
      renderAggregate(lastAggRender.agg, lastAggRender.opts);
    }
    if (!el("#usage-overview").hidden) renderOverviewTable();
  });

  rebuildChips();
  applyVisibility();
  setUsageCacheTtlMs(getUsageIntervalMinutes() > 0 ? getUsageIntervalMinutes() * 60_000 : DEFAULT_TTL_MS);
}
