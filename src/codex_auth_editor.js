import { el, invoke, resetError, fillStatus } from "./shared.js";

export function codexAuthErrorMessage(error) {
  const code = resetError(error);
  const pos = /^codex_auth_json:(\d+):(\d+)$/.exec(code);
  const messages = {
    codex_auth_conflict: "此账户的凭据已更新。草稿已保留，重新载入会替换凭据草稿。",
    codex_auth_object: "完整 Auth 必须是 JSON 对象。",
    codex_auth_access_token: "请填写有效的 tokens.access_token。",
    codex_auth_refresh_token: "tokens.refresh_token 应为字符串或 null。",
    codex_auth_unreadable: "无法读取此账户的 Auth 副本。",
    codex_auth_missing: "此账户还没有可读取的 Auth 凭据。",
    codex_auth_file_read: "无法读取所选文件，请检查文件内容和读取权限。",
    codex_auth_file_missing: "请选择 auth.json 文件。",
    codex_auth_text_missing: "请粘贴完整的 auth.json 内容。",
    duplicate_account: "该账号已存在，请检查凭据所属的账户。",
    not_found: "此账户已被删除。",
  };
  return pos ? `JSON 格式错误，第 ${pos[1]} 行，第 ${pos[2]} 列。` : messages[code];
}

// 一个编辑会话只持有当前账户的草稿。关闭后到达的 IPC 回复不得再写入表单。
export function createCodexAuthEditor({ id, codexAuthRevision }) {
  const access = el("#account-token"), refresh = el("#account-refresh-token");
  const raw = el("#account-auth-json"), status = el("#account-auth-status");
  const reload = el("#account-auth-reload"), save = el("#account-save");
  const tokenMode = el("#account-mode-token"), authMode = el("#account-mode-auth");
  let active = true, busy = false, mode = "token";
  let revision = codexAuthRevision, original = null;
  let projected = fields();

  function showMode(next) {
    mode = next;
    const full = mode === "auth";
    el("#account-token-fields").hidden = full;
    el("#account-auth-field").hidden = !full;
    for (const [button, pressed] of [[tokenMode, !full], [authMode, full]]) {
      button.classList.toggle("active", pressed);
      button.setAttribute("aria-pressed", String(pressed));
    }
  }

  function fields() {
    return { accessToken: access.value.trim(), refreshToken: refresh.value.trim() || null };
  }
  function tokenPatch() {
    const current = fields();
    return current.accessToken === projected.accessToken && current.refreshToken === projected.refreshToken ? null : current;
  }
  function showError(error) {
    const code = resetError(error);
    const message = codexAuthErrorMessage(error);
    if (!message) return false;
    fillStatus(status, "bad", message);
    reload.hidden = code !== "codex_auth_conflict";
    if (active) status.scrollIntoView?.({ block: "nearest" });
    return true;
  }
  function clear() {
    status.hidden = true;
    status.textContent = "";
    reload.hidden = true;
  }
  // 只从 JSON 读取字符串。修改与格式化由后端完成，扩展字段中的大整数不经过 JS 序列化。
  function syncFields(text) {
    const { tokens } = JSON.parse(text);
    access.value = typeof tokens?.access_token === "string" ? tokens.access_token : "";
    refresh.value = typeof tokens?.refresh_token === "string" ? tokens.refresh_token : "";
  }
  function project(patch) {
    return invoke("codex_auth_project", { text: raw.value, patch });
  }

  async function switchMode(next, discard = false) {
    if (!active || busy || (next === mode && !discard)) return;
    busy = true;
    let switched = false;
    const controls = [save, access, refresh, raw, tokenMode, authMode, reload];
    controls.forEach(n => { n.disabled = true; });
    clear();
    try {
      if (original === null || discard) {
        const snapshot = await invoke("codex_auth_read", { id });
        if (!active) return;
        const patch = discard ? null : tokenPatch();
        if (patch && revision && revision !== snapshot.revision) throw new Error("codex_auth_conflict");
        // 验证/合并成功后再替换草稿，失败时保留用户输入。
        const merged = patch ? await invoke("codex_auth_project", { text: snapshot.text, patch }) : null;
        if (!active) return;
        revision = snapshot.revision;
        original = snapshot.text;
        raw.value = merged?.text ?? snapshot.text;
        syncFields(raw.value);
      } else {
        const patch = mode === "token" ? tokenPatch() : null;
        if (mode === "auth" || patch) {
          const result = await project(patch);
          if (!active) return;
          // 只有简易字段被改动时才重写 JSON，来回查看保留粘贴的原文。
          if (patch) raw.value = result.text;
          access.value = result.accessToken;
          refresh.value = result.refreshToken || "";
        }
      }
      projected = fields();
      showMode(next);
      switched = true;
    } catch (error) {
      if (active && !showError(error)) {
        fillStatus(status, "bad", "无法载入或转换 Auth，请重试。");
      }
      if (active && discard) reload.hidden = false;
    } finally {
      if (active) {
        busy = false;
        controls.forEach(n => { n.disabled = false; });
        if (switched) (mode === "auth" ? raw : access).focus({ preventScroll: true });
      }
    }
  }

  // 以前端读取时的版本进行乐观锁校验。仅查看或格式化 JSON 不提交任何凭据。
  async function changes() {
    const patch = mode === "token" ? tokenPatch() : null;
    if (original === null) {
      return patch ? {
        updateCredentials: true, expectedAuthRevision: revision,
        token: patch.accessToken, refreshToken: patch.refreshToken,
      } : null;
    }
    if (!patch && raw.value === original) return null;
    const result = await project(patch);
    if (result.text === original) return null;
    return { updateCredentials: true, expectedAuthRevision: revision, codexAuthText: result.text };
  }

  showMode("token");
  clear();
  return {
    switchMode, changes, showError, clear,
    get busy() { return busy; },
    get full() { return mode === "auth"; },
    dispose() {
      active = false;
      for (const n of [save, raw, access, refresh, tokenMode, authMode, reload]) n.disabled = false;
      raw.value = "";
      original = null;
      clear();
      showMode("token");
    },
  };
}
