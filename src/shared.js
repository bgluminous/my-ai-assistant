// 通用工具：invoke 封装、格式化、DOM 助手。

export function invoke(cmd, args) {
  const core = window.__TAURI__ && window.__TAURI__.core;
  if (!core || typeof core.invoke !== "function") {
    return Promise.reject(new Error("需要在应用内运行（Tauri 环境不可用）。"));
  }
  return core.invoke(cmd, args);
}

/** 监听后端事件，返回 Promise<UnlistenFn>；非 Tauri 环境 reject。 */
export function listen(event, handler) {
  const ev = window.__TAURI__ && window.__TAURI__.event;
  if (!ev || typeof ev.listen !== "function") {
    return Promise.reject(new Error("需要在应用内运行（Tauri 环境不可用）。"));
  }
  return ev.listen(event, handler);
}

/** 向所有 WebView 广播事件（主窗口 / 托盘互通）；非 Tauri 环境直接忽略。 */
export function emit(event, payload) {
  const ev = window.__TAURI__ && window.__TAURI__.event;
  if (!ev || typeof ev.emit !== "function") return Promise.resolve();
  return ev.emit(event, payload).catch(() => {});
}

export function el(selector, root = document) {
  const found = root.querySelector(selector);
  if (!found) throw new Error(`缺少页面元素：${selector}`);
  return found;
}

export function fmtInt(n) {
  if (n == null || !Number.isFinite(n)) return "—";
  return Math.round(n).toLocaleString("zh-CN");
}

// ---------- 数字单位偏好（Token 数量显示） ----------

const UNIT_KEY = "numberUnit"; // "full" | "en" | "zh"

export function getNumberUnit() {
  try {
    const v = localStorage.getItem(UNIT_KEY);
    return v === "en" || v === "zh" ? v : "full";
  } catch {
    return "full";
  }
}

/** 保存单位偏好并广播 unitchange，已渲染的视图据此即时重绘。 */
export function setNumberUnit(unit) {
  const v = unit === "en" || unit === "zh" ? unit : "full";
  try { localStorage.setItem(UNIT_KEY, v); } catch { /* ignore */ }
  window.dispatchEvent(new CustomEvent("unitchange", { detail: { unit: v } }));
}

function scaledNumber(value, divisor, suffix) {
  const v = value / divisor;
  const decimals = Math.abs(v) >= 100 ? 0 : Math.abs(v) >= 10 ? 1 : 2;
  let text = v.toFixed(decimals);
  if (decimals > 0) text = text.replace(/\.?0+$/, "");
  return `${text}${suffix}`;
}

/** Token 数量按当前单位偏好显示：完整数字 / K·M·B / 万·亿。 */
export function fmtTokens(n) {
  if (n == null || !Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  const unit = getNumberUnit();
  if (unit === "en") {
    if (abs >= 1e9) return scaledNumber(n, 1e9, "B");
    if (abs >= 1e6) return scaledNumber(n, 1e6, "M");
    if (abs >= 1e3) return scaledNumber(n, 1e3, "K");
    return fmtInt(n);
  }
  if (unit === "zh") {
    if (abs >= 1e8) return scaledNumber(n, 1e8, "亿");
    if (abs >= 1e4) return scaledNumber(n, 1e4, "万");
    return fmtInt(n);
  }
  return fmtInt(n);
}

export function fmtUsd(n) {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n !== 0 && Math.abs(n) < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

export function fmtDateMs(ms) {
  if (ms == null || !Number.isFinite(ms) || ms <= 0) return "—";
  const value = ms < 1_000_000_000_000 ? ms * 1000 : ms;
  const d = new Date(value);
  return Number.isFinite(d.getTime()) ? d.toLocaleString("zh-CN", { hour12: false }) : "—";
}

export function resetError(err) {
  return err instanceof Error ? err.message : String(err);
}

// ---------- 全局提示（toast 与弹窗内状态条） ----------

// 页面级提示统一走右上角悬浮 toast，避免文档流内的状态条在显示 / 消失时挤动内容；
// 弹窗内的提示保留原位，由 fillStatus 填充（带关闭按钮）。
let toastBox = null;
const toastByKey = new Map(); // key -> toast 元素（同 key 原地更新，保持各模块「单条覆盖」语义）
const toastTimers = new Map(); // toast 元素 -> 自动消失计时器

function removeToast(node) {
  const timer = toastTimers.get(node);
  if (timer != null) clearTimeout(timer);
  toastTimers.delete(node);
  for (const [key, value] of toastByKey) {
    if (value === node) toastByKey.delete(key);
  }
  node.remove();
}

/** 提示条通用内容：文案 span + 右侧关闭按钮（toast 与弹窗内状态条共用结构）。 */
function statusChildren(text, onClose) {
  const span = document.createElement("span");
  span.className = "status-text";
  span.textContent = text;
  const close = document.createElement("button");
  close.type = "button";
  close.className = "status-close";
  close.setAttribute("aria-label", "关闭提示");
  close.textContent = "×";
  close.addEventListener("click", onClose);
  return [span, close];
}

/**
 * 右上角全局 toast。kind 取 "" | "ok" | "warn" | "bad"；opts.key 相同的调用原地更新
 * 内容与配色而非新增。ok / warn 约 5 秒后自动消失（原地更新时重置计时），
 * ""（进行中）与 bad（错误）常驻，直到被替换或手动关闭。
 */
export function toast(kind, text, opts = {}) {
  if (!toastBox) {
    toastBox = document.createElement("div");
    toastBox.className = "toast-container";
    document.body.append(toastBox);
  }
  const key = opts.key || "";
  let node = key ? toastByKey.get(key) : null;
  if (!node) {
    node = document.createElement("div");
    toastBox.append(node);
    if (key) toastByKey.set(key, node);
  }
  node.className = `status toast ${kind}`;
  node.replaceChildren(...statusChildren(text, () => removeToast(node)));
  const prev = toastTimers.get(node);
  if (prev != null) clearTimeout(prev);
  toastTimers.delete(node);
  if (kind === "ok" || kind === "warn") {
    toastTimers.set(node, setTimeout(() => removeToast(node), 5000));
  }
}

/** 移除指定 key 的 toast（对应页面级 clearStatus 语义），不存在时安全空操作。 */
export function dismissToast(key) {
  const node = toastByKey.get(key);
  if (node) removeToast(node);
}

/** 弹窗内状态条：填充文案与关闭按钮（点击后隐藏并清空）并显示。 */
export function fillStatus(box, kind, text) {
  box.className = `status ${kind}`;
  box.replaceChildren(
    ...statusChildren(text, () => {
      box.hidden = true;
      box.replaceChildren();
    })
  );
  box.hidden = false;
}

// 桌面应用加固：屏蔽 WebView 的浏览器式行为（刷新 / 查找 / 打印 / 保存 / 历史导航等），
// 避免误触 F5 等快捷键导致页面重载丢状态；主窗口与托盘面板共用。
export function setupDesktopGuards() {
  window.addEventListener("keydown", (event) => {
    const key = String(event.key || "").toLowerCase();
    const combo = event.ctrlKey || event.metaKey;
    const blocked =
      key === "f5" ||
      key === "f3" ||
      key === "f7" ||
      (combo && ["r", "f", "g", "p", "s", "u", "o", "j"].includes(key)) ||
      (event.altKey && (key === "arrowleft" || key === "arrowright"));
    if (blocked) event.preventDefault();
  });
  // 鼠标侧键（XButton1 / XButton2）会触发 WebView 历史前进 / 后退
  const swallowSideButtons = (event) => {
    if (event.button === 3 || event.button === 4) event.preventDefault();
  };
  window.addEventListener("mousedown", swallowSideButtons);
  window.addEventListener("mouseup", swallowSideButtons);
  // 系统右键菜单含「刷新 / 后退」等浏览器项：仅输入区域保留（便于复制粘贴）
  window.addEventListener("contextmenu", (event) => {
    const editable =
      event.target instanceof Element &&
      event.target.closest("input, textarea, [contenteditable]");
    if (!editable) event.preventDefault();
  });
}

export async function copyText(text) {
  if (navigator.clipboard && navigator.clipboard.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const temp = document.createElement("textarea");
  temp.value = text;
  temp.style.position = "fixed";
  temp.style.inset = "-9999px auto auto -9999px";
  document.body.append(temp);
  temp.select();
  const ok = document.execCommand("copy");
  temp.remove();
  if (!ok) throw new Error("copy_failed");
}

// 掐掉调色板，给每个模型分配稳定颜色。
const PALETTE = [
  "#6ea8fe", "#7bd88f", "#f7b955", "#e879a6", "#a78bfa",
  "#4dd0e1", "#f28b82", "#c3e88d", "#82aaff", "#ffcb6b",
  "#c792ea", "#89ddff", "#f78c6c", "#b2ccd6", "#ddd0ff",
];

export function colorFor(index) {
  return PALETTE[index % PALETTE.length];
}

/** Chart.js 动画时长：系统「减少动效」时为 0。 */
export function chartAnimMs(ms) {
  try {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return 0;
  } catch { /* ignore */ }
  return ms;
}

/** #rrggbb / #rgb -> rgba()。 */
export function hexAlpha(hex, alpha) {
  const h = String(hex || "").replace("#", "");
  if (h.length !== 6 && h.length !== 3) return `rgba(110, 168, 254, ${alpha})`;
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  const n = parseInt(full, 16);
  if (!Number.isFinite(n)) return `rgba(110, 168, 254, ${alpha})`;
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

/**
 * 高亮当前段、其余降透明度。chart.$baseColors[i] 为该 dataset 的底色：
 * 字符串 = 整列同色（堆叠柱按账户），数组 = 按数据点着色（饼图 / 横向柱）。
 * hit: { datasetIndex, index }；index < 0 表示高亮整个 dataset（图例）。
 */
export function applyChartHoverDim(chart, hit, opts = {}) {
  const dim = opts.dim ?? 0.35;
  const stroke = opts.stroke;
  const hitBorder = opts.hitBorder ?? 0;
  const colors = chart.$baseColors || [];
  for (let i = 0; i < chart.data.datasets.length; i += 1) {
    const ds = chart.data.datasets[i];
    const base = colors[i];
    const perPoint = Array.isArray(base);
    const bg = [];
    const bw = [];
    const bc = [];
    for (let j = 0; j < ds.data.length; j += 1) {
      const hex = perPoint ? base[j] || colorFor(j) : base || colorFor(i);
      let alpha = 1;
      let width = 0;
      if (hit) {
        if (hit.index < 0) alpha = i === hit.datasetIndex ? 1 : dim;
        else {
          const on = i === hit.datasetIndex && j === hit.index;
          alpha = on ? 1 : dim;
          width = on ? hitBorder : 0;
        }
      }
      bg.push(hexAlpha(hex, alpha));
      bw.push(width);
      bc.push(stroke || hex);
    }
    ds.backgroundColor = bg;
    ds.hoverBackgroundColor = bg;
    if (hitBorder > 0) {
      ds.borderWidth = bw;
      ds.hoverBorderWidth = bw;
      ds.borderColor = bc;
      ds.hoverBorderColor = bc;
      ds.borderSkipped = false;
    }
  }
  const prevAnim = chart.options.animation;
  const prevAnims = chart.options.animations;
  const ms = chartAnimMs(150);
  chart.options.animation = { duration: ms, easing: "easeOutQuad" };
  chart.options.animations = {
    colors: { duration: ms, easing: "easeOutQuad" },
    borderWidth: { duration: ms },
    x: { duration: 0 },
    y: { duration: 0 },
    numbers: { duration: 0 },
  };
  chart.update();
  chart.options.animation = prevAnim;
  chart.options.animations = prevAnims;
}

/** 命中目标未变则跳过，避免 mousemove 反复 update。 */
export function setChartHoverHit(chart, hit, opts) {
  if (!chart) return;
  if (opts) chart.$hoverOpts = opts;
  const key = hit ? `${hit.datasetIndex}:${hit.index}` : "";
  if (chart.$hoverKey === key) return;
  chart.$hoverKey = key;
  applyChartHoverDim(chart, hit, chart.$hoverOpts || {});
}

/** 指针离开画布时取消高亮（每个 chart 只绑一次）。 */
export function bindChartHoverLeave(chart) {
  if (!chart || chart.$hoverBound) return;
  chart.$hoverBound = true;
  chart.canvas.addEventListener("mouseleave", () => setChartHoverHit(chart, null));
}
