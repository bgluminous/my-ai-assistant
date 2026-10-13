import { el, invoke, listen, fmtDateMs, toast, copyText } from "./shared.js";
import { updatePresentation } from "./update_status.js";

let state = null;
let confirmingVersion = null;

function showAbout() {
  el("[role='tab'][data-panel='about-panel']").click();
}
function setUpdateStatus(text, kind = "") {
  const status = el("#app-update-status");
  status.className = `app-update-state ${kind}`.trim();
  status.textContent = text;
}
function render(view) {
  if (!view || (state && view.revision < state.revision)) return;
  state = view;
  const ui = updatePresentation(view);
  setUpdateStatus(ui.text, ui.failed ? "bad" : view.phase === "upToDate" ? "ok" :
    ui.busy ? "busy" : view.latestVersion ? "available" : "");
  el("#app-update-meta").textContent = `当前版本 ${view.currentVersion}${view.checkedAtMs ? ` · 最近检查 ${fmtDateMs(view.checkedAtMs)}` : ""}`;
  el("#app-update-notes").textContent = view.changelog || (view.latestVersion ? "该版本未提供更新说明。" : "");
  el("#app-update-notes").hidden = !view.latestVersion;
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
  const progress = el("#app-update-progress");
  progress.hidden = !["downloading", "waiting", "installing"].includes(view.phase);
  if (ui.percent === null) progress.removeAttribute("value"); else progress.value = ui.percent;
  el("#app-update-progress-text").textContent = progress.hidden ? "" :
    `${(ui.downloaded / 1048576).toFixed(1)} MB${ui.percent === null ? "" : ` · ${ui.percent}%`}`;
  el("#app-update-progress-text").hidden = progress.hidden;

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

export async function initAppUpdate() {
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
    try { render(await invoke("app_update_settings_set", { enabled: event.target.checked })); }
    catch (error) { if (state) render(state); report(error); }
  });
  el("#settings-btn").addEventListener("click", () => { void invoke("app_update_status").then(render).catch(() => {}); });
  if (!window.__TAURI__?.core) return;
  try {
    await listen("app-update-state", (event) => render(event.payload));
    render(await invoke("app_update_status"));
  } catch (error) { report(error); }
}
