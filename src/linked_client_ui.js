import { invoke, resetError, copyText } from "./shared.js";
import { dialog } from "./client_dialog.js";

const activeSwitches = new Map();
const NAMES = { codex: "GPT / Codex", claude: "Claude", opencode: "OpenCode" };
const ERRORS = {
  client_not_installed: "未检测到 CLI 或 Desktop，请在设置中配置客户端路径。",
  client_target_unavailable: "所选客户端已不可用，请检查路径或重新打开切换窗口。",
  client_operation_busy: "该供应商正在执行其他本机操作，请稍后重试。",
  client_path_invalid: "客户端路径无效，请在设置中修正。",
  codex_exe_invalid: "Codex Desktop 路径无效，请在设置中修正。",
  claude_exe_invalid: "Claude Desktop 路径无效，请在设置中修正。",
  opencode_exe_invalid: "OpenCode CLI 路径无效，请在设置中选择命令行程序。",
  opencode_desktop_exe_invalid: "OpenCode Desktop 路径无效，请在设置中修正。",
  account_not_verified: "查询凭据尚未验证，请刷新账户后重试此端切换。",
  claude_no_refresh_token: "CLI 缺少 Refresh Token，请编辑凭据或重新授权。",
  client_close_failed: "未能关闭 Desktop，请手动完全退出后重试。",
  client_close_required: "Desktop 已启动，请重新确认关闭后切换。",
  client_launch_failed: "启动失败，请检查客户端后重试。",
  client_launch_failed_rolled_back: "Desktop 启动失败，本端已恢复原登录。",
  client_switch_cancelled: "本端未执行切换。",
  claude_desktop_machine_unavailable: "无法确认快照所属电脑，请检查本机访问权限。",
  claude_keychain_unavailable: "无法读取 Claude 系统凭据，请检查钥匙串访问权限。",
  claude_keychain_rollback_failed: "Claude 钥匙串恢复失败，请检查本机登录。",
  opencode_helper_lifetime_failed: "无法建立 OpenCode 本机服务，请检查客户端和系统权限。",
  claude_identity_required: "请先刷新该账户，获取可校验的账户身份。",
  claude_desktop_account_mismatch: "Desktop 登录的账户与所选账户不符，已尝试恢复原登录。",
  claude_desktop_login_required: "未检测到有效的 Desktop 登录，请重新绑定。",
  claude_desktop_binding_required: "请先绑定此账户的 Desktop 登录。",
  claude_desktop_binding_pending: "Desktop 登录绑定尚未结束，请完成或取消绑定。",
  claude_desktop_binding_expired: "绑定已超时，已尝试恢复原登录。",
  claude_desktop_operation_stale: "绑定会话已结束，请重新打开。",
  claude_desktop_path_ambiguous: "检测到多个 Desktop 数据目录，请在设置中指定当前目录。",
  claude_desktop_path_invalid: "Desktop 数据目录无效，请在设置中修正。",
  claude_desktop_recovery_failed: "原登录恢复失败，恢复记录已保留，请重试恢复。",
  claude_desktop_snapshot_foreign: "此登录快照不属于当前电脑或数据目录，请重新绑定。",
  claude_desktop_snapshot_invalid: "登录快照无法读取，请重新绑定。",
  claude_desktop_snapshot_too_large: "Desktop 登录数据过大，本次未完成绑定。",
  codex_auth_store_unsupported: "当前 Codex 认证存储模式不支持持久切换。",
  codex_keyring_unavailable: "无法访问 Codex 系统凭据库，请检查访问权限。",
  codex_auth_rollback_failed: "Codex 登录恢复失败，请检查本机登录后重试。",
  opencode_v2_required: "需要 OpenCode V2 客户端。",
  opencode_gui_is_not_cli: "CLI 路径填写了 Desktop 程序，请分别配置。",
  opencode_desktop_cli_not_found: "未找到 Desktop 内置 CLI，请检查安装目录。",
  opencode_client_auth_pending: "OpenCode 原生授权尚未结束，请完成或取消授权。",
  opencode_workspace_mismatch: "授权工作区与所选账户不符，已尝试恢复原登录。",
  opencode_switch_restore_failed: "OpenCode 原登录未能恢复，请重试取消授权。",
  opencode_cli_timeout: "OpenCode 客户端响应超时，请稍后重试。",
};
export function clientError(error) {
  const text = resetError(error);
  if (text.startsWith("codex_refresh_denied")) return "登录凭据续期失败，可重新授权或尝试写入保存的凭据副本。";
  return ERRORS[text] || text;
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
    client.status === "success" ? "切换成功" : clientError(client.error || "切换失败")}`).join("\n");
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
    button.textContent = `关闭（${seconds}s）`;
    timer = setTimeout(tick, remainingMs - (seconds - 1) * 1000);
  }
  tick();
  return () => clearTimeout(timer);
}

/** Resolves only after binding completes or cancellation has restored the login. */
export function bindClaudeDesktop(id) {
  const view = dialog("绑定 Claude Desktop 登录");
  const message = paragraph(view, "将暂时关闭 Desktop 并保存原登录。请在官方客户端登录此账户；完成或取消绑定后恢复原登录。");
  const button = action(view, "开始绑定");
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
      message.textContent = result.instructions || "请在 Desktop 登录此账户，然后完成绑定。";
      button.textContent = "完成绑定";
      view.setStatus("", "等待 Desktop 登录；运行中的 CLI 会话保留。");
    } catch (error) {
      view.setStatus("bad", clientError(error));
      if (phase === "finish") {
        try {
          const state = await invoke("local_clients_status", { kind: "claude", id });
          if (state.desktop?.binding?.state !== "pending") {
            operationId = null; phase = "start"; button.textContent = "重新绑定";
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
  const view = dialog(`切换 ${NAMES[kind] || kind} 账户`, "confirm");
  const message = paragraph(view, "正在检测本机客户端…");
  const hint = paragraph(view, "");
  hint.className = "client-target-hint";
  hint.hidden = true;
  let status, submitting = false, forceButton = null;
  let attempt = null, timer, stopped = false, finished = false;
  let cancelAutoClose;
  const targets = Object.entries({ cli: "仅 CLI", desktop: "仅 Desktop", both: "两端一起" }).map(([target, label]) => {
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
      button.textContent = target === "desktop" && needsRecovery() ? "恢复原登录" : label;
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
    if (available("cli")) notes.push("运行中的 CLI 会话保留，新会话使用新账户。");
    if (available("desktop")) {
      if (needsRecovery()) {
        notes.push("有未完成的 Desktop 绑定。恢复原登录会关闭桌面应用。");
      } else {
        notes.push(status.desktop.running
          ? "切换 Desktop 会关闭并重启桌面应用，请先保存工作。"
          : "切换 Desktop 会启动桌面应用，已打开时会先关闭。");
        if (kind === "claude" && status.desktop.binding?.state !== "bound") {
          notes.push("首次切换 Desktop 需要绑定此账户的桌面登录。");
        }
      }
    }
    message.textContent = notes.join(" ") || "没有可切换的客户端，请检查设置中的路径。";
    const details = [];
    for (const [endpoint, label] of [["cli", "CLI"], ["desktop", "Desktop"]]) {
      if (available(endpoint)) continue;
      const error = status[endpoint]?.error;
      details.push(error ? `${label} · ${clientError(error)}` : `${label} 暂不可用。`);
    }
    if (status.cli?.installed && status.desktop?.installed) {
      if (kind === "codex") details.push("两端共用登录凭据，另一端下次读取时也可能使用新账户。");
      if (kind === "opencode") details.push("若两端共用登录存储，另一端下次读取时也可能使用新账户。");
    }
    hint.textContent = details.join(" ");
    hint.hidden = !hint.textContent;
    syncControls();
  }
  async function complete(result) {
    attempt = null; stopped = true; clearTimeout(timer);
    view.content.replaceChildren();
    if (result.clients?.some(client => client.target === "cli")) paragraph(view, "运行中的 CLI 会话保留；如未切换，请重新打开会话。");
    for (const client of result.clients || []) {
      const row = paragraph(view, resultText({ clients: [client] }));
      row.className = "client-endpoint-result";
      row.dataset.result = client.status;
    }
    const switched = result.switched !== false;
    const message = result.state === "cancelled" ? "授权已取消或超时。"
      : switched ? "已完成切换。" : "切换未全部完成。";
    view.setStatus(switched ? "ok" : "bad", message);
    view.cancel.textContent = "关闭";
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
    const force = action(view, "写入保存的凭据副本", "primary client-force-write");
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
      paragraph(view, result.instructions || "在浏览器中完成此客户端的原生授权。");
      const url = document.createElement("input"); url.type = "text"; url.readOnly = true; url.value = result.url; url.setAttribute("aria-label", "授权地址");
      const copy = document.createElement("button"); copy.type = "button"; copy.className = "secondary"; copy.textContent = "复制授权地址";
      copy.onclick = () => { void copyText(url.value).catch(error => view.setStatus("bad", clientError(error))); };
      view.content.append(url, copy);
    }
    view.setStatus(result.error ? "bad" : "", result.error ? clientError(result.error) : "等待授权完成…");
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
      view.setStatus("", "正在切换本机登录…");
      await submitSwitch(target, null);
    } catch (error) {
      view.setStatus("bad", clientError(error));
      if (kind === "codex" && resetError(error).startsWith("codex_refresh_denied")) {
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
