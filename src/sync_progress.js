import { invoke, listen } from "./shared.js";
import { syncError } from "./sync_errors.js";

// 每个窗口共用一次订阅；快照序号来自后端，迟到的查询不能覆盖新的事件。
let current = null;
let listening = null;
let reading = null;
const subscribers = new Set();
const mounted = new WeakMap();

function accept(status) {
  if (!status || typeof status.state !== "string") return;
  if (current && (status.sequence ?? 0) < (current.sequence ?? 0)) return;
  current = status;
  for (const update of subscribers) update(status);
}
function ensureListening() {
  if (!listening) {
    listening = (async () => {
      await listen("codex-usage-sync-status", (event) => {
        if (event.payload?.state) accept(event.payload);
        else void refreshSyncStatus().catch(() => {});
      });
      await listen("codex-usage-changed", () => { void refreshSyncStatus().catch(() => {}); });
    })().catch((error) => { listening = null; throw error; });
  }
  return listening;
}
export function refreshSyncStatus() {
  if (!reading) {
    reading = (async () => {
      await ensureListening();
      accept(await invoke("codex_usage_sync_status"));
      return current;
    })().finally(() => { reading = null; });
  }
  return reading;
}

const phases = {
  waiting: "GPT 等待同步", scanning: "GPT 正在扫描本机", storing: "GPT 正在整理数据",
  connecting: "GPT 正在连接服务器", uploading: "GPT 正在上传", downloading: "GPT 正在下载",
  recovering: "GPT 正在重新同步", completed: "GPT 同步完成", failed: "GPT 同步失败",
};
const number = (value) => Number(value || 0).toLocaleString("zh-CN");
const time = (value) => value ? new Date(value).toLocaleString("zh-CN", { hour12: false }) : "尚无成功记录";

/** 计数表示本次尝试中已确认或落库的同步记录，不是新增 Token 或去重后的用量。 */
export function syncProgressView(status, { compact = false, showDisabled = false, now = Date.now() } = {}) {
  const p = status.progress;
  const running = status.state === "running";
  const failed = status.state === "error" || status.state === "paused";
  const completed = status.state === "idle" && p?.phase === "completed";
  const remaining = completed ? Math.max(0, (p.finishedAt || 0) + 5000 - now) : 0;
  const disabled = status.state === "disabled";
  let title = disabled ? "未启用，仅采集本机数据" : "GPT 同步已就绪";
  let counts = "";
  let percent = null;
  if (running) {
    title = phases[p?.phase] || "GPT 同步中";
    if (p?.phase === "uploading") {
      counts = `${number(p.uploadedRecords)} / ${number(p.uploadTotal)} 条`;
      if (p.uploadTotal > 0) percent = Math.min(100, Math.floor(p.uploadedRecords / p.uploadTotal * 100));
      else counts = "无待上传记录，正在登记设备";
    } else if (p?.phase === "downloading") {
      counts = `${number(p.downloadedRecords)} 条 · ${number(p.downloadedPages)} 页`;
    }
  } else if (failed || completed) {
    title = failed ? (status.state === "paused" ? "GPT 同步已暂停" : "GPT 同步失败，等待重试") : phases.completed;
    if (p) counts = `已上传 ${number(p.uploadedRecords)} 条 · 已下载 ${number(p.downloadedRecords)} 条`;
  }
  return {
    hidden: disabled ? !showDisabled : compact && !running && !failed && !remaining,
    title, counts, percent, running, remaining,
    tone: failed ? "error" : completed ? "success" : running ? "running" : "idle",
    details: disabled ? "配置服务器和同步密钥后，自动同步本机与其他设备的用量。"
      : `待上传 ${number(status.pendingRecords)} 条 · 最近成功：${time(status.lastSuccessAt)}`,
    error: failed && status.error ? syncError(status.error) : "",
  };
}

/** 仅渲染状态，不启动同步、不使统计缓存失效；关闭面板不影响后台任务。 */
export function mountSyncProgress(node, options = {}) {
  if (!node) return () => {};
  if (mounted.has(node)) return mounted.get(node);
  node.classList.add("sync-progress");
  node.classList.toggle("sync-progress-compact", !!options.compact);
  node.innerHTML = `<div class="sync-progress-line" role="status" aria-live="polite" aria-atomic="true">
    <span class="sync-progress-dot" aria-hidden="true"></span>
    <span class="sync-progress-title"></span><span class="sync-progress-counts"></span>
    <span class="sync-progress-percent"></span></div>
    <progress class="sync-progress-bar" max="100" aria-label="GPT 上传进度"></progress>
    <p class="sync-progress-details"></p><p class="sync-progress-error" role="status" aria-live="polite"></p>`;
  const find = (name) => node.querySelector(`.sync-progress-${name}`);
  const title = find("title"), counts = find("counts"), percent = find("percent"), bar = find("bar");
  const details = find("details"), error = find("error");
  let timer = null;
  let disposed = false;
  function render(status) {
    clearTimeout(timer);
    timer = null;
    const view = syncProgressView(status, options);
    node.hidden = view.hidden;
    node.dataset.state = view.tone;
    title.textContent = view.title;
    counts.textContent = view.counts;
    counts.hidden = !view.counts;
    percent.textContent = view.percent == null ? "" : `${view.percent}%`;
    percent.hidden = view.percent == null || options.compact;
    bar.hidden = !view.running;
    bar.setAttribute("aria-label", view.percent == null ? "GPT 同步进行中" : "GPT 上传进度");
    if (view.percent == null) bar.removeAttribute("value");
    else bar.setAttribute("value", String(view.percent));
    details.textContent = view.details;
    details.hidden = !!options.compact;
    error.textContent = view.error;
    error.hidden = !view.error;
    if (options.compact && view.remaining > 0) timer = setTimeout(() => render(current), view.remaining);
  }
  subscribers.add(render);
  if (current) render(current);
  else node.hidden = true;
  void refreshSyncStatus().catch((failure) => {
    if (disposed || current) return;
    node.hidden = false;
    node.dataset.state = "error";
    title.textContent = "暂时无法读取 GPT 同步状态";
    bar.hidden = true;
    error.textContent = syncError(failure);
  });
  const dispose = () => {
    disposed = true;
    clearTimeout(timer);
    subscribers.delete(render);
    mounted.delete(node);
  };
  mounted.set(node, dispose);
  return dispose;
}
