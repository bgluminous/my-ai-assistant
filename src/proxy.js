import { errorCode, translate } from "./i18n.js";
import { createModal } from "./modal.js";
import { el, invoke, copyText, resetError, fillStatus, toast } from "./shared.js";

// 代理设置弹窗：跟随系统 / 直连 / 自定义（http、https、socks5，可带账号密码）。
// 保存到用户目录 .xilore/myaiassistant/settings.json 的 proxy 字段，后端每次构建 HTTP 客户端时读取，保存后立即生效。

const state = { mode: "system", url: "", path: "" };
let button = null;
let modalController;
function getModal() {
  return modalController ||= createModal(el("#proxy-modal"));
}

function mapError(err) {
  const code = errorCode(err);
  if (code === "empty_proxy_url") return translate("proxy.enterACustomProxyAddress");
  if (code === "invalid_proxy_url") return translate("proxy.invalidProxyAddressExampleSocks5127");
  return resetError(code);
}

function setStatus(kind, text) {
  fillStatus(el("#proxy-status"), kind, text);
}
function clearStatus() {
  const box = el("#proxy-status");
  box.hidden = true;
  box.textContent = "";
}
function clearTestResult() {
  const box = el("#proxy-test-result");
  box.textContent = "";
  box.className = "muted small";
}

function applyMode(mode) {
  state.mode = mode;
  for (const seg of el("#proxy-mode").querySelectorAll(".seg")) {
    seg.classList.toggle("active", seg.dataset.mode === mode);
  }
  el("#proxy-url").disabled = mode !== "custom";
}

function render() {
  el("#proxy-url").value = state.url || "";
  applyMode(state.mode || "system");
}

function readForm() {
  return { mode: state.mode, url: el("#proxy-url").value.trim() };
}

async function refresh() {
  const view = await invoke("proxy_get");
  state.mode = view.mode || "system";
  state.url = view.url || "";
  state.path = view.path || "";
}

async function onTest() {
  const btn = el("#proxy-test");
  const result = el("#proxy-test-result");
  btn.disabled = true;
  result.className = "muted small";
  result.textContent = translate("proxy.testing");
  try {
    const r = await invoke("proxy_test", { config: readForm() });
    if (r.ok) {
      result.className = "small test-ok";
      result.textContent =
        translate("proxy.connectedIpRegionIspMs", r.ip || translate("proxy.unknown"), r.region || translate("proxy.unknown"), r.isp || translate("proxy.unknown"), r.ms);
    } else {
      result.className = "small test-bad";
      const reason = r.error ? r.error : r.status ? `HTTP ${r.status}` : translate("proxy.noResponse");
      result.textContent = translate("proxy.connectionFailedMs", reason, r.ms);
    }
  } catch (err) {
    result.className = "small test-bad";
    result.textContent = translate("proxy.testFailed", mapError(err));
  } finally {
    btn.disabled = false;
  }
}

async function onSave() {
  const btn = el("#proxy-save");
  const operation = getModal().beginSave([btn]);
  if (!operation) return;
  setStatus("", () => translate("accounts.saving"));
  try {
    const view = await invoke("proxy_set", { config: readForm() });
    if (!operation.current()) return;
    state.mode = view.mode;
    state.url = view.url;
    state.path = view.path;
    render();
    // 保存成功即关闭弹窗，结果用右上角 toast 提示；失败时保留弹窗与状态条
    const label = state.mode === "custom" ? state.url : state.mode === "direct" ? translate("proxy.direct") : translate("settings.followSystem");
    closeModal();
    toast("ok", () => translate("proxy.proxySavedNewRequestsUseItImmediately", label), { key: "proxy" });
  } catch (err) {
    if (!operation.current()) return;
    setStatus("bad", () => translate("accounts.saveFailed", mapError(err)));
  } finally {
    operation.finish();
  }
}

async function onCopyPath() {
  if (!state.path) return;
  const btn = el("#proxy-copy-path");
  try {
    await copyText(state.path);
    btn.textContent = translate("pricing.copied");
    setTimeout(() => (btn.textContent = translate("proxy.copyConfigurationPath")), 1200);
  } catch {
    setStatus("bad", () => translate("pricing.couldNotWriteToTheClipboard"));
  }
}

function openModal() {
  if (getModal().busy) return;
  getModal().open();
  clearStatus();
  clearTestResult();
  render();
}
function closeModal() {
  getModal().close();
}

export async function initProxy() {
  button = el("#proxy-btn");
  for (const seg of el("#proxy-mode").querySelectorAll(".seg")) {
    seg.addEventListener("click", () => {
      if (getModal().busy) return;
      applyMode(seg.dataset.mode);
      clearTestResult();
    });
  }
  el("#proxy-test").addEventListener("click", onTest);
  el("#proxy-save").addEventListener("click", onSave);
  el("#proxy-copy-path").addEventListener("click", onCopyPath);

  try {
    await refresh();
    button.addEventListener("click", openModal);
  } catch {
    button.disabled = true;
    button.title = translate("proxy.availableInTheDesktopApp");
  }
}
