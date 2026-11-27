import { translate } from "./i18n.js";
import { createModal } from "./modal.js";
import { el, invoke, copyText, resetError, fillStatus, fmtDateMs, escapeHtml, toast } from "./shared.js";
import { notifyPricingChanged } from "./usage_data.js";
import { pricingError } from "./update_status.js";

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
  error: "",
  checking: false,
};
let chip = null;
let chipCount = null;
let modalController;
function getModal() {
  return modalController ||= createModal(el("#pricing-modal"));
}

const SOURCE_LABEL = { get default() { return translate("pricing.default"); }, get remote() { return translate("pricing.online"); }, get custom() { return translate("usage.custom"); } };

// 与后端 model_match 的 Flash 别名规则一致；只允许在基础项修改各版本的共享价格。
const DEEPSEEK_FLASH_NAME = /^(?:cursor-)?deepseek-(?:v[0-9]+(?:[._-][0-9]+)*-)?flash(?:-[0-9]+)*(?:-(?:fast|xhigh|high|medium|low|minimal|max|thinking|reasoning|latest))*$/;

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
  chipCount = count;
  if (chip) chip.textContent = translate("pricing.pricingModels", count);
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
      <td><span class="tag ${e.source === "default" ? "" : e.source}">${escapeHtml(SOURCE_LABEL[e.source] || translate("pricing.default"))}</span></td>
      <td class="col-actions"><button class="table-button" type="button" data-remove aria-label="${escapeHtml(translate("pricing.removeModel", e.key))}">✕</button></td>`;
    body.append(tr);
  });
  el("#pricing-empty").hidden = shown > 0;
  el("#pricing-count").textContent = translate("pricing.models", state.entries.length);
}

function onEdit(ev) {
  if (getModal().busy) return;
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
    badge.textContent = translate("usage.custom");
    badge.className = "tag custom";
  }
}

function onRemove(ev) {
  if (getModal().busy) return;
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
  if (getModal().busy) return;
  const key = el("#add-key").value.trim().toLowerCase();
  if (!key) {
    setStatus("bad", () => translate("pricing.enterAModelNameFirst"));
    return;
  }
  if (key !== "deepseek-flash" && DEEPSEEK_FLASH_NAME.test(key)) {
    setStatus("bad", () => translate("pricing.deepseekFlashVersionsSharePricingEditThe"));
    return;
  }
  if (state.entries.some((e) => e.key.toLowerCase() === key)) {
    setStatus("bad", () => translate("pricing.modelAlreadyExistsEditItsEntryIn", key));
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
  const operation = getModal().beginSave([el("#pricing-save"), el("#pricing-reset"), el("#pricing-update-btn")]);
  if (!operation) return;
  setStatus("", () => translate("accounts.saving"));
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
    if (!operation.current()) return;
    await load();
    if (!operation.current()) return;
    render();
    setChip(status.models);
    // 保存成功即关闭弹窗，结果用右上角 toast 提示；失败时保留弹窗与状态条
    closeModal();
    toast("ok", () => translate("pricing.pricesSavedForModelsUsageCostsHave", status.models), { key: "pricing" });
  } catch (err) {
    if (!operation.current()) return;
    setStatus("bad", () => translate("accounts.saveFailed", resetError(err)));
  } finally {
    operation.finish();
  }
}

async function onReset() {
  const operation = getModal().beginSave([el("#pricing-save"), el("#pricing-reset"), el("#pricing-update-btn")]);
  if (!operation) return;
  setStatus("", () => translate("pricing.resetting"));
  try {
    const view = await invoke("pricing_reset");
    notifyPricingChanged();
    if (!operation.current()) return;
    applyView(view);
    render();
    syncUpdateArea();
    setChip(view.count);
    setStatus("ok", () => translate("pricing.builtInPricesRestoredAndTheOnline"));
    checkUpdate(); // 重置后重新检测：在线表若与默认表不同会再次提示
  } catch (err) {
    if (!operation.current()) return;
    setStatus("bad", () => translate("pricing.resetFailed", resetError(err)));
  } finally {
    operation.finish();
  }
}

/** 把 remote / 更新检测状态同步到弹窗的在线更新区与 chip 角标。 */
function syncUpdateArea(resetInput = false) {
  const input = el("#pricing-update-url");
  if (resetInput) input.value = state.remote.url;
  input.placeholder = state.remote.defaultUrl ? translate("pricing.default2", state.remote.defaultUrl) : "";
  el("#pricing-update-meta").textContent = state.remote.fetchedAtMs
    ? translate("pricing.lastUpdatedOnlineModels", fmtDateMs(state.remote.fetchedAtMs), state.remote.models)
    : translate("pricing.noOnlineUpdateYetUsingBuiltIn");
  const hint = el("#pricing-update-hint");
  hint.hidden = !state.update.available;
  if (state.update.available) {
    hint.textContent = translate("pricing.onlinePricingUpdateAvailableModelsSelectOnline", state.update.models);
  }
  if (chip) {
    chip.classList.toggle("has-update", state.update.available);
    chip.title = state.error || (state.update.available
      ? translate("pricing.pricingUpdateAvailableClickToView")
      : translate("pricing.viewOrEditPrices"));
    chip.classList.toggle("has-error", !!state.error);
  }
  el("#pricing-update-error").textContent = state.error;
  el("#pricing-update-error").hidden = !state.error;
  el("#pricing-update-retry").disabled = state.checking;
  el("#pricing-update-retry").textContent = state.checking ? translate("pricing.checking") : state.error ? translate("pricing.checkAgain2") : translate("updater.checkForUpdates");
}

async function onUpdate() {
  const operation = getModal().beginSave([el("#pricing-save"), el("#pricing-reset"), el("#pricing-update-btn")]);
  if (!operation) return;
  setStatus("", () => translate("pricing.fetchingOnlinePrices"));
  try {
    const url = el("#pricing-update-url").value.trim();
    const view = await invoke("pricing_update_apply", { url });
    notifyPricingChanged();
    if (!operation.current()) return;
    applyView(view);
    state.update = { available: false, models: 0 };
    state.error = "";
    render();
    syncUpdateArea(true);
    setChip(state.entries.length);
    setStatus("ok", () => translate("pricing.updatedPricingForModelsUsageCostsHave", state.remote.models));
  } catch (err) {
    if (!operation.current()) return;
    state.error = pricingError(err);
    setStatus("bad", state.error);
    syncUpdateArea();
  } finally {
    operation.finish();
  }
}

/** 静默检测一次是否有在线更新（用已保存地址）；网络失败不打扰用户。 */
async function checkUpdate(manual = false) {
  if (state.checking) return;
  state.checking = true;
  syncUpdateArea();
  try {
    const res = await invoke("pricing_update_check", { url: manual ? el("#pricing-update-url").value.trim() : null });
    state.update = { available: !!res.hasUpdate, models: Number(res.models) || 0 };
    state.error = "";
    if (manual) setStatus("ok", () => res.hasUpdate ? translate("pricing.pricingUpdateAvailableSelectOnlineUpdatesTo") : translate("pricing.pricingMatchesTheCurrentBaseTable"));
  } catch (err) {
    // 失败不清除已有缓存、自定义价格或上次已知的更新。
    state.error = pricingError(err);
    if (manual) setStatus("bad", state.error);
  } finally {
    state.checking = false;
  }
  syncUpdateArea();
}

async function onCopyPath() {
  if (!state.path) return;
  const btn = el("#pricing-copy-path");
  try {
    await copyText(state.path);
    btn.textContent = translate("pricing.copied");
    setTimeout(() => (btn.textContent = translate("pricing.copyPath")), 1200);
  } catch {
    setStatus("bad", () => translate("pricing.couldNotWriteToTheClipboard"));
  }
}

function openModal() {
  if (getModal().busy) return;
  getModal().open();
  clearStatus();
  syncUpdateArea(true);
  render();
  el("#pricing-search").focus();
}
function closeModal() {
  getModal().close();
}

export async function initPricing() {
  chip = el("#pricing-chip");
  window.addEventListener("uilanguagechange", () => {
    if (chipCount != null) setChip(chipCount);
    else chip.textContent = translate("pricing.pricingUnavailable");
    syncUpdateArea();
    for (const row of el("#pricing-body").querySelectorAll("tr")) {
      const entry = state.entries[Number(row.dataset.index)];
      if (!entry) continue;
      row.querySelector(".tag").textContent = SOURCE_LABEL[entry.source];
      row.querySelector("[data-remove]").setAttribute("aria-label", translate("pricing.removeModel", entry.key));
    }
  });
  el("#pricing-search").addEventListener("input", render);
  el("#pricing-add-btn").addEventListener("click", onAdd);
  el("#pricing-save").addEventListener("click", onSave);
  el("#pricing-reset").addEventListener("click", onReset);
  el("#pricing-copy-path").addEventListener("click", onCopyPath);
  el("#pricing-update-btn").addEventListener("click", onUpdate);
  el("#pricing-update-retry").addEventListener("click", () => { void checkUpdate(true); });
  el("#pricing-update-proxy").addEventListener("click", () => { if (getModal().busy) return; closeModal(); el("#proxy-btn").click(); });
  el("#pricing-update-source").addEventListener("click", () => {
    void invoke("app_update_open_pricing").catch(() => setStatus("bad", () => translate("pricing.openResourcesMyAiAssistantPricingJson")));
  });
  el("#pricing-body").addEventListener("input", onEdit);
  el("#pricing-body").addEventListener("click", onRemove);

  try {
    await load();
    setChip(state.entries.length);
    syncUpdateArea(true);
    chip.addEventListener("click", openModal);
    checkUpdate(); // 启动时静默检测在线更新，不阻塞初始化
  } catch {
    chip.textContent = translate("pricing.pricingUnavailable");
  }
}
