import {
  invoke,
  fmtInt,
  fmtTokens,
  fmtUsd,
  fmtDateMs,
  compactTokens,
  colorFor,
  resetError,
  pieSliceLabelsPlugin,
  tickDate,
  topSlices,
} from "./shared.js";
import {
  fetchCursorAggregate,
  fetchArchivedAggregate,
  getCachedAgg,
  localYmd,
  parseYmd,
  addLocalDays,
} from "./usage_data.js";
import { membershipLabel, planMonthlyUsd, cursorIdentity } from "./account_format.js";

// Cursor 账户全量用量快照：把「全部」跨度的聚合结果渲染成一张 PNG 图片保存到本地。
//
// 数据获取回退链（acquireData）：
// - 账户有效：在线同步事件库后切片 → 本地事件库只读切片 → localStorage 缓存；
// - 账户已失效（status.alive === false）：本地事件库只读切片 → localStorage 缓存 → 在线兜底
//   （状态可能过期，token 或许仍有效，最后一搏）。
// 绘制在离屏 canvas 上进行：卡片 / 明细表手绘，图表用 Chart.js 渲到独立 canvas 后
// drawImage 合成；配色读当前主题的 CSS 变量，字体与页面一致。
// 保存走后端 usage_snapshot_save（系统保存对话框 + 写盘 + 审计）。

const SCALE = 2; // 输出位图 = 逻辑尺寸 × 2，保证文字清晰
const W = 1200;
const PAD = 36;
const INNER_W = W - PAD * 2;
const TABLE_MAX_ROWS = 40; // 模型行数上限，超出合并为「其他」

let appInfoCache = null;

/* ---------- 主题与字体 ---------- */

function cssVar(name, fallback) {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

function themeColors() {
  return {
    bg: cssVar("--bg", "#0a0e16"),
    surface: cssVar("--surface", "#121826"),
    surface2: cssVar("--surface-2", "#1a2334"),
    border: cssVar("--border", "#232d43"),
    text: cssVar("--text", "#e9edf5"),
    textStrong: cssVar("--text-strong", "#f7f9fc"),
    muted: cssVar("--muted", "#93a0b8"),
    accent: cssVar("--accent", "#7c9aff"),
    accentSoft: cssVar("--accent-soft", "rgba(124, 154, 255, 0.13)"),
    ok: cssVar("--ok", "#57d38c"),
    bad: cssVar("--bad", "#f28080"),
    warn: cssVar("--warn", "#f2b04e"),
    chartGrid: cssVar("--chart-grid", "rgba(147, 160, 184, 0.13)"),
    chartTick: cssVar("--chart-tick", "#93a0b8"),
    chartTickStrong: cssVar("--chart-tick-strong", "#e9edf5"),
    chartBorder: cssVar("--chart-border", "#0e1420"),
  };
}

function fontFamily() {
  try {
    return getComputedStyle(document.body).fontFamily || "sans-serif";
  } catch {
    return "sans-serif";
  }
}

/* ---------- 数据获取（在线 / 磁盘存档 / localStorage 回退） ---------- */

const SOURCE_LABELS = {
  online: "实时拉取",
  archive: "本地存档",
  cache: "本地缓存",
};

async function readArchive(account) {
  const stored = await fetchArchivedAggregate(account.id, { start: null, end: null }).catch(() => null);
  if (stored && stored.agg) return { ...stored, source: "archive" };
  return null;
}

function readLocalCache(account) {
  const cached = getCachedAgg(account.id, "0");
  if (cached && cached.agg) return { agg: cached.agg, at: cached.at, source: "cache" };
  return null;
}

async function fetchOnline(account) {
  const entry = await fetchCursorAggregate(account, "0", { start: null, end: null });
  return { agg: entry.agg, at: entry.at, source: "online" };
}

async function acquireData(account) {
  const dead = !!(account.status && account.status.alive === false);
  if (dead) {
    const local = (await readArchive(account)) || readLocalCache(account);
    if (local) return local;
    try {
      return await fetchOnline(account);
    } catch (error) {
      throw new Error(`账户已失效且没有本地存档，无法生成快照（${resetError(error)}）`);
    }
  }
  try {
    return await fetchOnline(account);
  } catch (error) {
    const fallback = (await readArchive(account)) || readLocalCache(account);
    if (fallback) return { ...fallback, fallbackError: resetError(error) };
    throw error;
  }
}

/* ---------- 文件名 ---------- */

function pad2(n) {
  return String(n).padStart(2, "0");
}

function sanitizeFileName(s) {
  return String(s || "")
    .replace(/[\\/:*?"<>|\x00-\x1f]/g, "_")
    .replace(/\s+/g, " ")
    .trim();
}

function defaultFileName(account) {
  const d = new Date();
  const stamp = `${d.getFullYear()}${pad2(d.getMonth() + 1)}${pad2(d.getDate())}-${pad2(d.getHours())}${pad2(d.getMinutes())}`;
  // 文件名用账户昵称（与用量页显示同一口径），未命名账户不带名字段
  const primary = cursorIdentity(account).primary;
  const name = sanitizeFileName(primary === "未命名账户" ? "" : primary).slice(0, 40);
  return `cursor-usage-${name ? `${name}-` : ""}${stamp}.png`;
}

/* ---------- 绘制小工具 ---------- */

function roundRectPath(ctx, x, y, w, h, r) {
  const rr = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + rr, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rr);
  ctx.arcTo(x, y, x + w, y, rr);
  ctx.closePath();
}

function makeText(ctx, font) {
  return (str, x, y, { size = 13, weight = 400, color = "#000", align = "left", baseline = "alphabetic" } = {}) => {
    ctx.font = `${weight} ${size}px ${font}`;
    ctx.fillStyle = color;
    ctx.textAlign = align;
    ctx.textBaseline = baseline;
    ctx.fillText(str, x, y);
  };
}

/** 按当前 ctx.font 截断超宽文本，尾部加省略号。 */
function ellipsize(ctx, str, maxWidth) {
  let s = String(str ?? "");
  if (ctx.measureText(s).width <= maxWidth) return s;
  while (s.length > 1 && ctx.measureText(`${s}…`).width > maxWidth) s = s.slice(0, -1);
  return `${s}…`;
}

function surfaceBox(ctx, t, x, y, w, h) {
  roundRectPath(ctx, x, y, w, h, 12);
  ctx.fillStyle = t.surface;
  ctx.fill();
  ctx.strokeStyle = t.border;
  ctx.lineWidth = 1;
  ctx.stroke();
}

/* ---------- 图表（Chart.js 离屏渲染） ---------- */

function renderChart(config) {
  const canvas = document.createElement("canvas");
  canvas.width = config.$w;
  canvas.height = config.$h;
  const chart = new Chart(canvas, {
    type: config.type,
    data: config.data,
    plugins: config.plugins || [],
    options: {
      responsive: false,
      devicePixelRatio: SCALE,
      animation: false,
      events: [],
      ...config.options,
    },
  });
  if (config.$pieSliceLabels) chart.$pieSliceLabels = config.$pieSliceLabels;
  // 挂实例属性后重绘一次，让扇区标注插件生效（配置不能进 options，见 shared.js 注释）
  chart.draw();
  return chart;
}

/** 按日横轴：最早数据日 → 数据时间所在日，从左到右由旧到新（与用量页一致）。 */
function dailyAxisLabels(daily, endMs) {
  const endDate = Number.isFinite(endMs) && endMs > 0 ? new Date(endMs) : new Date();
  const end0 = new Date(endDate.getFullYear(), endDate.getMonth(), endDate.getDate());
  let min = null;
  for (const d of daily || []) {
    if (d && d.date && (!min || d.date < min)) min = d.date;
  }
  if (!min) return [localYmd(end0)];
  const labels = [];
  let cur = parseYmd(min);
  while (cur.getTime() <= end0.getTime() && labels.length < 3660) {
    labels.push(localYmd(cur));
    cur = addLocalDays(cur, 1);
  }
  return labels;
}

function dailyChartConfig(agg, at, t, font, w, h) {
  const daily = agg.daily || [];
  const labels = dailyAxisLabels(daily, at);
  const byDate = new Map(daily.map((d) => [d.date, d]));
  return {
    $w: w,
    $h: h,
    type: "bar",
    data: {
      labels,
      datasets: [
        {
          data: labels.map((l) => {
            const row = byDate.get(l);
            return row && row.tokens > 0 ? row.tokens : null;
          }),
          backgroundColor: colorFor(0),
          borderWidth: 0,
          borderRadius: 2,
          maxBarThickness: 42,
        },
      ],
    },
    options: {
      plugins: { legend: { display: false }, tooltip: { enabled: false } },
      scales: {
        x: {
          ticks: {
            color: t.chartTick,
            maxRotation: 0,
            autoSkip: true,
            maxTicksLimit: 14,
            font: { family: font, size: 10 },
            callback(value) {
              return tickDate(this.getLabelForValue(value));
            },
          },
          grid: { display: false },
        },
        y: {
          beginAtZero: true,
          ticks: {
            color: t.chartTickStrong,
            font: { family: font, size: 10 },
            callback: (value) => compactTokens(value),
          },
          grid: { color: t.chartGrid },
        },
      },
    },
  };
}

function pieConfig(labels, data, colors, t, font, w, h, sliceFormatter) {
  return {
    $w: w,
    $h: h,
    type: "doughnut",
    plugins: [pieSliceLabelsPlugin],
    $pieSliceLabels: { formatter: sliceFormatter },
    data: {
      labels,
      datasets: [
        {
          data,
          backgroundColor: colors,
          borderColor: t.chartBorder,
          borderWidth: 2,
        },
      ],
    },
    options: {
      plugins: {
        legend: {
          position: "right",
          labels: { color: t.chartTickStrong, boxWidth: 10, boxHeight: 10, font: { family: font, size: 10 } },
        },
        tooltip: { enabled: false },
      },
    },
  };
}

/* ---------- 明细表数据 ---------- */

/** 模型行超过上限时合并尾部为「其他」，保持总量不变。 */
function tableRows(agg) {
  const models = agg.models || [];
  if (models.length <= TABLE_MAX_ROWS) return models;
  const head = models.slice(0, TABLE_MAX_ROWS - 1);
  const rest = models.slice(TABLE_MAX_ROWS - 1);
  const merged = {
    model: `其他（${rest.length} 个模型）`,
    priced: rest.some((m) => m.priced),
    pricedAs: null,
    events: rest.reduce((s, m) => s + (m.events || 0), 0),
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    totalTokens: 0,
    actualUsd: 0,
    equivalentUsd: 0,
    inputUsd: 0,
    outputUsd: 0,
    cacheReadUsd: 0,
    cacheWriteUsd: 0,
  };
  for (const m of rest) {
    merged.inputTokens += m.inputTokens;
    merged.outputTokens += m.outputTokens;
    merged.cacheReadTokens += m.cacheReadTokens;
    merged.cacheWriteTokens += m.cacheWriteTokens;
    merged.totalTokens += m.totalTokens;
    merged.actualUsd += m.actualUsd;
    merged.equivalentUsd += m.equivalentUsd;
    merged.inputUsd += m.inputUsd;
    merged.outputUsd += m.outputUsd;
    merged.cacheReadUsd += m.cacheReadUsd;
    merged.cacheWriteUsd += m.cacheWriteUsd;
  }
  return [...head, merged];
}

/* ---------- 主绘制 ---------- */

// 数值列固定宽；模型列宽在绘制时取「表宽 - 数值列合计」，保证整行恰好占满表宽不越界。
const TABLE_COLS = [
  { key: "model", label: "模型", w: 0, align: "left" },
  { key: "input", label: "输入", w: 118, align: "right" },
  { key: "cacheRead", label: "缓存读", w: 118, align: "right" },
  { key: "cacheWrite", label: "缓存写", w: 118, align: "right" },
  { key: "output", label: "输出", w: 118, align: "right" },
  { key: "total", label: "合计 Token", w: 118, align: "right" },
  { key: "actual", label: "实扣", w: 96, align: "right" },
  { key: "equivalent", label: "等价 API 价", w: 130, align: "right" },
];

function renderSnapshot(account, data, appInfo) {
  const t = themeColors();
  const font = fontFamily();
  const agg = data.agg;
  const status = account.status || null;
  const membership = status ? status.membershipType : null;
  const planLabel = membership ? membershipLabel(membership) : "";
  const price = planMonthlyUsd(membership);

  const rows = tableRows(agg);
  const hasModels = rows.length > 0;
  const events = (agg.models || []).reduce((s, m) => s + (m.events || 0), 0);

  // 卡片列表（套餐月费卡片仅在月费已知时出现）
  const cards = [
    { label: "总等价费用（官方 API 价）", value: fmtUsd(agg.totalEquivalentUsd), accent: true },
    { label: "实际支出（Cursor）", value: fmtUsd(agg.totalActualUsd) },
    {
      label: "总 Token",
      value: fmtTokens(agg.totalTokens),
      sub: fmtTokens(agg.totalTokens) !== fmtInt(agg.totalTokens) ? fmtInt(agg.totalTokens) : "",
    },
    { label: "未定价模型", value: `${agg.unpricedModels} 个`, sub: `${fmtTokens(agg.unpricedTokens)} tok` },
  ];
  if (price != null) {
    const ratio = price > 0 ? `${(agg.totalEquivalentUsd / price).toFixed(1)}×` : "—";
    cards.splice(1, 0, { label: "套餐月费 · 等价倍数", value: `$${price} · ${ratio}` });
  }

  // ---- 布局（先算总高，再建 canvas）----
  const headerH = 30 + 24 + 16 + 84 + 8;
  const cardH = 92;
  const cardGap = 14;
  const breakdownH = 34;
  const dailyChartH = 280;
  const dailyBoxH = 40 + dailyChartH + 14;
  const pieChartH = 220;
  const pieBoxH = 40 + pieChartH + 14;
  const rowH = 38;
  const headRowH = 30;
  const tableRowsCount = hasModels ? rows.length + 1 : 0; // +1 合计行
  const tableBodyH = hasModels ? headRowH + tableRowsCount * rowH : 36;
  const tableBoxH = 14 + 24 + 8 + tableBodyH + 14;
  const footerH = 30;
  const H =
    PAD + headerH + cardH + 16 + breakdownH + dailyBoxH + 16 + pieBoxH + 16 + tableBoxH + 14 + footerH + PAD - 16;

  const canvas = document.createElement("canvas");
  canvas.width = W * SCALE;
  canvas.height = Math.round(H * SCALE);
  const ctx = canvas.getContext("2d");
  ctx.scale(SCALE, SCALE);
  const text = makeText(ctx, font);

  // 背景
  ctx.fillStyle = t.bg;
  ctx.fillRect(0, 0, W, H);

  let y = PAD;

  // ---- 头部 ----
  text("Cursor 账户用量快照", PAD, y + 20, { size: 22, weight: 700, color: t.textStrong });
  text(`生成时间 ${fmtDateMs(Date.now())}`, W - PAD, y + 20, { size: 12, color: t.muted, align: "right" });
  y += 30;
  const appLine = `${appInfo && appInfo.version ? `my-ai-assistant v${appInfo.version} · ` : ""}时间范围：全部（全量数据）`;
  text(appLine, PAD, y + 12, { size: 12, color: t.muted });
  y += 24;
  ctx.strokeStyle = t.border;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(PAD, y + 0.5);
  ctx.lineTo(W - PAD, y + 0.5);
  ctx.stroke();
  y += 16;

  // ---- 账户信息（2 行 × 3 列）----
  const aliveText = status == null ? "未知" : status.alive === false ? "已失效" : "有效";
  const aliveColor = status == null ? t.muted : status.alive === false ? t.bad : t.ok;
  const sourceText =
    SOURCE_LABELS[data.source] + (data.fallbackError ? "（在线拉取失败，使用本地数据）" : "");
  const sourceColor = data.source === "online" ? t.text : t.warn;
  const planText = planLabel ? (price != null ? `${planLabel} · $${price}/月` : planLabel) : "—";
  const infoCells = [
    { label: "账户", value: cursorIdentity(account).primary },
    { label: "邮箱", value: (status && status.email) || "—" },
    { label: "套餐", value: planText },
    { label: "账户状态", value: aliveText, color: aliveColor },
    { label: "数据来源", value: sourceText, color: sourceColor },
    { label: "数据时间", value: fmtDateMs(data.at) },
  ];
  const colW = INNER_W / 3;
  for (let i = 0; i < infoCells.length; i += 1) {
    const cx = PAD + (i % 3) * colW;
    const cy = y + Math.floor(i / 3) * 42;
    const cell = infoCells[i];
    text(cell.label, cx, cy + 11, { size: 11, color: t.muted });
    ctx.font = `600 14px ${font}`;
    const shown = ellipsize(ctx, cell.value, colW - 24);
    text(shown, cx, cy + 31, { size: 14, weight: 600, color: cell.color || t.textStrong });
  }
  y += 84 + 8;

  // ---- 汇总卡片 ----
  const cardW = (INNER_W - cardGap * (cards.length - 1)) / cards.length;
  for (let i = 0; i < cards.length; i += 1) {
    const cx = PAD + i * (cardW + cardGap);
    const card = cards[i];
    surfaceBox(ctx, t, cx, y, cardW, cardH);
    if (card.accent) {
      roundRectPath(ctx, cx, y, cardW, cardH, 12);
      ctx.strokeStyle = t.accent;
      ctx.lineWidth = 1.5;
      ctx.stroke();
    }
    ctx.font = `400 11.5px ${font}`;
    text(ellipsize(ctx, card.label, cardW - 28), cx + 14, y + 24, { size: 11.5, color: t.muted });
    ctx.font = `700 21px ${font}`;
    text(ellipsize(ctx, card.value, cardW - 28), cx + 14, y + 56, {
      size: 21,
      weight: 700,
      color: card.accent ? t.accent : t.textStrong,
    });
    if (card.sub) {
      ctx.font = `400 10.5px ${font}`;
      text(ellipsize(ctx, card.sub, cardW - 28), cx + 14, y + 76, { size: 10.5, color: t.muted });
    }
  }
  y += cardH + 16;

  // ---- 等价费用构成 ----
  {
    let bx = PAD;
    text("等价费用构成", bx, y + 16, { size: 12, color: t.muted });
    bx += ctx.measureText("等价费用构成").width + 20;
    const parts = [
      ["输入", agg.totalInputUsd],
      ["缓存读", agg.totalCacheReadUsd],
      ["缓存写", agg.totalCacheWriteUsd],
      ["输出", agg.totalOutputUsd],
    ];
    for (const [label, usd] of parts) {
      text(label, bx, y + 16, { size: 11.5, color: t.muted });
      bx += ctx.measureText(label).width + 6;
      text(fmtUsd(usd || 0), bx, y + 16, { size: 12.5, weight: 600, color: t.text });
      bx += ctx.measureText(fmtUsd(usd || 0)).width + 22;
    }
  }
  y += breakdownH;

  // ---- 按日 Token 柱图 ----
  surfaceBox(ctx, t, PAD, y, INNER_W, dailyBoxH);
  text("按日 Token（全部）", PAD + 16, y + 26, { size: 14, weight: 600, color: t.textStrong });
  {
    const chart = renderChart(dailyChartConfig(agg, data.at, t, font, INNER_W - 32, dailyChartH));
    ctx.drawImage(chart.canvas, PAD + 16, y + 40, INNER_W - 32, dailyChartH);
    chart.destroy();
  }
  y += dailyBoxH + 16;

  // ---- 三张饼图 ----
  {
    const boxW = (INNER_W - 28) / 3;
    const chartW = boxW - 32;
    const tokenSlices = topSlices(agg.models, "totalTokens", 8);
    const costSlices = topSlices(agg.models, "equivalentUsd", 8);
    const sums = (agg.models || []).reduce(
      (acc, m) => {
        acc.input += m.inputTokens;
        acc.cacheRead += m.cacheReadTokens;
        acc.cacheWrite += m.cacheWriteTokens;
        acc.output += m.outputTokens;
        return acc;
      },
      { input: 0, cacheRead: 0, cacheWrite: 0, output: 0 }
    );
    const pies = [
      {
        title: "各模型 Token 数量",
        config: pieConfig(
          tokenSlices.map((s) => s.label),
          tokenSlices.map((s) => Math.round(s.value)),
          tokenSlices.map((_, i) => colorFor(i)),
          t,
          font,
          chartW,
          pieChartH,
          (value, share) => [share, compactTokens(value)]
        ),
      },
      {
        title: "各模型等价费用（USD）",
        config: pieConfig(
          costSlices.map((s) => s.label),
          costSlices.map((s) => Number(s.value.toFixed(4))),
          costSlices.map((_, i) => colorFor(i)),
          t,
          font,
          chartW,
          pieChartH,
          (value, share) => [share, fmtUsd(value)]
        ),
      },
      {
        title: "Token 构成",
        config: pieConfig(
          ["输入", "缓存读", "缓存写", "输出"],
          [sums.input, sums.cacheRead, sums.cacheWrite, sums.output],
          [colorFor(0), colorFor(2), colorFor(4), colorFor(1)],
          t,
          font,
          chartW,
          pieChartH,
          (value, share) => [share, compactTokens(value)]
        ),
      },
    ];
    for (let i = 0; i < pies.length; i += 1) {
      const bx = PAD + i * (boxW + 14);
      surfaceBox(ctx, t, bx, y, boxW, pieBoxH);
      text(pies[i].title, bx + 16, y + 26, { size: 14, weight: 600, color: t.textStrong });
      const chart = renderChart(pies[i].config);
      ctx.drawImage(chart.canvas, bx + 16, y + 40, chartW, pieChartH);
      chart.destroy();
    }
  }
  y += pieBoxH + 16;

  // ---- 按模型明细表 ----
  surfaceBox(ctx, t, PAD, y, INNER_W, tableBoxH);
  text("按模型明细", PAD + 16, y + 28, { size: 14, weight: 600, color: t.textStrong });
  {
    const tableX = PAD + 16;
    const tableW = INNER_W - 32;
    // 模型列吃掉数值列之外的全部剩余宽度（TABLE_COLS[0].w 为占位 0）
    const numericW = TABLE_COLS.slice(1).reduce((sum, c) => sum + c.w, 0);
    const cols = [{ ...TABLE_COLS[0], w: tableW - numericW }, ...TABLE_COLS.slice(1)];
    let ty = y + 14 + 24 + 8;
    if (!hasModels) {
      text("该账户没有可统计的用量记录。", tableX, ty + 22, { size: 12.5, color: t.muted });
    } else {
      // 表头
      ctx.fillStyle = t.surface2;
      ctx.fillRect(tableX, ty, tableW, headRowH);
      let cx = tableX;
      for (const col of cols) {
        const tx = col.align === "right" ? cx + col.w - 12 : cx + 12;
        text(col.label, tx, ty + headRowH / 2, {
          size: 11.5,
          weight: 600,
          color: t.muted,
          align: col.align,
          baseline: "middle",
        });
        cx += col.w;
      }
      ty += headRowH;

      // 数值单元格：上行 token 数，下行等价费用（未定价或无量时单行居中）
      const numCell = (x, w, yMid, tokens, usd, showCost) => {
        const right = x + w - 12;
        if (!(tokens > 0)) {
          text("—", right, yMid, { size: 12, color: t.muted, align: "right", baseline: "middle" });
          return;
        }
        if (showCost) {
          text(fmtTokens(tokens), right, yMid - 7, { size: 12.5, color: t.text, align: "right", baseline: "middle" });
          text(fmtUsd(usd), right, yMid + 9, { size: 10, color: t.muted, align: "right", baseline: "middle" });
        } else {
          text(fmtTokens(tokens), right, yMid, { size: 12.5, color: t.text, align: "right", baseline: "middle" });
        }
      };

      for (let i = 0; i < rows.length; i += 1) {
        const m = rows[i];
        const rowY = ty + i * rowH;
        const yMid = rowY + rowH / 2;
        if (i % 2 === 1) {
          ctx.fillStyle = t.surface2;
          ctx.globalAlpha = 0.45;
          ctx.fillRect(tableX, rowY, tableW, rowH);
          ctx.globalAlpha = 1;
        }
        let x = tableX;
        // 模型名 + 定价标签
        ctx.font = `600 12.5px ${font}`;
        const name = ellipsize(ctx, m.model, cols[0].w - 24);
        text(name, x + 12, yMid - 7, { size: 12.5, weight: 600, color: t.text, baseline: "middle" });
        const tag = m.priced ? m.pricedAs || "" : "未定价";
        if (tag) {
          ctx.font = `400 10px ${font}`;
          text(ellipsize(ctx, tag, cols[0].w - 24), x + 12, yMid + 9, {
            size: 10,
            color: m.priced ? t.muted : t.warn,
            baseline: "middle",
          });
        }
        x += cols[0].w;
        numCell(x, cols[1].w, yMid, m.inputTokens, m.inputUsd, m.priced);
        x += cols[1].w;
        numCell(x, cols[2].w, yMid, m.cacheReadTokens, m.cacheReadUsd, m.priced);
        x += cols[2].w;
        numCell(x, cols[3].w, yMid, m.cacheWriteTokens, m.cacheWriteUsd, m.priced);
        x += cols[3].w;
        numCell(x, cols[4].w, yMid, m.outputTokens, m.outputUsd, m.priced);
        x += cols[4].w;
        text(fmtTokens(m.totalTokens), x + cols[5].w - 12, yMid, {
          size: 12.5,
          color: t.text,
          align: "right",
          baseline: "middle",
        });
        x += cols[5].w;
        text(fmtUsd(m.actualUsd), x + cols[6].w - 12, yMid, {
          size: 12.5,
          color: t.text,
          align: "right",
          baseline: "middle",
        });
        x += cols[6].w;
        text(m.priced ? fmtUsd(m.equivalentUsd) : "—", x + cols[7].w - 12, yMid, {
          size: 12.5,
          weight: 600,
          color: m.priced ? t.text : t.muted,
          align: "right",
          baseline: "middle",
        });
      }

      // 合计行
      const totalY = ty + rows.length * rowH;
      const yMid = totalY + rowH / 2;
      ctx.strokeStyle = t.border;
      ctx.beginPath();
      ctx.moveTo(tableX, totalY + 0.5);
      ctx.lineTo(tableX + tableW, totalY + 0.5);
      ctx.stroke();
      const sums = (agg.models || []).reduce(
        (acc, m) => {
          acc.input += m.inputTokens;
          acc.cacheRead += m.cacheReadTokens;
          acc.cacheWrite += m.cacheWriteTokens;
          acc.output += m.outputTokens;
          return acc;
        },
        { input: 0, cacheRead: 0, cacheWrite: 0, output: 0 }
      );
      let x = tableX;
      text("合计", x + 12, yMid, { size: 12.5, weight: 700, color: t.textStrong, baseline: "middle" });
      x += cols[0].w;
      const totalCell = (w, value) => {
        text(value, x + w - 12, yMid, {
          size: 12.5,
          weight: 700,
          color: t.textStrong,
          align: "right",
          baseline: "middle",
        });
        x += w;
      };
      totalCell(cols[1].w, fmtTokens(sums.input));
      totalCell(cols[2].w, fmtTokens(sums.cacheRead));
      totalCell(cols[3].w, fmtTokens(sums.cacheWrite));
      totalCell(cols[4].w, fmtTokens(sums.output));
      totalCell(cols[5].w, fmtTokens(agg.totalTokens));
      totalCell(cols[6].w, fmtUsd(agg.totalActualUsd));
      totalCell(cols[7].w, fmtUsd(agg.totalEquivalentUsd));
    }
  }
  y += tableBoxH + 14;

  // ---- 页脚 ----
  text(
    `等价费用按官方 API 价折算，实扣为 Cursor 实际计费 · 共 ${(agg.models || []).length} 个模型 / ${fmtInt(events)} 次调用 · 由 my-ai-assistant 生成`,
    W / 2,
    y + 14,
    { size: 11, color: t.muted, align: "center" }
  );

  return canvas;
}

/* ---------- 对外入口 ---------- */

/**
 * 生成并保存指定 Cursor 账户的全量用量快照。
 * 返回后端保存结果 { cancelled, path }；数据获取或渲染失败时抛错（由调用方提示）。
 */
export async function generateCursorSnapshot(account) {
  const data = await acquireData(account);
  if (!appInfoCache) {
    appInfoCache = await invoke("app_info").catch(() => null);
  }
  const canvas = renderSnapshot(account, data, appInfoCache);
  const dataUrl = canvas.toDataURL("image/png");
  return invoke("usage_snapshot_save", {
    fileName: defaultFileName(account),
    dataUrl,
  });
}
