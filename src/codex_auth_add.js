import { bindText, translate } from "./i18n.js";
import { el, invoke, fillStatus } from "./shared.js";
import { codexAuthErrorMessage } from "./codex_auth_editor.js";

// 三个来源各自保留草稿；文件和 JSON 始终以原文交给 Rust，避免扩展字段的数值精度丢失。
export function createCodexAuthAdder({ isSaving, clearStatus }) {
  const tabs = el("#account-add-mode");
  const buttons = [...tabs.querySelectorAll("[data-mode]")];
  const panels = { token: el("#account-token-fields"), file: el("#account-auth-file-field"), json: el("#account-auth-field") };
  const raw = el("#account-auth-json"), status = el("#account-auth-status");
  const pick = el("#account-auth-pick"), name = el("#account-auth-file-name"), fileStatus = el("#account-auth-file-status");
  let active = true, busy = false, mode = "token", file = null, disabled = [];

  function clear() {
    status.hidden = true;
    status.textContent = "";
    clearStatus();
  }
  function showError(error) {
    if (!active) return false;
    const message = codexAuthErrorMessage(error);
    if (!message) return false;
    fillStatus(status, "bad", message);
    status.scrollIntoView?.({ block: "nearest" });
    return true;
  }
  function switchMode(next, focus = false) {
    if (!active || busy || isSaving() || !panels[next]) return;
    mode = next;
    clear();
    for (const button of buttons) {
      const selected = button.dataset.mode === mode;
      button.classList.toggle("active", selected);
      button.setAttribute("aria-selected", String(selected));
      button.tabIndex = selected ? 0 : -1;
      panels[button.dataset.mode].hidden = !selected;
      if (selected && focus) button.focus();
    }
  }
  const onClick = event => {
    const button = event.target.closest("[data-mode]");
    if (button) switchMode(button.dataset.mode);
  };
  const onKey = event => {
    const index = buttons.indexOf(event.target);
    if (index < 0 || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
      : (index + (event.key === "ArrowRight" ? 1 : -1) + buttons.length) % buttons.length;
    switchMode(buttons[next].dataset.mode, true);
  };
  function restoreControls() {
    for (const [control, value] of disabled) control.disabled = value;
    disabled = [];
  }
  async function pickFile() {
    if (!active || busy || isSaving()) return;
    busy = true;
    disabled = [...buttons, pick, el("#account-save"), el("#account-import-local")].map(control => [control, control.disabled]);
    for (const [control] of disabled) control.disabled = true;
    clear();
    try {
      const selected = await invoke("codex_auth_pick_file");
      if (!active || !selected) return;
      file = selected;
      name.textContent = file.name;
      fillStatus(fileStatus, "ok", () => translate("codexAuthAdd.fileValidatedSelectSaveToAddThe"));
    } catch (error) {
      if (active && !showError(error)) fillStatus(status, "bad", () => translate("codexAuthAdd.couldNotLoadTheAuthFileTry"));
    } finally {
      if (active) { busy = false; restoreControls(); }
    }
  }
  tabs.hidden = false;
  for (const button of buttons) {
    const panel = panels[button.dataset.mode];
    panel.setAttribute("role", "tabpanel");
    panel.setAttribute("aria-labelledby", button.id);
  }
  tabs.addEventListener("click", onClick);
  tabs.addEventListener("keydown", onKey);
  pick.addEventListener("click", pickFile);
  switchMode("token");
  return {
    clear, showError,
    get busy() { return busy; },
    get full() { return mode !== "token"; },
    credentials() {
      if (mode === "token") return null;
      if (mode === "file" && !file) throw new Error("codex_auth_file_missing");
      if (mode === "json" && !raw.value.trim()) throw new Error("codex_auth_text_missing");
      return { codexAuthText: mode === "file" ? file.text : raw.value };
    },
    dispose() {
      active = false;
      restoreControls();
      tabs.removeEventListener("click", onClick);
      tabs.removeEventListener("keydown", onKey);
      pick.removeEventListener("click", pickFile);
      tabs.hidden = true;
      for (const [kind, panel] of Object.entries(panels)) {
        panel.hidden = kind !== "token";
        panel.removeAttribute("role");
        panel.removeAttribute("aria-labelledby");
      }
      raw.value = "";
      file = null;
      bindText(name, () => translate("account.noFileSelected"));
      fileStatus.hidden = true;
      fileStatus.textContent = "";
      clear();
    },
  };
}
