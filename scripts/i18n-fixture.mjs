import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../src/i18n.js", import.meta.url), "utf8");
export const packs = await Promise.all(["zh-CN", "en"].map(async locale =>
  JSON.parse(await readFile(new URL(`../src/locales/${locale}.json`, import.meta.url), "utf8"))));

// Each simulated WebView owns an independent copy of the application's language state.
export async function i18nFixture(context, bind = true) {
  const module = new vm.SourceTextModule(source, { context });
  await module.link(() => { throw new Error("Unexpected language module import"); });
  await module.evaluate();
  module.namespace.registerBundledPacks(packs);
  if (bind && context.document?.querySelectorAll) module.namespace.localizeMarkup(context.document);
  return module;
}
