// Project build helper (Node is already required by Tauri). Never publishes anything.
import { readFile, writeFile, stat } from "node:fs/promises";
import { createHash, createPublicKey, verify } from "node:crypto";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

const releases = "https://github.com/bgluminous/my-ai-assistant/releases";
export function artifacts(platform, version) {
  if (!/^(0|[1-9]\d?)\.(0|[1-9]\d?)\.(0|[1-9]\d?)$/.test(version)) throw Error("E_VERSION");
  if (platform === "windows-x64") return {
    "windows-x86_64-nsis": `my-ai-assistant_${version}_x64-setup.exe`,
    "windows-x86_64-msi": `my-ai-assistant_${version}_x64_en-US.msi`,
  };
  if (platform === "macos-arm64") return { "darwin-aarch64": "my-ai-assistant.app.tar.gz" };
  throw Error("E_PLATFORM");
}
function base64(value) {
  const clean = value.trim();
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(clean)) throw Error("E_SIGNATURE_ENCODING");
  const bytes = Buffer.from(clean, "base64");
  if (bytes.toString("base64") !== clean) throw Error("E_SIGNATURE_ENCODING");
  return bytes;
}
// Tauri wraps a minisign file in base64. Verify both the payload and trusted comment
// with Node's Ed25519 implementation; the shipped updater repeats this verification.
export function verifySignature(payload, encodedSignature, encodedPublicKey) {
  const keyLines = base64(encodedPublicKey).toString("utf8").trim().split(/\r?\n/);
  const lines = base64(encodedSignature).toString("utf8").trim().split(/\r?\n/);
  if (keyLines.length !== 2 || lines.length !== 4 || !lines[2].startsWith("trusted comment: ")) throw Error("E_SIGNATURE_FORMAT");
  const key = base64(keyLines[1]);
  const sig = base64(lines[1]);
  const global = base64(lines[3]);
  if (key.length !== 42 || sig.length !== 74 || global.length !== 64
      || key.subarray(0, 2).toString() !== "Ed" || !key.subarray(2, 10).equals(sig.subarray(2, 10))) throw Error("E_SIGNATURE_KEY");
  const algo = sig.subarray(0, 2).toString();
  if (algo !== "ED" && algo !== "Ed") throw Error("E_SIGNATURE_ALGORITHM");
  const publicKey = createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), key.subarray(10)]), format: "der", type: "spki" });
  const data = algo === "ED" ? createHash("blake2b512").update(payload).digest() : payload;
  if (!verify(null, data, publicKey, sig.subarray(10))
      || !verify(null, Buffer.concat([sig.subarray(10), Buffer.from(lines[2].slice(17))]), publicKey, global)) throw Error("E_SIGNATURE_INVALID");
}
export function validateManifest(manifest, platform, version) {
  const expected = artifacts(platform, version);
  if (manifest.version !== version || typeof manifest.notes !== "string"
      || Object.keys(manifest.platforms || {}).sort().join() !== Object.keys(expected).sort().join()) throw Error("E_MANIFEST_IDENTITY");
  for (const [target, name] of Object.entries(expected)) {
    const item = manifest.platforms[target];
    if (item.url !== `${releases}/download/v${version}/${name}` || typeof item.signature !== "string" || !item.signature.trim()) throw Error("E_MANIFEST_ASSET");
    base64(item.signature);
  }
}
export async function buildManifest(platform, dir, registration, configPath = "src-tauri/tauri.conf.json") {
  const config = JSON.parse(await readFile(configPath, "utf8"));
  const version = config.version;
  const expected = artifacts(platform, version);
  const input = registration ? JSON.parse(await readFile(registration, "utf8")) : { version, changelog: "测试构建，不登记正式版本。" };
  const outputName = `updater-${platform}.json`;
  if (input.version !== version || (registration && input.extra?.updater?.manifestUrl !== `${releases}/download/v${version}/${outputName}`)) throw Error("E_REGISTRATION_MISMATCH");
  const manifest = { version, notes: input.changelog || "", platforms: {} };
  for (const [target, name] of Object.entries(expected)) {
    const asset = join(dir, name);
    if (!(await stat(asset)).isFile()) throw Error("E_ASSET_MISSING");
    const payload = await readFile(asset);
    if (!payload.length) throw Error("E_ASSET_EMPTY");
    const signature = (await readFile(`${asset}.sig`, "utf8")).trim();
    verifySignature(payload, signature, config.plugins.updater.pubkey);
    manifest.platforms[target] = { signature, url: `${releases}/download/v${version}/${name}` };
  }
  validateManifest(manifest, platform, version);
  // Reproducible, with no wall clock timestamp, and never overwrite differing bytes.
  const content = JSON.stringify(manifest, null, 2) + "\n";
  const path = join(dir, outputName);
  try { await writeFile(path, content, { flag: "wx" }); }
  catch (error) { if (error.code !== "EEXIST" || await readFile(path, "utf8") !== content) throw Error("E_MANIFEST_CONFLICT"); }
  return manifest;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length < 4 || process.argv.length > 5) throw Error("E_ARGUMENT");
    await buildManifest(process.argv[2], process.argv[3], process.argv[4]);
    console.log("Updater artifacts and signatures verified; manifest saved.");
  } catch (error) { console.error(error.message.startsWith("E_") ? error.message : "E_UPDATER_ARTIFACT"); process.exitCode = 1; }
}
