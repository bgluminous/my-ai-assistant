import { locale, translate } from "./i18n.js";
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
  waiting: "syncProgress.gptWaitingToSync", scanning: "syncProgress.gptScanningLocalFiles", storing: "syncProgress.gptPreparingData",
  connecting: "syncProgress.gptConnectingToServer", uploading: "syncProgress.gptUploading", downloading: "syncProgress.gptDownloading",
  recovering: "syncProgress.gptResyncing", completed: "syncProgress.gptSyncComplete", failed: "syncProgress.gptSyncFailed",
};
const number = (value) => Number(value || 0).toLocaleString(locale());
const time = (value) => value ? new Date(value).toLocaleString(locale(), { hour12: false }) : translate("syncProgress.noSuccessfulSyncYet");

/** 计数表示本次尝试中已确认或落库的同步记录，不是新增 Token 或去重后的用量。 */
export function syncProgressView(status, { compact = false, showDisabled = false, now = Date.now() } = {}) {
  const p = status.progress;
  const running = status.state === "running";
  const failed = status.state === "error" || status.state === "paused";
  const completed = status.state === "idle" && p?.phase === "completed";
  const remaining = completed ? Math.max(0, (p.finishedAt || 0) + 5000 - now) : 0;
  const disabled = status.state === "disabled";
  let title = disabled ? translate("syncProgress.disabledCollectingLocallyOnly") : translate("syncProgress.gptSyncReady");
  let counts = "";
  let percent = null;
  if (running) {
    title = (phases[p?.phase] && translate(phases[p?.phase])) || translate("syncProgress.gptSyncing");
    if (p?.phase === "uploading") {
      counts = translate("syncProgress.records", number(p.uploadedRecords), number(p.uploadTotal));
      if (p.uploadTotal > 0) percent = Math.min(100, Math.floor(p.uploadedRecords / p.uploadTotal * 100));
      else counts = translate("syncProgress.noPendingUploadsRegisteringDevice");
    } else if (p?.phase === "downloading") {
      counts = translate("syncProgress.recordsPages", number(p.downloadedRecords), number(p.downloadedPages));
    }
  } else if (failed || completed) {
    title = failed ? (status.state === "paused" ? translate("syncProgress.gptSyncPaused") : translate("syncProgress.gptSyncFailedWaitingToRetry")) : phases.completed;
    if (p) counts = translate("syncProgress.uploadedDownloaded", number(p.uploadedRecords), number(p.downloadedRecords));
  }
  return {
    hidden: disabled ? !showDisabled : compact && !running && !failed && !remaining,
    title, counts, percent, running, remaining,
    tone: failed ? "error" : completed ? "success" : running ? "running" : "idle",
    details: disabled ? translate("syncProgress.configureTheServerAndSyncKeyTo")
      : translate("syncProgress.pendingUploadsLastSuccess", number(status.pendingRecords), time(status.lastSuccessAt)),
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
    <progress class="sync-progress-bar" max="100" data-i18n-aria-label="syncProgress.gptUploadProgress"></progress>
    <p class="sync-progress-details"></p><p class="sync-progress-error" role="status" aria-live="polite"></p>`;
  const find = (name) => node.querySelector(`.sync-progress-${name}`);
  const title = find("title"), counts = find("counts"), percent = find("percent"), bar = find("bar");
  bar.setAttribute("aria-label", translate("syncProgress.gptUploadProgress"));
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
    bar.setAttribute("aria-label", view.percent == null ? translate("syncProgress.gptSyncInProgress") : translate("syncProgress.gptUploadProgress"));
    if (view.percent == null) bar.removeAttribute("value");
    else bar.setAttribute("value", String(view.percent));
    details.textContent = view.details;
    details.hidden = !!options.compact;
    error.textContent = view.error;
    error.hidden = !view.error;
    if (options.compact && view.remaining > 0) timer = setTimeout(() => render(current), view.remaining);
  }
  const relabel = () => { if (current) render(current); };
  globalThis.window?.addEventListener?.("uilanguagechange", relabel);
  subscribers.add(render);
  if (current) render(current);
  else node.hidden = true;
  void refreshSyncStatus().catch((failure) => {
    if (disposed || current) return;
    node.hidden = false;
    node.dataset.state = "error";
    title.textContent = translate("syncProgress.gptSyncStatusTemporarilyUnavailable");
    bar.hidden = true;
    error.textContent = syncError(failure);
  });
  const dispose = () => {
    disposed = true;
    clearTimeout(timer);
    globalThis.window?.removeEventListener?.("uilanguagechange", relabel);
    subscribers.delete(render);
    mounted.delete(node);
  };
  mounted.set(node, dispose);
  return dispose;
}
