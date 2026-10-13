import { el, invoke } from "./shared.js";
import { renderReleaseNotes } from "./release_notes.js";

let versions = [];
let loaded = false;
let loadingList = false;
let loadingNotes = false;
let paused = false;
let currentVersion = "";
let selectedVersion = "";
let notesRequest = 0;
const cache = new Map();

function controls() {
  el("#app-update-history-toggle").disabled = paused;
  el("#app-update-history").inert = paused;
  el("#app-update-history-version").disabled = paused || loadingList || !versions.length;
  el("#app-update-history-refresh").disabled = paused || loadingList;
  el("#app-update-history-retry").disabled = paused || loadingList || loadingNotes;
  el("#app-update-history").setAttribute("aria-busy", String(loadingList || loadingNotes));
}
function status(message, failed = false) {
  const node = el("#app-update-history-status");
  node.textContent = message;
  node.className = failed ? "status bad" : "muted small";
  node.hidden = !message;
}
function renderVersions() {
  const select = el("#app-update-history-version");
  select.replaceChildren(...versions.map(({ version }) => {
    const option = document.createElement("option");
    option.value = version;
    option.textContent = version === currentVersion ? `${version}（当前版本）` : version;
    option.selected = version === selectedVersion;
    return option;
  }));
}
async function loadNotes(version, refresh = false) {
  if (!version || paused) return;
  const request = ++notesRequest;
  selectedVersion = version;
  const notes = el("#app-update-history-notes");
  notes.hidden = false;
  notes.setAttribute("aria-label", `${version} 更新日志`);
  el("#app-update-history-retry").hidden = true;
  loadingNotes = true;
  status("");
  renderReleaseNotes(notes, "正在读取更新日志…");
  controls();
  try {
    const release = !refresh && cache.has(version) ? cache.get(version) :
      await invoke("app_update_changelog", { version });
    if (release.version !== version) throw new Error("返回的版本与所选版本不一致，请重试。");
    cache.set(version, release);
    // 快速切换版本时，旧请求不得覆盖新选择。
    if (request !== notesRequest) return;
    renderReleaseNotes(notes, release.changelog?.trim() || "该版本未提供更新日志。");
  } catch (error) {
    if (request !== notesRequest) return;
    notes.hidden = true;
    status(`读取 ${version} 更新日志失败。${error?.message || "请稍后重试。"}`, true);
    el("#app-update-history-retry").hidden = false;
  } finally {
    if (request === notesRequest) { loadingNotes = false; controls(); }
  }
}
async function loadVersions() {
  if (loadingList || paused) return;
  loadingList = true;
  loadingNotes = false;
  notesRequest++;
  el("#app-update-history-retry").hidden = true;
  status("正在读取历史版本列表…");
  controls();
  try {
    const history = await invoke("app_update_history");
    if (!Array.isArray(history)) throw new Error("版本列表格式无效，请重试。");
    versions = history;
    loaded = true;
    selectedVersion = versions.some(v => v.version === selectedVersion) ? selectedVersion : versions[0]?.version || "";
    renderVersions();
    loadingList = false;
    if (selectedVersion) void loadNotes(selectedVersion);
    else {
      el("#app-update-history-notes").hidden = true;
      status("当前平台暂无已登记的历史版本，可前往 GitHub Releases 查看。");
    }
  } catch (error) {
    status(`历史版本列表获取失败。${error?.message || "请点击“刷新列表”重试。"}`, true);
  } finally {
    loadingList = false;
    controls();
  }
}

export function updateHistoryState(view, isPaused) {
  paused = isPaused;
  if (currentVersion !== view.currentVersion) {
    currentVersion = view.currentVersion;
    if (loaded) renderVersions();
  }
  controls();
}

export function initUpdateHistory() {
  el("#app-update-history-toggle").addEventListener("click", () => {
    if (paused) return;
    const section = el("#app-update-history");
    section.hidden = !section.hidden;
    const button = el("#app-update-history-toggle");
    button.setAttribute("aria-expanded", String(!section.hidden));
    button.textContent = section.hidden ? "历史更新日志" : "收起历史日志";
    if (!section.hidden && !loaded) void loadVersions();
  });
  el("#app-update-history-refresh").addEventListener("click", () => { void loadVersions(); });
  el("#app-update-history-version").addEventListener("change", event => { void loadNotes(event.target.value); });
  el("#app-update-history-retry").addEventListener("click", () => { void loadNotes(selectedVersion, true); });
}
