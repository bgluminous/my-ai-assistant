import { el, invoke, listen, fmtDateMs, toast, copyText } from "./shared.js";
import { updatePresentation } from "./update_status.js";
import { renderReleaseNotes } from "./release_notes.js";
import { initUpdateHistory, updateHistoryState } from "./update_history.js";

let state = null;
let confirmingVersion = null;
let panelKey = null;
let progressFrame = null;
let lastProgress = null;

function renderProgress(view, ui) {
  const progress = el("#app-update-progress");
  const fill = progress.firstElementChild;
  const text = el("#app-update-progress-text");
  const hidden = !["downloading", "waiting", "installing"].includes(view.phase);
  if (progress.hidden !== hidden) progress.hidden = hidden;
  if (text.hidden !== hidden) text.hidden = hidden;
  const label = hidden ? "" : `${(ui.downloaded / 1048576).toFixed(1)} MB${ui.percent === null ? "" : ` · ${ui.percent}%`}`;
  if (text.textContent !== label) text.textContent = label;
  if (lastProgress?.fraction !== ui.fraction || lastProgress?.phase !== view.phase) {
    // 只在同一次下载前进时插值；新下载、进度回退和阶段切换立即落到真实位置。
    progress.dataset.animate = String(view.phase === "downloading" && lastProgress?.phase === "downloading" &&
      ui.fraction !== null && lastProgress.fraction !== null && ui.fraction >= lastProgress.fraction);
    progress.classList.toggle("is-indeterminate", ui.fraction === null);
    if (ui.fraction === null) fill.style.removeProperty("transform");
    else fill.style.transform = `scaleX(${ui.fraction})`;
  }
  if (ui.percent === null) {
    if (progress.hasAttribute("aria-valuenow")) progress.removeAttribute("aria-valuenow");
  } else if (progress.getAttribute("aria-valuenow") !== String(ui.percent)) {
    progress.setAttribute("aria-valuenow", String(ui.percent));
  }
  lastProgress = { fraction: ui.fraction, phase: view.phase };
}

function showAbout() {
  el("[role='tab'][data-panel='about-panel']").click();
}
function setUpdateStatus(text, kind = "") {
  const status = el("#app-update-status");
  status.className = `app-update-state ${kind}`.trim();
  status.textContent = text;
}
function renderChangelogs(view) {
  el("#app-update-new-notes").hidden = !view.latestVersion;
  el("#app-update-notes-title").textContent = `新版本 ${view.latestVersion || ""} · 更新日志`;
  renderReleaseNotes(el("#app-update-notes"), view.changelog?.trim() || "该版本未提供更新日志。");
  el("#app-update-current-notes-title").textContent = `当前版本 ${view.currentVersion} · 更新日志`;
  const error = view.currentChangelogError;
  const errorMessage = error?.code === "not_published" ? "当前版本暂无已发布的更新日志。" :
    error ? `当前版本更新日志获取失败。${error.message || "请稍后重试。"}` : "";
  const hasNotes = view.currentChangelog != null;
  renderReleaseNotes(el("#app-update-current-notes"), hasNotes ?
    (view.currentChangelog.trim() || "该版本未提供更新日志。") :
    errorMessage || (view.phase === "checking" ? "正在获取当前版本的更新日志…" : "检查更新后显示当前版本的更新日志。"));
  el("#app-update-current-notes-status").textContent = hasNotes && error ? `${errorMessage}已保留上次读取的内容。` : "";
  el("#app-update-current-notes-status").hidden = !hasNotes || !error;
}
function render(view, forcePanel = false) {
  if (!view || (state && view.revision < state.revision)) return;
  state = view;
  const ui = updatePresentation(view);
  // 下载字节更新不重写日志标题、按钮和业务面板；同一帧只绘制最新快照。
  const { revision, downloadedBytes, totalBytes, ...panel } = view;
  const key = JSON.stringify(panel);
  if (!forcePanel && panelKey === key) {
    if (progressFrame === null) progressFrame = requestAnimationFrame(() => {
      progressFrame = null;
      renderProgress(state, updatePresentation(state));
    });
    return;
  }
  panelKey = key;
  // 取消、失败和安装等状态立即生效，不能被尚未绘制的下载事件覆盖。
  if (progressFrame !== null) cancelAnimationFrame(progressFrame);
  progressFrame = null;
  renderProgress(view, ui);
  setUpdateStatus(ui.text, ui.failed ? "bad" : view.phase === "upToDate" ? "ok" :
    ui.busy ? "busy" : view.latestVersion ? "available" : "");
  el("#app-update-meta").textContent = `当前版本 ${view.currentVersion}${view.checkedAtMs ? ` · 最近检查 ${fmtDateMs(view.checkedAtMs)}` : ""}`;
  renderChangelogs(view);
  el("#app-update-notes").inert = ui.paused;
  el("#app-update-current-notes").inert = ui.paused;
  updateHistoryState(view, ui.paused);
  el("#app-update-manual").textContent = view.manualReason || "";
  el("#app-update-manual").hidden = !view.manualReason;
  el("#app-update-required").hidden = !view.forceUpdate;
  const canInstall = !!view.latestVersion && !!view.canInstall;
  const checkButton = el("#app-update-check");
  checkButton.disabled = ui.busy;
  checkButton.textContent = ui.failed ? "重试" : view.phase === "checking" ? "检查中…" : "检查更新";
  checkButton.classList.toggle("primary", !canInstall);
  checkButton.classList.toggle("secondary", canInstall);
  el("#app-update-install").hidden = !canInstall;
  el("#app-update-install").disabled = ui.busy;
  el("#app-update-install").textContent = `更新到 ${view.latestVersion || "新版本"}`;
  el("#app-update-cancel").hidden = !ui.cancelable;
  el("#app-update-releases").disabled = ui.paused;
  el("#app-update-releases").title = view.releasesUrl;
  el("#app-update-proxy").disabled = ui.paused;
  el("#app-update-auto").checked = !!view.automatic;
  el("#app-update-auto").disabled = ui.paused;
  const chip = el("#app-update-chip");
  chip.hidden = !ui.failed && !view.latestVersion && !ui.paused;
  chip.textContent = ui.failed ? "更新失败" : ui.paused ? "正在准备更新" : `新版本 ${view.latestVersion}`;
  chip.classList.toggle("has-update", !!view.latestVersion);
  chip.title = ui.text;
  // 不调用 show()/setFocus()：静默启动仍保持主窗口隐藏。
  if ((view.forceUpdate || ui.paused) && el("#about-panel").hidden) showAbout();
  for (const tab of document.querySelectorAll("[role='tab'][data-panel]")) {
    tab.disabled = (view.forceUpdate || ui.paused) && tab.dataset.panel !== "about-panel";
  }
  for (const panel of document.querySelectorAll(".panel[role='tabpanel']")) {
    panel.inert = (view.forceUpdate || ui.paused) && panel.id !== "about-panel";
  }
  el("#pricing-chip").disabled = !!view.forceUpdate || ui.paused;
  el("#settings-btn").disabled = ui.paused;
  el("#proxy-btn").disabled = ui.paused;
  if (ui.paused) for (const modal of document.querySelectorAll(".modal")) modal.hidden = true;
}
function report(error) {
  panelKey = null;
  const message = error?.message || "操作失败，请重试或前往 GitHub Releases。";
  setUpdateStatus(message, "bad");
}
async function check() {
  try { render(await invoke("app_update_check")); } catch (error) { report(error); }
}
function requestInstall() {
  if (!state?.canInstall || !state.latestVersion) return;
  confirmingVersion = state.latestVersion;
  el("#app-update-confirm-text").textContent = `下载并安装 ${confirmingVersion}，完成后将重启应用。请先保存尚未提交的编辑。`;
  el("#app-update-confirm").hidden = false;
  el("#app-update-confirm-ok").focus();
}
async function install() {
  const version = confirmingVersion;
  confirmingVersion = null;
  el("#app-update-confirm").hidden = true;
  if (!version) return;
  try { render(await invoke("app_update_install", { version })); } catch (error) { report(error); }
}

function openChangelogLink(event) {
  if (event.type === "auxclick" && event.button !== 1) return;
  const link = event.target.closest?.("a[href]");
  if (!link || !window.__TAURI__?.core) return;
  // 不在具有本机权限的 WebView 内导航，也不依赖各平台的 target=_blank 行为。
  event.preventDefault();
  if (state && updatePresentation(state).paused) return;
  void invoke("app_update_open_link", { url: link.getAttribute("href") }).catch(report);
}

export async function initAppUpdate() {
  initUpdateHistory();
  for (const selector of ["#app-update-notes", "#app-update-current-notes", "#app-update-history-notes"]) {
    el(selector).addEventListener("click", openChangelogLink);
    el(selector).addEventListener("auxclick", openChangelogLink);
  }
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { confirmingVersion = null; el("#app-update-confirm").hidden = true; }
  });
  el("#app-update-chip").addEventListener("click", showAbout);
  el("#app-update-check").addEventListener("click", check);
  el("#app-update-install").addEventListener("click", requestInstall);
  el("#app-update-confirm-ok").addEventListener("click", install);
  for (const node of el("#app-update-confirm").querySelectorAll("[data-close]")) {
    node.addEventListener("click", () => { confirmingVersion = null; el("#app-update-confirm").hidden = true; });
  }
  el("#app-update-cancel").addEventListener("click", () => { void invoke("app_update_cancel").catch(report); });
  el("#app-update-proxy").addEventListener("click", () => el("#proxy-btn").click());
  el("#app-update-releases").addEventListener("click", () => { void invoke("app_update_open_releases").catch(report); });
  el("#app-update-copy-link").addEventListener("click", () => {
    void copyText(state?.releasesUrl || "https://github.com/bgluminous/my-ai-assistant/releases")
      .then(() => toast("ok", "已复制 GitHub Releases 地址。", { key: "app-update" })).catch(report);
  });
  el("#app-update-auto").addEventListener("change", async (event) => {
    // 复选框已被用户改动，即使快照相同也需恢复服务器确认的值。
    try { render(await invoke("app_update_settings_set", { enabled: event.target.checked }), true); }
    catch (error) { if (state) render(state, true); report(error); }
  });
  el("#settings-btn").addEventListener("click", () => { void invoke("app_update_status").then(render).catch(() => {}); });
  if (!window.__TAURI__?.core) return;
  try {
    await listen("app-update-state", (event) => render(event.payload));
    render(await invoke("app_update_status"));
  } catch (error) { report(error); }
}
