// node --experimental-vm-modules --test scripts/test-i18n.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import vm from "node:vm";
import { parseHTML } from "linkedom";
import { i18nFixture, packs } from "./i18n-fixture.mjs";

const src = new URL("../src/", import.meta.url);
const main = await readFile(new URL("index.html", src), "utf8");
const tray = await readFile(new URL("tray.html", src), "utf8");
const [zh, en] = packs;
const dictionary = Object.keys(en.messages).sort();
const placeholders = text => [...new Set(text.match(/\{\d+\}/g) || [])].sort();

test("both JSON packs define every UI key and preserve interpolation arguments", async () => {
  assert.deepEqual(Object.keys(zh.messages).sort(), dictionary);
  for (const key of dictionary) {
    assert.ok(zh.messages[key] && en.messages[key], key);
    assert.deepEqual(placeholders(zh.messages[key]), placeholders(en.messages[key]), key);
  }
  const scripts = await Promise.all((await readdir(src)).filter(name => name.endsWith(".js"))
    .map(name => readFile(new URL(name, src), "utf8")));
  for (const source of [main, tray, ...scripts]) {
    for (const [, key] of source.matchAll(/(?:data-i18n(?:-[\w-]+)?="|translate\(")([^"]+)"/g)) {
      assert.ok(Object.hasOwn(en.messages, key), key);
    }
  }
});

function backend() {
  const handlers = new Set();
  const custom = { locale: "fr", name: "Français", messages: { ...en.messages, "common.settings": "Paramètres" } };
  let preference = "system";
  const view = () => {
    const pack = [zh, en, custom].find(pack => pack.locale === preference) || zh;
    return { preference, locale: pack.locale, messages: pack.messages, languages: [zh, en, custom].map(({locale, name}) => ({locale, name})) };
  };
  const core = { async invoke(cmd, args) {
    if (cmd === "ui_language_get") return view();
    assert.equal(cmd, "ui_language_set");
    if (this.rejectSave) throw new Error("write_failed");
    preference = args.preference;
    for (const fn of handlers) fn({ payload: view() });
    return view();
  }};
  return { core, event: { async listen(name, fn) { assert.equal(name, "ui-language-changed"); handlers.add(fn); } } };
}

async function webview(html, bridge) {
  const { document, window: dom } = parseHTML(html);
  Object.defineProperty(dom.HTMLSelectElement.prototype, "value", {
    configurable: true,
    get() { return this.querySelector("option[selected]")?.value ?? this.firstElementChild?.value ?? ""; },
    set(value) { for (const option of this.querySelectorAll("option")) option.toggleAttribute("selected", option.value === value); },
  });
  const handlers = new Map();
  const window = { __TAURI__: bridge, addEventListener(type, fn) { handlers.set(type, fn); },
    dispatchEvent(event) { handlers.get(event.type)?.(event); } };
  const context = vm.createContext({ document, window, CustomEvent: dom.CustomEvent, navigator: { languages: ["zh-CN"] } });
  const module = await i18nFixture(context, false);
  const api = module.namespace;
  await api.initializeLanguage();
  return { api, document, context, module };
}

test("switching updates both WebViews, preserves drafts and reads the saved or restored choice", async () => {
  const bridge = backend();
  const a = await webview(main, bridge);
  const b = await webview(tray, bridge);
  const draft = a.document.querySelector("#gpt-sync-url");
  draft.value = "设置";
  await a.api.setLanguage("en");
  assert.equal(a.document.documentElement.lang, "en");
  assert.equal(a.document.querySelector("#ui-language-folder").textContent, "Open language folder");
  assert.equal(b.document.querySelector(".tray-tab").textContent, "Overview");
  assert.equal(a.document.querySelector("#gpt-sync-url"), draft);
  assert.equal(draft.value, "设置");
  assert.equal((await webview(main, bridge)).api.languagePreference(), "en");
  await b.api.setLanguage("fr");
  assert.equal(a.document.querySelector("#settings-btn").getAttribute("aria-label"), "Paramètres");
  assert.ok([...a.document.querySelectorAll("#ui-language option")].some(o => o.textContent === "Français"));
  bridge.core.rejectSave = true;
  await assert.rejects(a.api.setLanguage("zh-CN"), /write_failed/);
  assert.equal(a.api.locale(), "fr");
  assert.equal(b.api.locale(), "fr");
});

test("matching follows system preference order and supports regional and custom language tags", async () => {
  const { api } = await webview(tray, backend());
  for (const [list, expected] of [[["de", "zh-Hant-TW", "en"], "zh-CN"], [["en-GB", "zh"], "en"],
    [["de-DE", "fr-CA"], "fr"], [["ja"], "en"], [[], "en"]]) {
    assert.equal(api.resolveLocale("system", list), expected);
  }
});

test("date-only quota resets compare calendar dates across a year boundary in both languages", async () => {
  const a = await webview(main, backend());
  class Clock extends Date { static now() { return new Date(2026, 11, 31, 23, 30).getTime(); } }
  a.context.Date = Clock;
  const dates = new vm.SourceTextModule(await readFile(new URL("account_format.js", src), "utf8"), { context: a.context });
  await dates.link(() => a.module);
  await dates.evaluate();
  const { quotaResetInfo } = dates.namespace;
  for (const lang of ["zh-CN", "en"]) {
    await a.api.setLanguage(lang);
    const recent = quotaResetInfo("2026-12-30");
    const today = quotaResetInfo("2026-12-31");
    const tomorrow = quotaResetInfo("2027-01-01");
    assert.equal(recent.text, a.api.translate("quota.reset"));
    assert.equal(today.text, a.api.translate("quota.resetsAt", lang === "zh-CN" ? "12-31" : "12/31"));
    assert.equal(tomorrow.text, a.api.translate("quota.resetsAt", lang === "zh-CN" ? "01-01" : "01/01"));
    assert.equal(tomorrow.title, a.api.translate("quota.resetTooltip", lang === "zh-CN" ? "2027/01/01" : "01/01/2027"));
    assert.deepEqual(quotaResetInfo("2026/12/30"), recent);
    assert.deepEqual(quotaResetInfo("2026/12/31"), today);
    assert.deepEqual(quotaResetInfo("2027/01/01"), tomorrow);
    assert.deepEqual(quotaResetInfo("2027/1/1"), tomorrow);
  }
});

test("CSV localizes headings and readable dates while preserving numeric and provider data", async () => {
  const a = await webview(main, backend());
  const raw = new vm.SourceTextModule(await readFile(new URL("raw_events.js", src), "utf8"), { context: a.context });
  const shared = new vm.SourceTextModule(await readFile(new URL("shared.js", src), "utf8"), { context: a.context });
  await shared.link(() => a.module);
  const stubs = new vm.SyntheticModule(["createModal", "fetchRawEvents", "clearCursorLocalData", "exportUsageEventsCsv", "confirmDialog"],
    function () {}, { context: a.context });
  await raw.link(path => path === "./i18n.js" ? a.module : path === "./shared.js" ? shared : stubs);
  await raw.evaluate();
  const record = { timestampMs: 1795800000000, requestId: "req-1", model: "模型 A", product: "demo",
    inputTokens: 37, outputTokens: 12, cacheReadTokens: 4, cacheWriteTokens: 0, totalTokens: 53,
    actualUsd: 0.000002, reportedCostUsd: 0.000002, equivalentUsd: 0.01, reasoningTokens: 9 };
  for (const kind of ["cursor", "opencode"]) {
    const files = [];
    for (const lang of ["zh-CN", "en"]) {
      await a.api.setLanguage(lang);
      files.push(raw.namespace.usageEventsCsv([record], kind).split("\r\n"));
    }
    assert.match(files[0][0], /^时间,时间戳\(ms\)/);
    assert.match(files[1][0], /^Time,Timestamp \(ms\)/);
    assert.notEqual(files[0][1], files[1][1]);
    const numericSuffix = row => row.slice(row.indexOf(String(record.timestampMs)));
    assert.equal(numericSuffix(files[0][1]), numericSuffix(files[1][1]));
    assert.ok(files[1][1].includes("模型 A"));
  }
});
