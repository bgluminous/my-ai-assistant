import { el, invoke, copyText, resetError, fillStatus, fmtDateMs, escapeHtml } from "./shared.js";
import { notifyPricingChanged } from "./usage_data.js";

// 价格表编辑弹窗：读取“默认 + 在线 + 用户覆盖”的有效表，逐模型编辑 输入/输出/缓存读/缓存写，
// 覆盖写入用户目录 .xilore/myaiassistant/settings.json 的 pricing 字段（后端只写与基础层不同的条目）。
// 「在线更新」把远端表写入 settings.json 的 pricingRemote 缓存层（默认 < 在线 < 用户自定义）；
// 启动时静默检测一次，仅提示存在更新，是否应用由用户手动决定。
// 保存 / 重置 / 在线更新成功后通过 notifyPricingChanged 作废各窗口的用量聚合缓存并让用量页重算，
// 否则缓存有效期内等价费用仍按旧价显示。

const state = {
  path: "",
  note: null,
  entries: [],
  remote: { url: "", defaultUrl: "", fetchedAtMs: null, models: 0 },
  update: { available: false, models: 0 },
};
let chip = null;

const SOURCE_LABEL = { default: "默认", remote: "在线", custom: "自定义" };

function numToStr(v) {
  return v === null || v === undefined || !Number.isFinite(v) ? "0" : String(v);
}

function setStatus(kind, text) {
  fillStatus(el("#pricing-status"), kind, text);
}
function clearStatus() {
  const box = el("#pricing-status");
  box.hidden = true;
  box.textContent = "";
}

function setChip(count) {
  if (chip) chip.textContent = `价格表：${count} 个模型`;
}

function applyView(view) {
  state.path = view.path || "";
  state.note = view.note ?? null;
  state.remote = {
    url: view.remoteUrl || "",
    defaultUrl: view.remoteDefaultUrl || "",
    fetchedAtMs: view.remoteFetchedAtMs ?? null,
    models: Number(view.remoteModels) || 0,
  };
  state.entries = (view.models || []).map((m) => ({
    key: m.key,
    input: Number(m.input) || 0,
    output: Number(m.output) || 0,
    cacheRead: Number(m.cacheRead) || 0,
    cacheWrite: Number(m.cacheWrite) || 0,
    source: m.source === "custom" || m.source === "remote" ? m.source : "default",
  }));
}

async function load() {
  const view = await invoke("pricing_get");
  applyView(view);
}

function priceCell(field, val) {
  return `<td class="num"><input class="price-input" type="number" min="0" step="any" inputmode="decimal" data-field="${field}" value="${numToStr(val)}" /></td>`;
}

function render() {
  const query = el("#pricing-search").value.trim().toLowerCase();
  const body = el("#pricing-body");
  body.replaceChildren();
  let shown = 0;
  state.entries.forEach((e, i) => {
    if (query && !e.key.toLowerCase().includes(query)) return;
    shown += 1;
    const tr = document.createElement("tr");
    tr.dataset.index = String(i);
    tr.innerHTML = `
      <td><span class="pricing-model">${escapeHtml(e.key)}</span></td>
      ${priceCell("input", e.input)}
      ${priceCell("output", e.output)}
      ${priceCell("cacheRead", e.cacheRead)}
      ${priceCell("cacheWrite", e.cacheWrite)}
      <td><span class="tag ${e.source === "default" ? "" : e.source}">${SOURCE_LABEL[e.source] || "默认"}</span></td>
      <td class="col-actions"><button class="table-button" type="button" data-remove aria-label="删除 ${escapeHtml(e.key)}">✕</button></td>`;
    body.append(tr);
  });
  el("#pricing-empty").hidden = shown > 0;
  el("#pricing-count").textContent = `${state.entries.length} 个模型`;
}

function onEdit(ev) {
  const input = ev.target.closest("input.price-input");
  if (!input) return;
  const tr = input.closest("tr");
  if (!tr) return;
  const i = Number(tr.dataset.index);
  const entry = state.entries[i];
  if (!entry) return;
  const num = parseFloat(input.value);
  entry[input.dataset.field] = Number.isFinite(num) && num >= 0 ? num : 0;
  entry.source = "custom";
  const badge = tr.querySelector(".tag");
  if (badge) {
    badge.textContent = "自定义";
    badge.className = "tag custom";
  }
}

function onRemove(ev) {
  const btn = ev.target.closest("button[data-remove]");
  if (!btn) return;
  const tr = btn.closest("tr");
  if (!tr) return;
  const i = Number(tr.dataset.index);
  if (Number.isInteger(i)) {
    state.entries.splice(i, 1);
    render();
  }
}

function onAdd() {
  const key = el("#add-key").value.trim().toLowerCase();
  if (!key) {
    setStatus("bad", "请先填写模型名。");
    return;
  }
  if (state.entries.some((e) => e.key.toLowerCase() === key)) {
    setStatus("bad", `模型「${key}」已存在，可直接在表中修改。`);
    return;
  }
  const num = (id) => {
    const v = parseFloat(el(id).value);
    return Number.isFinite(v) && v >= 0 ? v : 0;
  };
  state.entries.push({
    key,
    input: num("#add-input"),
    output: num("#add-output"),
    cacheRead: num("#add-cacheRead"),
    cacheWrite: num("#add-cacheWrite"),
    source: "custom",
  });
  for (const id of ["#add-key", "#add-input", "#add-output", "#add-cacheRead", "#add-cacheWrite"]) {
    el(id).value = "";
  }
  el("#pricing-search").value = "";
  render();
  clearStatus();
}

async function onSave() {
  const btn = el("#pricing-save");
  btn.disabled = true;
  setStatus("", "保存中…");
  try {
    const models = state.entries.map((e) => ({
      key: e.key,
      input: e.input,
      output: e.output,
      cacheRead: e.cacheRead,
      cacheWrite: e.cacheWrite,
    }));
    const status = await invoke("pricing_save", { models, note: state.note ?? null });
    notifyPricingChanged();
    await load();
    render();
    setChip(status.models);
    setStatus("ok", `已保存，共 ${status.models} 个模型生效，用量统计已按新价重算。`);
  } catch (err) {
    setStatus("bad", `保存失败：${resetError(err)}`);
  } finally {
    btn.disabled = false;
  }
}

async function onReset() {
  const btn = el("#pricing-reset");
  btn.disabled = true;
  setStatus("", "重置中…");
  try {
    const view = await invoke("pricing_reset");
    notifyPricingChanged();
    applyView(view);
    render();
    syncUpdateArea();
    setChip(view.count);
    setStatus("ok", "已恢复为内置默认价格表（同时清除了在线表缓存），用量统计已按默认价重算。");
    checkUpdate(); // 重置后重新检测：在线表若与默认表不同会再次提示
  } catch (err) {
    setStatus("bad", `重置失败：${resetError(err)}`);
  } finally {
    btn.disabled = false;
  }
}

/** 把 remote / 更新检测状态同步到弹窗的在线更新区与 chip 角标。 */
function syncUpdateArea() {
  const input = el("#pricing-update-url");
  input.value = state.remote.url;
  input.placeholder = state.remote.defaultUrl ? `默认：${state.remote.defaultUrl}` : "";
  el("#pricing-update-meta").textContent = state.remote.fetchedAtMs
    ? `上次更新：${fmtDateMs(state.remote.fetchedAtMs)} · 在线表 ${state.remote.models} 个模型`
    : "尚未在线更新，当前使用内置默认表。";
  const hint = el("#pricing-update-hint");
  hint.hidden = !state.update.available;
  if (state.update.available) {
    hint.textContent = `检测到在线价格表有更新（远端 ${state.update.models} 个模型），点击「在线更新」应用。`;
  }
  if (chip) {
    chip.classList.toggle("has-update", state.update.available);
    chip.title = state.update.available
      ? "检测到在线价格表有更新，点击查看"
      : "点击查看 / 编辑价格表";
  }
}

async function onUpdate() {
  const btn = el("#pricing-update-btn");
  btn.disabled = true;
  setStatus("", "正在拉取在线价格表…");
  try {
    const url = el("#pricing-update-url").value.trim();
    const view = await invoke("pricing_update_apply", { url: url || null });
    notifyPricingChanged();
    applyView(view);
    state.update = { available: false, models: 0 };
    render();
    syncUpdateArea();
    setChip(state.entries.length);
    setStatus("ok", `在线价格表已更新：${state.remote.models} 个模型，用量统计已按新价重算。`);
  } catch (err) {
    setStatus("bad", `在线更新失败：${resetError(err)}`);
  } finally {
    btn.disabled = false;
  }
}

/** 静默检测一次是否有在线更新（用已保存地址）；网络失败不打扰用户。 */
async function checkUpdate() {
  try {
    const res = await invoke("pricing_update_check", { url: null });
    state.update = { available: !!res.hasUpdate, models: Number(res.models) || 0 };
  } catch {
    state.update = { available: false, models: 0 };
  }
  syncUpdateArea();
}

async function onCopyPath() {
  if (!state.path) return;
  const btn = el("#pricing-copy-path");
  try {
    await copyText(state.path);
    btn.textContent = "已复制";
    setTimeout(() => (btn.textContent = "复制路径"), 1200);
  } catch {
    setStatus("bad", "复制失败，无法写入剪贴板。");
  }
}

function openModal() {
  el("#pricing-modal").hidden = false;
  clearStatus();
  render();
  el("#pricing-search").focus();
}
function closeModal() {
  el("#pricing-modal").hidden = true;
}

export async function initPricing() {
  chip = el("#pricing-chip");
  const modal = el("#pricing-modal");

  for (const node of modal.querySelectorAll("[data-close]")) {
    node.addEventListener("click", closeModal);
  }
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !modal.hidden) closeModal();
  });

  el("#pricing-search").addEventListener("input", render);
  el("#pricing-add-btn").addEventListener("click", onAdd);
  el("#pricing-save").addEventListener("click", onSave);
  el("#pricing-reset").addEventListener("click", onReset);
  el("#pricing-copy-path").addEventListener("click", onCopyPath);
  el("#pricing-update-btn").addEventListener("click", onUpdate);
  el("#pricing-body").addEventListener("input", onEdit);
  el("#pricing-body").addEventListener("click", onRemove);

  try {
    await load();
    setChip(state.entries.length);
    syncUpdateArea();
    chip.addEventListener("click", openModal);
    checkUpdate(); // 启动时静默检测在线更新，不阻塞初始化
  } catch {
    chip.textContent = "价格表：不可用";
  }
}
