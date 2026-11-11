import { el, invoke, resetError, copyText } from "./shared.js";
import { createModal } from "./modal.js";

let dialogSequence = 0;
function dialog(title) {
  const root = document.createElement("dialog");
  root.className = "opencode-dialog";
  root.innerHTML = '<div class="modal-head"><h3></h3><button type="button" class="icon-btn" aria-label="关闭">×</button></div><div class="modal-body"><div class="opencode-content"></div><p class="status" role="status" hidden></p></div><div class="modal-foot"><button type="button" class="secondary">取消</button></div>';
  const heading = root.querySelector("h3");
  heading.id = "opencode-dialog-title-" + (++dialogSequence);
  heading.textContent = title;
  root.setAttribute("aria-labelledby", heading.id);
  const content = root.querySelector(".opencode-content");
  const status = root.querySelector(".status");
  const foot = root.querySelector(".modal-foot");
  const cancel = foot.querySelector("button");
  let closed = false, busy = false, operation = null;
  let onCancel = async () => {};
  const modal = createModal(root, {
    isBusy: () => busy,
    onRequestClose: requestClose,
    onClose() { closed = true; root.remove(); void onCancel().catch(() => {}); },
  });
  function setStatus(kind, text) {
    status.className = "status" + (kind ? " " + kind : "");
    status.textContent = text;
    status.hidden = !text;
  }
  function close() {
    modal.close();
  }
  async function requestClose() {
    busy = true; cancel.disabled = true;
    try { await onCancel(); close(); }
    catch (error) { setStatus("bad", resetError(error)); }
    finally { busy = false; cancel.disabled = false; }
  }
  root.querySelector(".icon-btn").setAttribute("data-close", "");
  cancel.setAttribute("data-close", "");
  document.body.append(root); modal.open();
  return { root, content, setStatus, foot, cancel, close,
    get closed() { return closed; },
    busy(value) {
      if (value && !busy) operation = modal.beginSave();
      if (!value) { operation?.finish(); operation = null; }
      busy = value;
      modal.reset();
    },
    onCancel(fn) { onCancel = fn; },
  };
}

/** 独立授权与客户端原生授权共用显示；各自取消命令互不串用。 */
async function authorize({ title, begin, poll, cancel, onComplete }) {
  const view = dialog(title);
  let attempt = null, timer, stopped = false, cancelConfirmed = false;
  view.setStatus("", "正在准备授权…");
  view.busy(true);
  view.onCancel(async () => {
    if (cancelConfirmed) return;
    // 停止轮询与取消成功分别记录：取消失败时仍保留 attempt，下一次关闭必须再次请求。
    stopped = true; clearTimeout(timer);
    if (attempt) {
      await cancel(attempt.id);
      cancelConfirmed = true;
    }
  });
  async function complete(result) {
    stopped = true; cancelConfirmed = true;
    view.busy(true);
    view.setStatus("ok", "已完成。");
    view.busy(false); view.close();
    if (onComplete) await onComplete(result);
  }
  async function tick() {
    if (stopped || view.closed) return;
    try {
      const result = await poll(attempt.id);
      if (stopped || view.closed) return;
      if (result.state === "complete") { await complete(result); return; }
      timer = setTimeout(tick, Math.max(1000, Number(attempt.interval || 3) * 1000));
    } catch (error) {
      if (stopped || view.closed) return;
      view.setStatus("bad", resetError(error));
      // 暂态网络失败可继续同一设备码；其它错误由用户关闭后重新授权。
      if (/opencode_(network_error|(?:oauth_)?http_(?:5\d\d|429))/.test(String(error))) timer = setTimeout(tick, 5000);
    }
  }
  try {
    attempt = await begin();
    if (stopped || view.closed) {
      if (attempt.state !== "complete") await cancel(attempt.id);
      cancelConfirmed = true;
      return;
    }
    if (attempt.state === "complete") { await complete(attempt); return; }
    const message = document.createElement("p");
    message.textContent = attempt.instructions || ("在浏览器中完成授权，验证码：" + (attempt.code || ""));
    const url = document.createElement("input");
    url.type = "text";
    url.readOnly = true; url.value = attempt.url || ""; url.setAttribute("aria-label", "授权地址");
    const copy = document.createElement("button");
    copy.type = "button"; copy.className = "secondary"; copy.textContent = "复制授权地址";
    copy.onclick = () => { void copyText(url.value).then(() => { view.setStatus("ok", "授权地址已复制。"); }).catch(error => { view.setStatus("bad", resetError(error)); }); };
    view.content.append(message, url, copy);
    view.setStatus("", attempt.opened ? "等待浏览器授权完成…" : "请复制授权地址到浏览器打开。");
    view.busy(false);
    timer = setTimeout(tick, Math.max(1000, Number(attempt.interval || 3) * 1000));
  } catch (error) {
    view.busy(false); view.setStatus("bad", resetError(error));
  }
}

/** 添加先选择来源；本机只列出身份，选定工作区后才执行导入。 */
export function addOpenCode(onComplete) {
  const view = dialog("添加 OpenCode 账户");
  const noteLabel = document.createElement("label");
  const note = document.createElement("input");
  note.type = "text";
  note.id = "opencode-add-note-" + dialogSequence;
  noteLabel.htmlFor = note.id; noteLabel.textContent = "备注";
  note.placeholder = "备注（选填，留空使用邮箱）";
  note.spellcheck = false;
  const methods = document.createElement("div");
  methods.className = "opencode-add-methods";
  const local = document.createElement("button");
  local.type = "button"; local.className = "secondary"; local.textContent = "从本机导入";
  const oauth = document.createElement("button");
  oauth.type = "button"; oauth.className = "secondary"; oauth.textContent = "新增 OAuth 授权";
  methods.append(local, oauth);
  const localFields = document.createElement("div");
  localFields.className = "opencode-local-fields"; localFields.hidden = true;
  const workspaceLabel = document.createElement("label");
  const workspaces = document.createElement("select");
  workspaces.id = "opencode-local-workspace-" + dialogSequence;
  workspaceLabel.htmlFor = workspaces.id; workspaceLabel.textContent = "本机账户";
  const help = document.createElement("p");
  help.className = "muted small";
  help.textContent = "导入后可在编辑中完成独立授权，查询套餐与用量。";
  localFields.append(workspaceLabel, workspaces, help);
  view.content.append(noteLabel, note, methods, localFields);
  const save = document.createElement("button");
  save.type = "button"; save.className = "primary"; save.textContent = "导入账户"; save.hidden = true;
  view.foot.append(save);
  let loading = false;
  function busy(value) {
    loading = value; view.busy(value);
    for (const control of [local, oauth, save, workspaces, note]) control.disabled = value;
  }
  local.onclick = async () => {
    if (loading) return;
    busy(true); view.setStatus("", "正在读取本机账户…");
    try {
      const identities = await invoke("opencode_local_accounts");
      workspaces.replaceChildren();
      for (const identity of identities) {
        const option = document.createElement("option");
        option.value = identity.workspaceId;
        option.textContent = (identity.name || identity.workspaceId) + (identity.email ? " · " + identity.email : "");
        workspaces.append(option);
      }
      localFields.hidden = !identities.length; save.hidden = !identities.length;
      view.setStatus("", identities.length ? "" : "未检测到本机已登录的 OpenCode V2 账户。");
    } catch (error) {
      view.setStatus("bad", resetError(error));
    } finally { busy(false); }
  };
  save.onclick = async () => {
    if (save.disabled || view.closed) return;
    if (loading || !workspaces.value) return;
    busy(true); view.setStatus("", "正在导入…");
    try {
      const result = await invoke("accounts_import_local", { kind: "opencode", workspaceId: workspaces.value, note: note.value.trim() });
      view.close();
      if (onComplete) await onComplete((result.imported || []).map(item => item.id));
    } catch (error) {
      view.setStatus("bad", resetError(error));
    } finally { busy(false); }
  };
  oauth.onclick = () => {
    if (loading) return;
    void authorizeOpenCode(null, async result => {
      view.close();
      if (onComplete) await onComplete(result?.account ? [result.account.id] : []);
    }, note.value.trim());
  };
  note.focus();
}

export function authorizeOpenCode(workspaceId, onComplete, note = null) {
  return authorize({
    title: "授权 OpenCode 工作区",
    begin: () => invoke("opencode_oauth_start", { workspaceId: workspaceId || null, note }),
    poll: id => invoke("opencode_oauth_poll", { id }),
    cancel: id => invoke("opencode_oauth_cancel", { id }),
    onComplete,
  });
}

export function switchOpenCode(id, onComplete) {
  return authorize({
    title: "切换 OpenCode 客户端",
    begin: () => invoke("opencode_switch_local", { id }),
    poll: id => invoke("opencode_client_oauth_poll", { id, cancel: false }),
    cancel: id => invoke("opencode_client_oauth_poll", { id, cancel: true }),
    onComplete,
  });
}

export function editOpenCode(account, onComplete) {
  const view = dialog("编辑 OpenCode 账户");
  const fields = document.createElement("div");
  fields.className = "opencode-edit-fields";
  const label = document.createElement("label");
  label.textContent = "备注";
  const note = document.createElement("input");
  note.type = "text";
  note.id = "opencode-note-" + dialogSequence; label.htmlFor = note.id;
  note.value = account.noteAuto ? "" : account.note || "";
  note.placeholder = "备注（选填，留空使用邮箱）";
  note.spellcheck = false;
  const intervalLabel = document.createElement("label");
  intervalLabel.textContent = "同步间隔";
  const interval = el("#account-refresh-interval").cloneNode(true);
  interval.id = "opencode-refresh-interval-" + dialogSequence;
  intervalLabel.htmlFor = interval.id;
  interval.value = account.refreshIntervalMinutes == null ? "" : String(account.refreshIntervalMinutes);
  const intervalHint = document.createElement("p");
  intervalHint.className = "muted small";
  intervalHint.textContent = "独立间隔覆盖全局，刷新此账户的状态与工作区用量。";
  const intervalField = document.createElement("div");
  intervalField.className = "account-interval-field";
  intervalField.append(intervalLabel, interval, intervalHint);
  fields.append(label, note, intervalField);
  const workspace = document.createElement("p");
  workspace.className = "muted small opencode-edit-workspace"; workspace.textContent = "工作区：" + account.token;
  const auth = document.createElement("button");
  auth.type = "button"; auth.className = "secondary"; auth.textContent = account.opencodeAuthorized ? "重新授权" : "独立授权";
  auth.onclick = () => { void authorizeOpenCode(account.token, onComplete); };
  view.content.append(fields, workspace, auth);
  const save = document.createElement("button");
  save.type = "button"; save.className = "primary"; save.textContent = "保存";
  save.onclick = async () => {
    if (save.disabled || view.closed) return;
    view.busy(true); save.disabled = true; auth.disabled = true;
    try {
      await invoke("accounts_update", { id: account.id, note: note.value.trim(), token: account.token,
        refreshToken: null, ignoreApiModels: false,
        refreshIntervalMinutes: interval.value === "" ? null : Number(interval.value) });
      view.close();
      if (onComplete) await onComplete();
    } catch (error) { view.setStatus("bad", resetError(error)); }
    finally { view.busy(false); save.disabled = false; auth.disabled = false; }
  };
  view.foot.append(save); note.focus();
}
