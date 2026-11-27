import { translate } from "./i18n.js";
import { el, invoke } from "./shared.js";
import { updateErrorText } from "./update_status.js";
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
    option.textContent = version === currentVersion ? translate("updateHistory.current", version) : version;
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
  notes.setAttribute("aria-label", translate("updateHistory.releaseNotes", version));
  el("#app-update-history-retry").hidden = true;
  loadingNotes = true;
  status("");
  renderReleaseNotes(notes, translate("updateHistory.loadingReleaseNotes"));
  controls();
  try {
    const release = !refresh && cache.has(version) ? cache.get(version) :
      await invoke("app_update_changelog", { version });
    if (release.version !== version) throw new Error(translate("updateHistory.theReturnedVersionDoesNotMatchThe"));
    cache.set(version, release);
    // 快速切换版本时，旧请求不得覆盖新选择。
    if (request !== notesRequest) return;
    renderReleaseNotes(notes, release.changelog?.trim() || translate("appUpdate.noReleaseNotesWereProvidedForThis"));
  } catch (error) {
    if (request !== notesRequest) return;
    notes.hidden = true;
    status(translate("updateHistory.couldNotLoadReleaseNotesFor", version, updateErrorText(error) || translate("appUpdate.tryAgainLater")), true);
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
  status(translate("updateHistory.loadingReleaseHistory"));
  controls();
  try {
    const history = await invoke("app_update_history");
    if (!Array.isArray(history)) throw new Error(translate("updateHistory.invalidVersionListTryAgain"));
    versions = history;
    loaded = true;
    selectedVersion = versions.some(v => v.version === selectedVersion) ? selectedVersion : versions[0]?.version || "";
    renderVersions();
    loadingList = false;
    if (selectedVersion) void loadNotes(selectedVersion);
    else {
      el("#app-update-history-notes").hidden = true;
      status(translate("updateHistory.noPastVersionsRegisteredForThisPlatform"));
    }
  } catch (error) {
    status(translate("updateHistory.couldNotLoadReleaseHistory", updateErrorText(error) || translate("updateHistory.selectRefreshListToTryAgain")), true);
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
  window.addEventListener("uilanguagechange", () => {
    renderVersions();
    const section = el("#app-update-history");
    el("#app-update-history-toggle").textContent = section.hidden ? translate("updater.releaseHistory") : translate("updateHistory.hideReleaseHistory");
  });
  el("#app-update-history-toggle").addEventListener("click", () => {
    if (paused) return;
    const section = el("#app-update-history");
    section.hidden = !section.hidden;
    const button = el("#app-update-history-toggle");
    button.setAttribute("aria-expanded", String(!section.hidden));
    button.textContent = section.hidden ? translate("updater.releaseHistory") : translate("updateHistory.hideReleaseHistory");
    if (!section.hidden && !loaded) void loadVersions();
  });
  el("#app-update-history-refresh").addEventListener("click", () => { void loadVersions(); });
  el("#app-update-history-version").addEventListener("change", event => { void loadNotes(event.target.value); });
  el("#app-update-history-retry").addEventListener("click", () => { void loadNotes(selectedVersion, true); });
}
