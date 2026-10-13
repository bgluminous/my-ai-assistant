// Registration gate: every update resource must be publicly readable and signed.
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { artifacts, validateManifest, verifySignature } from "./updater-manifest.mjs";

async function download(url, expected, limit, timeout = 30_000) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeout) });
  if (!response.ok) throw Error("E_PUBLIC_ASSET_UNAVAILABLE");
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > limit) throw Error("E_PUBLIC_ASSET_SIZE");
    chunks.push(chunk);
  }
  const data = Buffer.concat(chunks);
  if (expected && (size !== expected.size || `sha256:${createHash("sha256").update(data).digest("hex")}` !== expected.digest)) throw Error("E_PUBLIC_ASSET_DIGEST");
  return data;
}
try {
  const [platform, inputPath] = process.argv.slice(2);
  const input = JSON.parse(await readFile(inputPath, "utf8"));
  const config = JSON.parse(await readFile("src-tauri/tauri.conf.json", "utf8"));
  const version = config.version;
  const expected = artifacts(platform, version);
  const base = `https://github.com/bgluminous/my-ai-assistant/releases/download/v${version}/`;
  const manifestName = `updater-${platform}.json`;
  if (input.version !== version || input.extra?.updater?.manifestUrl !== base + manifestName) throw Error("E_PREPARED_MANIFEST");
  const release = JSON.parse((await download(`https://api.github.com/repos/bgluminous/my-ai-assistant/releases/tags/v${version}`, null, 1024 * 1024)).toString());
  if (release.draft || release.prerelease || release.tag_name !== `v${version}`) throw Error("E_RELEASE_NOT_PUBLIC");
  const assets = new Map((release.assets || []).map(a => [a.name, a]));
  const get = async (name, limit, timeout) => {
    const meta = assets.get(name);
    if (!meta || meta.state !== "uploaded" || !(meta.size > 0) || !/^sha256:[a-f0-9]{64}$/.test(meta.digest || "") || meta.browser_download_url !== base + name) throw Error("E_ASSET_METADATA");
    return download(meta.browser_download_url, meta, limit, timeout);
  };
  const manifest = JSON.parse((await get(manifestName, 256 * 1024)).toString());
  validateManifest(manifest, platform, version);
  if (manifest.notes !== (input.changelog || "")) throw Error("E_MANIFEST_NOTES");
  for (const [target, name] of Object.entries(expected)) {
    const signature = (await get(`${name}.sig`, 16 * 1024)).toString().trim();
    if (signature !== manifest.platforms[target].signature) throw Error("E_SIGNATURE_CONTENT");
    verifySignature(await get(name, 512 * 1024 * 1024, 900_000), signature, config.plugins.updater.pubkey);
  }
  console.log("Public updater manifest, downloads, digests and signatures verified.");
} catch (error) {
  console.error(error.message.startsWith("E_") ? error.message : "E_PUBLIC_UPDATER_VERIFY");
  process.exitCode = 1;
}
