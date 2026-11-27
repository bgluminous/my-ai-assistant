import { errorCode, translate } from "./i18n.js";
import { invoke, resetError, copyText } from "./shared.js";
import { dialog } from "./client_dialog.js";

const activeSwitches = new Map();
const NAMES = { codex: "GPT / Codex", claude: "Claude", opencode: "OpenCode" };
const ERRORS = {
  client_not_installed: "linkedClientUi.noCliOrDesktopClientDetectedConfigure",
  client_target_unavailable: "linkedClientUi.thisClientIsUnavailableCheckItsPath",
  client_operation_busy: "linkedClientUi.anotherLocalOperationIsRunningForThis",
  client_path_invalid: "linkedClientUi.invalidClientPathCorrectItInSettings",
  codex_exe_invalid: "linkedClientUi.invalidCodexDesktopPathCorrectItIn",
  claude_exe_invalid: "linkedClientUi.invalidClaudeDesktopPathCorrectItIn",
  opencode_exe_invalid: "linkedClientUi.invalidOpencodeCliPathSelectTheCli",
  opencode_desktop_exe_invalid: "linkedClientUi.invalidOpencodeDesktopPathCorrectItIn",
  account_not_verified: "linkedClientUi.refreshTheAccountToVerifyItsCredentials",
  claude_no_refresh_token: "linkedClientUi.theCliNeedsARefreshTokenEdit",
  client_close_failed: "linkedClientUi.desktopCouldNotBeClosedQuitIt",
  client_close_required: "linkedClientUi.desktopHasStartedConfirmItCanBe",
  client_launch_failed: "linkedClientUi.couldNotStartTheClientCheckThe",
  client_launch_failed_rolled_back: "linkedClientUi.desktopDidNotStartTheOriginalLogin",
  client_switch_cancelled: "linkedClientUi.thisClientWasNotSwitched",
  claude_desktop_machine_unavailable: "linkedClientUi.couldNotIdentifyTheComputerThatOwns",
  claude_keychain_unavailable: "linkedClientUi.couldNotReadClaudeSystemCredentialsCheck",
  claude_keychain_rollback_failed: "linkedClientUi.couldNotRestoreTheClaudeKeychainCheck",
  opencode_helper_lifetime_failed: "linkedClientUi.couldNotStartTheLocalOpencodeService",
  claude_identity_required: "linkedClientUi.refreshTheAccountToVerifyItsIdentity",
  claude_desktop_account_mismatch: "linkedClientUi.theDesktopLoginBelongsToAnotherAccount",
  claude_desktop_login_required: "linkedClientUi.noValidDesktopLoginDetectedLinkAgain",
  claude_desktop_binding_required: "linkedClientUi.linkThisAccountSDesktopLoginFirst",
  claude_desktop_binding_pending: "linkedClientUi.aDesktopLoginLinkIsInProgress",
  claude_desktop_binding_expired: "linkedClientUi.linkTimedOutARestoreWasAttempted",
  claude_desktop_operation_stale: "linkedClientUi.thisLinkingSessionHasEndedOpenIt",
  claude_desktop_path_ambiguous: "linkedClientUi.severalDesktopDataFoldersWereFoundChoose",
  claude_desktop_path_invalid: "linkedClientUi.invalidDesktopDataFolderCorrectItIn",
  claude_desktop_recovery_failed: "linkedClientUi.couldNotRestoreTheOriginalLoginThe",
  claude_desktop_snapshot_foreign: "linkedClientUi.thisSnapshotBelongsToAnotherComputerOr",
  claude_desktop_snapshot_invalid: "linkedClientUi.loginSnapshotUnreadableLinkAgain",
  claude_desktop_snapshot_too_large: "linkedClientUi.desktopLoginDataIsTooLargeTo",
  codex_auth_store_unsupported: "linkedClientUi.theCurrentCodexAuthenticationStorageModeCannot",
  codex_keyring_unavailable: "linkedClientUi.couldNotAccessTheCodexSystemCredential",
  codex_auth_rollback_failed: "linkedClientUi.couldNotRestoreTheCodexLoginCheck",
  opencode_v2_required: "linkedClientUi.opencodeV2IsRequired",
  opencode_gui_is_not_cli: "linkedClientUi.theCliPathPointsToTheDesktop",
  opencode_desktop_cli_not_found: "linkedClientUi.bundledDesktopCliNotFoundCheckThe",
  opencode_client_auth_pending: "linkedClientUi.opencodeNativeAuthorizationIsInProgressFinish",
  opencode_workspace_mismatch: "linkedClientUi.theAuthorizedWorkspaceDiffersFromThisAccount",
  opencode_switch_restore_failed: "linkedClientUi.theOriginalOpencodeLoginCouldNotBe",
  opencode_cli_timeout: "linkedClientUi.opencodeClientTimedOutTryAgainLater",
};
export function clientError(error) {
  const text = errorCode(error);
  if (text.startsWith("codex_refresh_denied")) return translate("linkedClientUi.credentialsCouldNotBeRenewedAuthorizeAgain");
  return (ERRORS[text] && translate(ERRORS[text])) || resetError(error);
}
function action(view, label, className = "primary") {
  const button = document.createElement("button");
  button.type = "button"; button.className = className; button.textContent = label;
  view.foot.append(button); return button;
}
function paragraph(view, text) {
  const p = document.createElement("p"); p.textContent = text; view.content.append(p); return p;
}
export function resultText(result) {
  return (result.clients || []).map(client => `${client.target === "cli" ? "CLI" : "Desktop"}：${
    client.status === "success" ? translate("linkedClientUi.switched") : clientError(client.error || translate("linkedClientUi.switchFailed"))}`).join("\n");
}

/** 成功结果显示 3 秒。按截止时间更新按钮，手动关闭或复用弹窗时可取消倒计时。 */
export function startSwitchResultCountdown(button, close) {
  const deadline = Date.now() + 3000;
  let timer;
  function tick() {
    const remainingMs = deadline - Date.now();
    if (remainingMs <= 0) {
      close();
      return;
    }
    const seconds = Math.ceil(remainingMs / 1000);
    button.textContent = translate("linkedClientUi.closeS", seconds);
    timer = setTimeout(tick, remainingMs - (seconds - 1) * 1000);
  }
  tick();
  return () => clearTimeout(timer);
}

/** Resolves only after binding completes or cancellation has restored the login. */
export function bindClaudeDesktop(id) {
  const view = dialog(translate("linkedClientUi.linkClaudeDesktopLogin"));
  const message = paragraph(view, translate("linkedClientUi.desktopWillCloseAndItsLoginWill"));
  const button = action(view, translate("linkedClientUi.startLinking"));
  let operationId = null, phase = "start", settled = false;
  let resolve;
  const done = new Promise(r => { resolve = r; });
  const settle = value => { if (!settled) { settled = true; resolve(value); } };
  view.onCancel(async () => {
    if (operationId) await invoke("claude_desktop_binding_cancel", { id: operationId });
    operationId = null; settle(false);
  });
  button.onclick = async () => {
    if (button.disabled || view.closed) return;
    button.disabled = true; view.busy(true);
    try {
      const result = phase === "start"
        ? await invoke("claude_desktop_binding_start", { id, closeDesktop: true })
        : await invoke("claude_desktop_binding_finish", { id: operationId });
      if (result.state === "complete") {
        operationId = null; settle(true); view.busy(false); view.close(); return;
      }
      operationId = result.id; phase = "finish";
      message.textContent = result.instructionsKey ? translate(result.instructionsKey) : result.instructions || translate("linkedClientUi.signInToThisAccountInDesktop");
      button.textContent = translate("linkedClientUi.finishLinking");
      view.setStatus("", translate("linkedClientUi.waitingForDesktopLoginRunningCliSessions"));
    } catch (error) {
      view.setStatus("bad", clientError(error));
      if (phase === "finish") {
        try {
          const state = await invoke("local_clients_status", { kind: "claude", id });
          if (state.desktop?.binding?.state !== "pending") {
            operationId = null; phase = "start"; button.textContent = translate("accounts.linkAgain");
          }
        } catch { /* Keep the operation ID so cancellation can retry recovery. */ }
      }
    } finally { view.busy(false); button.disabled = false; }
  };
  return done;
}

/** Shared by the main window and tray; the backend owns close/write/launch ordering. */
export function switchLocalClients(account, onComplete) {
  const { id, kind } = account;
  if (activeSwitches.has(kind)) return activeSwitches.get(kind);
  const view = dialog(translate("linkedClientUi.switchAccount", NAMES[kind] || kind), "confirm");
  const message = paragraph(view, translate("linkedClientUi.detectingLocalClients"));
  const hint = paragraph(view, "");
  hint.className = "client-target-hint";
  hint.hidden = true;
  let status, submitting = false, forceButton = null;
  let attempt = null, timer, stopped = false, finished = false;
  let cancelAutoClose;
  const targets = Object.entries({ cli: translate("linkedClientUi.cliOnly"), desktop: translate("linkedClientUi.desktopOnly"), both: translate("linkedClientUi.bothClients") }).map(([target, label]) => {
    const button = action(view, label, "secondary client-switch-target");
    button.onclick = () => start(target);
    return { target, label, button };
  });
  let resolve;
  const done = new Promise(r => { resolve = r; });
  activeSwitches.set(kind, done);
  void done.finally(() => activeSwitches.delete(kind));
  view.onCancel(async () => {
    stopped = true; clearTimeout(timer);
    cancelAutoClose?.();
    if (attempt) await invoke("opencode_client_oauth_poll", { id: attempt, cancel: true });
    attempt = null; if (!finished) { finished = true; resolve(false); }
  });
  function available(endpoint) {
    return !!status?.[endpoint]?.installed && status[endpoint].switchable !== false && !status[endpoint].error;
  }
  function needsRecovery() {
    return kind === "claude" && status?.desktop?.binding?.state === "pending";
  }
  function canStart(target) {
    if (target === "both") return available("cli") && available("desktop") && !needsRecovery();
    return available(target);
  }
  function syncControls() {
    const showTargets = !attempt && !finished && !forceButton;
    view.cancel.hidden = showTargets;
    for (const { target, label, button } of targets) {
      button.hidden = !showTargets;
      button.disabled = !showTargets || submitting || !canStart(target);
      button.textContent = target === "desktop" && needsRecovery() ? translate("accounts.restoreOriginalLogin") : label;
      button.classList.toggle("danger", target !== "cli" && canStart(target) && !!status?.desktop?.running);
    }
  }
  function busy(value) {
    submitting = value;
    view.busy(value);
    syncControls();
  }
  async function loadStatus() {
    const detected = await invoke("local_clients_status", { kind, id });
    if (view.closed) return;
    status = detected;
    const notes = [];
    if (available("cli")) notes.push(translate("linkedClientUi.runningCliSessionsStayOpenNewSessions"));
    if (available("desktop")) {
      if (needsRecovery()) {
        notes.push(translate("linkedClientUi.anUnfinishedDesktopLinkExistsRestoringThe"));
      } else {
        notes.push(status.desktop.running
          ? translate("linkedClientUi.switchingWillRestartDesktopSaveYourWork")
          : translate("linkedClientUi.switchingWillStartDesktopClosingItFirst"));
        if (kind === "claude" && status.desktop.binding?.state !== "bound") {
          notes.push(translate("linkedClientUi.linkThisAccountSDesktopLoginBefore"));
        }
      }
    }
    message.textContent = notes.join(" ") || translate("linkedClientUi.noAvailableClientsCheckThePathsIn");
    const details = [];
    for (const [endpoint, label] of [["cli", "CLI"], ["desktop", "Desktop"]]) {
      if (available(endpoint)) continue;
      const error = status[endpoint]?.error;
      details.push(error ? `${label} · ${clientError(error)}` : translate("linkedClientUi.isTemporarilyUnavailable", label));
    }
    if (status.cli?.installed && status.desktop?.installed) {
      if (kind === "codex") details.push(translate("linkedClientUi.bothClientsShareCredentialsTheOtherClient"));
      if (kind === "opencode") details.push(translate("linkedClientUi.ifBothClientsShareACredentialStore"));
    }
    hint.textContent = details.join(" ");
    hint.hidden = !hint.textContent;
    syncControls();
  }
  async function complete(result) {
    attempt = null; stopped = true; clearTimeout(timer);
    view.content.replaceChildren();
    if (result.clients?.some(client => client.target === "cli")) paragraph(view, translate("linkedClientUi.runningCliSessionsStayOpenReopenA"));
    for (const client of result.clients || []) {
      const row = paragraph(view, resultText({ clients: [client] }));
      row.className = "client-endpoint-result";
      row.dataset.result = client.status;
    }
    const switched = result.switched !== false;
    const message = result.state === "cancelled" ? translate("linkedClientUi.authorizationCancelledOrTimedOut")
      : switched ? translate("linkedClientUi.switchCompleted") : translate("linkedClientUi.someClientsCouldNotBeSwitched");
    view.setStatus(switched ? "ok" : "bad", message);
    view.cancel.textContent = translate("common.close");
    if (forceButton && !switched) {
      syncControls();
      view.cancel.focus();
      return;
    }
    if (!finished) { finished = true; resolve(switched); }
    busy(false);
    if (switched && !view.closed) cancelAutoClose = startSwitchResultCountdown(view.cancel, view.close);
    view.cancel.focus();
    if (onComplete) await onComplete(result);
  }
  function addForceAction(target) {
    if (forceButton) return;
    const force = action(view, translate("linkedClientUi.useSavedCredentials"), "primary client-force-write");
    forceButton = force;
    syncControls();
    force.onclick = async () => {
      if (submitting || force.disabled || view.closed) return;
      force.disabled = true; busy(true);
      try {
        const result = await submitSwitch(target, true);
        if (result.switched !== false) {
          force.remove();
          forceButton = null;
        }
      }
      catch (failure) { view.setStatus("bad", clientError(failure)); }
      finally { busy(false); force.disabled = false; }
    };
  }
  async function handle(result) {
    if (result.state === "complete") { await complete(result); return; }
    if (result.state === "cancelled") { await complete({ ...result, switched: false }); return; }
    if (result.state !== "pending" || !result.id) throw new Error("opencode_cli_invalid_response");
    attempt = result.id;
    syncControls();
    if (result.url) {
      view.content.replaceChildren();
      paragraph(view, result.instructionsKey ? translate(result.instructionsKey) : result.instructions || translate("linkedClientUi.completeNativeAuthorizationForThisClientIn"));
      const url = document.createElement("input"); url.type = "text"; url.readOnly = true; url.value = result.url; url.setAttribute("aria-label", translate("linkedClientUi.authorizationUrl"));
      const copy = document.createElement("button"); copy.type = "button"; copy.className = "secondary"; copy.textContent = translate("linkedClientUi.copyAuthorizationUrl");
      copy.onclick = () => { void copyText(url.value).catch(error => view.setStatus("bad", clientError(error))); };
      view.content.append(url, copy);
    }
    view.setStatus(result.error ? "bad" : "", result.error ? clientError(result.error) : translate("linkedClientUi.waitingForAuthorization"));
    timer = setTimeout(tick, 3000);
  }
  async function tick() {
    if (stopped || view.closed) return;
    try {
      const result = await invoke("opencode_client_oauth_poll", { id: attempt, cancel: false });
      if (!stopped && !view.closed) await handle({ id: attempt, ...result });
    } catch (error) {
      if (stopped || view.closed) return;
      const code = error instanceof Error ? error.message : String(error);
      if (code === "opencode_oauth_expired") {
        await handle({ state: "cancelled" });
      } else {
        view.setStatus("bad", clientError(error));
        timer = setTimeout(tick, 3000);
      }
    }
  }
  async function submitSwitch(target, force) {
    const result = await invoke("local_clients_switch", { kind, id, target, closeDesktop: target !== "cli", force });
    if (kind === "codex" && result.clients?.some(client => String(client.error || "").startsWith("codex_refresh_denied"))) {
      addForceAction(target);
    }
    await handle(result);
    return result;
  }
  async function start(target) {
    if (submitting || attempt || finished || forceButton || view.closed || !canStart(target)) return;
    busy(true);
    view.setStatus("", "");
    try {
      if (target === "desktop" && needsRecovery()) {
        await invoke("claude_desktop_binding_cancel", { id: status.desktop.binding.operationId });
        await loadStatus();
        return;
      }
      if (kind === "claude" && target !== "cli" && status.desktop.binding?.state !== "bound") {
        if (!await bindClaudeDesktop(id)) return;
      }
      view.setStatus("", translate("linkedClientUi.switchingLocalLogin"));
      await submitSwitch(target, null);
    } catch (error) {
      view.setStatus("bad", clientError(error));
      if (kind === "codex" && errorCode(error).startsWith("codex_refresh_denied")) {
        addForceAction(target);
      }
    } finally { busy(false); }
  }
  syncControls();
  void loadStatus().catch(error => {
    if (view.closed) return;
    message.hidden = true;
    view.setStatus("bad", clientError(error));
  });
  return done;
}
