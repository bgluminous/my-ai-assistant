// Anonymous availability gate: initialize the public source before shipping its URL.
const url = "https://raw.githubusercontent.com/bgluminous/open-source-registry/main/resources/my-ai-assistant/pricing.json";
try {
  const response = await fetch(url, { signal: AbortSignal.timeout(30_000), headers: { "Cache-Control": "no-cache" } });
  if (!response.ok) throw Error("E_PRICING_SOURCE_UNAVAILABLE");
  const text = await response.text();
  if (text.length > 2 * 1024 * 1024) throw Error("E_PRICING_SOURCE_SIZE");
  const data = JSON.parse(text);
  if (!data.models || typeof data.models !== "object" || Array.isArray(data.models) || !Object.keys(data.models).length) throw Error("E_PRICING_SOURCE_INVALID");
  for (const model of Object.values(data.models)) {
    if (!model || !["input", "output"].every(key => Number.isFinite(model[key]) && model[key] >= 0)) throw Error("E_PRICING_SOURCE_INVALID");
  }
  console.log("Public pricing source is anonymously readable.");
} catch (error) {
  console.error(error.message.startsWith("E_") ? error.message : "E_PRICING_SOURCE_FETCH");
  process.exitCode = 1;
}
