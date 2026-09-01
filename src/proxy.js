import { el, invoke, copyText, resetError, fillStatus } from "./shared.js";

// 代理设置弹窗：跟随系统 / 直连 / 自定义（http、https、socks5，可带账号密码）。
// 保存到用户目录 xilore/myaiassistant/settings.json 的 proxy 字段，后端每次构建 HTTP 客户端时读取，保存后立即生效。

const state = { mode: "system", url: "", path: "" };
let button = null;

function mapError(err) {
  const code = resetError(err);
  if (code === "empty_proxy_url") return "请填写自定义代理地址。";
  if (code === "invalid_proxy_url") return "代理地址无效，示例：socks5://127.0.0.1:1080";
  return code;
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
  result.textContent = "测试中…";
  try {
    const r = await invoke("proxy_test", { config: readForm() });
    if (r.ok) {
      result.className = "small test-ok";
      result.textContent = `已连通 · 出口 IP ${r.ip || "未知"} · ${r.ms}ms`;
    } else {
      result.className = "small test-bad";
      const reason = r.error ? r.error : r.status ? `HTTP ${r.status}` : "无响应";
      result.textContent = `连接失败：${reason} · ${r.ms}ms`;
    }
  } catch (err) {
    result.className = "small test-bad";
    result.textContent = `测试失败：${mapError(err)}`;
  } finally {
    btn.disabled = false;
  }
}

async function onSave() {
  const btn = el("#proxy-save");
  btn.disabled = true;
  setStatus("", "保存中…");
  try {
    const view = await invoke("proxy_set", { config: readForm() });
    state.mode = view.mode;
    state.url = view.url;
    state.path = view.path;
    render();
    const label = state.mode === "custom" ? state.url : state.mode === "direct" ? "直连" : "跟随系统";
    setStatus("ok", `已保存（${label}），后续请求立即生效。`);
  } catch (err) {
    setStatus("bad", `保存失败：${mapError(err)}`);
  } finally {
    btn.disabled = false;
  }
}

async function onCopyPath() {
  if (!state.path) return;
  const btn = el("#proxy-copy-path");
  try {
    await copyText(state.path);
    btn.textContent = "已复制";
    setTimeout(() => (btn.textContent = "复制配置路径"), 1200);
  } catch {
    setStatus("bad", "复制失败，无法写入剪贴板。");
  }
}

function openModal() {
  el("#proxy-modal").hidden = false;
  clearStatus();
  clearTestResult();
  render();
}
function closeModal() {
  el("#proxy-modal").hidden = true;
}

export async function initProxy() {
  button = el("#proxy-btn");
  const modal = el("#proxy-modal");

  for (const node of modal.querySelectorAll("[data-close]")) {
    node.addEventListener("click", closeModal);
  }
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) closeModal();
  });
  for (const seg of el("#proxy-mode").querySelectorAll(".seg")) {
    seg.addEventListener("click", () => {
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
    button.title = "需在应用内使用";
  }
}
