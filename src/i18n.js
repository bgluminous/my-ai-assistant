const bundled = new Map();
let languages = [];
let selection = { preference: "system", locale: "zh-CN" };
let messages = {};
let incoming = 0;
const previewKey = "uiLanguage";
const markup = new WeakMap();
const computed = new WeakMap();

export function locale() { return selection.locale; }
export function languagePreference() { return selection.preference; }
export function errorCode(error) {
  if (typeof error?.code === "string") return error.code;
  return typeof error?.message === "string" ? error.message : String(error);
}

export function registerBundledPacks(packs) {
  for (const pack of packs) bundled.set(pack.locale, pack);
  languages = packs.map(({ locale, name }) => ({ locale, name }));
  messages = bundled.get(locale())?.messages || bundled.get("en")?.messages || {};
}

async function loadBundledPacks() {
  if (bundled.size) return;
  async function read(file) {
    const response = await fetch(new URL(`./locales/${file}.json`, import.meta.url));
    if (!response.ok) throw new Error(`language_pack_unavailable: ${file}`);
    return response.json();
  }
  const names = await read("index");
  registerBundledPacks(await Promise.all(names.map(read)));
}

export function resolveLocale(preference, preferred, choices = languages) {
  const exact = tag => choices.find(c => c.locale.toLowerCase() === tag.toLowerCase())?.locale;
  if (exact(preference)) return exact(preference);
  for (const raw of preferred) {
    let tag = String(raw).replaceAll("_", "-");
    const primary = tag.split("-")[0].toLowerCase();
    while (tag) {
      if (exact(tag)) return exact(tag);
      const end = tag.lastIndexOf("-");
      tag = end < 0 ? "" : tag.slice(0, end);
    }
    if (primary === "zh" && exact("zh-CN")) return "zh-CN";
    const regional = choices.find(c => c.locale.toLowerCase().split("-")[0] === primary);
    if (regional) return regional.locale;
  }
  return "en";
}

export function hasMessage(key) {
  return Object.hasOwn(messages, key) || Object.hasOwn(bundled.get("en")?.messages || {}, key);
}

export function translate(key, ...values) {
  const fallback = bundled.get("en")?.messages;
  const message = Object.hasOwn(messages, key) ? messages[key]
    : fallback && Object.hasOwn(fallback, key) ? fallback[key] : key;
  return values.length ? message.replace(/\{(\d+)\}/g, (_, i) => String(values[i])) : message;
}

// Old account caches contain app-authored Chinese labels. Only callers that own the field
// opt in to this lookup. Account names, provider errors, audit details and release notes stay opaque.
export function legacyText(value, allowedKeys) {
  const original = bundled.get("zh-CN")?.messages || {};
  const key = allowedKeys.find(key => original[key] === value);
  return key ? translate(key) : value;
}

export function localizeMarkup(root) {
  for (const attr of ["text", "title", "aria-label", "placeholder", "alt"]) {
    const marker = attr === "text" ? "data-i18n" : "data-i18n-" + attr;
    for (const element of root.querySelectorAll("[" + marker + "]")) {
      const key = element.getAttribute(marker);
      let owners = markup.get(element);
      if (!owners) { owners = new Map(); markup.set(element, owners); }
      if (attr === "text") {
        let text = owners.get(attr);
        if (!text) {
          text = element.ownerDocument.createTextNode("");
          element.replaceChildren(text);
          owners.set(attr, text);
        }
        if (text.parentNode === element) text.nodeValue = translate(key);
      } else if (!owners.has(attr) || owners.get(attr) === element.getAttribute(attr)) {
        const value = translate(key);
        element.setAttribute(attr, value);
        owners.set(attr, value);
      }
    }
  }
}

// A callback binds a currently displayed status or form label to its original data.
export function bindText(node, content, property = "textContent") {
  let fields = computed.get(node);
  if (typeof content === "function") {
    if (!fields) { fields = new Map(); computed.set(node, fields); }
    node.setAttribute("data-i18n-live", "");
    const last = content();
    fields.set(property, { content, last });
    node[property] = last;
  } else {
    fields?.delete(property);
    node[property] = content;
  }
}

function refreshComputed(root) {
  for (const node of root.querySelectorAll("[data-i18n-live]")) {
    for (const [property, binding] of computed.get(node) || []) {
      if (node[property] !== binding.last) continue;
      binding.last = binding.content();
      node[property] = binding.last;
    }
  }
}
function applyLanguage(state, initial = false) {
  if (!state?.locale || !state.messages || !Array.isArray(state.languages)) return;
  const changed = locale() !== state.locale || languagePreference() !== state.preference;
  languages = state.languages;
  messages = state.messages;
  selection = { locale: state.locale, preference: state.preference };
  document.documentElement.lang = locale();
  if (initial || changed) {
    localizeMarkup(document);
    refreshComputed(document);
  }
  const picker = document.getElementById("ui-language");
  if (picker) {
    const options = [{ locale: "system", name: translate("settings.followSystem") }, ...languages];
    picker.replaceChildren(...options.map(choice => {
      const item = document.createElement("option");
      item.value = choice.locale;
      item.textContent = choice.name;
      if (choice.locale !== "system") item.lang = choice.locale;
      return item;
    }));
    picker.value = selection.preference;
  }
  if (changed) window.dispatchEvent(new CustomEvent("uilanguagechange", { detail: selection }));
}

function previewState(preference) {
  if (!bundled.has(preference)) preference = "system";
  const preferred = navigator.languages || [navigator.language];
  const selected = resolveLocale(preference, preferred);
  return { preference, locale: selected, languages, messages: bundled.get(selected).messages };
}

function readPreviewPreference() {
  try { return localStorage.getItem(previewKey) || "system"; } catch { return "system"; }
}

export async function initializeLanguage() {
  const bridge = window.__TAURI__;
  // Native builds also bundle the packs in the executable. They can start if a WebView
  // cannot fetch local assets. The standalone preview reads the same JSON files directly.
  try { await loadBundledPacks(); } catch (error) { if (!bridge?.core) throw error; }
  let initialized = false;
  function accept(state) {
    applyLanguage(state, !initialized);
    initialized = true;
  }
  if (bridge?.event) await bridge.event.listen("ui-language-changed", ({ payload }) => {
    incoming++;
    accept(payload);
  }).catch(() => {});
  const read = async () => {
    const before = incoming;
    const next = bridge?.core ? await bridge.core.invoke("ui_language_get") : previewState(readPreviewPreference());
    if (before === incoming) accept(next);
  };
  try { await read(); } catch (error) {
    if (!bundled.size) throw error;
    accept(previewState("system"));
  }
  let systemLanguages = String(navigator.languages);
  window.addEventListener("languagechange", () => { systemLanguages = String(navigator.languages); void read().catch(() => {}); });
  window.addEventListener("focus", () => {
    if (selection.preference === "system" && systemLanguages !== String(navigator.languages)) {
      systemLanguages = String(navigator.languages);
      void read().catch(() => {});
    }
  });
  if (!bridge?.core) window.addEventListener("storage", (event) => {
    if (event.key === previewKey || event.key === null) void read();
  });
}

export async function setLanguage(preference) {
  if (preference !== "system" && !languages.some(choice => choice.locale === preference)) throw new Error("invalid_ui_language");
  const core = window.__TAURI__?.core;
  if (core) {
    const before = incoming;
    const next = await core.invoke("ui_language_set", { preference });
    if (before === incoming) applyLanguage(next);
  } else {
    localStorage.setItem(previewKey, preference);
    applyLanguage(previewState(preference));
  }
}
