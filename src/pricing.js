import { el, invoke, copyText, resetError, fillStatus } from "./shared.js";

// 价格表编辑弹窗：读取“默认 + 用户覆盖”的有效表，逐模型编辑 输入/输出/缓存读/缓存写，
// 覆盖写入用户目录 xilore/myaiassistant/settings.json 的 pricing 字段（后端只写与默认不同的条目）。

const state = { path: "", note: null, entries: [] };
let chip = null;

function escapeHtml(text) {
  return String(text).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

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
  state.entries = (view.models || []).map((m) => ({
    key: m.key,
    input: Number(m.input) || 0,
    output: Number(m.output) || 0,
    cacheRead: Number(m.cacheRead) || 0,
    cacheWrite: Number(m.cacheWrite) || 0,
    custom: !!m.custom,
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
      <td><span class="tag ${e.custom ? "custom" : ""}">${e.custom ? "自定义" : "默认"}</span></td>
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
  entry.custom = true;
  const badge = tr.querySelector(".tag");
  if (badge) {
    badge.textContent = "自定义";
    badge.classList.add("custom");
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
    custom: true,
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
    await load();
    render();
    setChip(status.models);
    setStatus("ok", `已保存，共 ${status.models} 个模型生效。`);
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
    applyView(view);
    render();
    setChip(view.count);
    setStatus("ok", "已恢复为内置默认价格表。");
  } catch (err) {
    setStatus("bad", `重置失败：${resetError(err)}`);
  } finally {
    btn.disabled = false;
  }
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
  el("#pricing-body").addEventListener("input", onEdit);
  el("#pricing-body").addEventListener("click", onRemove);

  try {
    await load();
    setChip(state.entries.length);
    chip.title = "点击查看 / 编辑价格表";
    chip.addEventListener("click", openModal);
  } catch {
    chip.textContent = "价格表：不可用";
  }
}
