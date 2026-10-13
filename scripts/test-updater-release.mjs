// Uses real Tauri CLI signatures with disposable test keys; no production keys or network.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { execFileSync } from "node:child_process";
import { artifacts, buildManifest, validateManifest, verifySignature } from "../ci/updater-manifest.mjs";

test("real Tauri signatures, both installer types, identity and immutable manifests", async () => {
  const root = await mkdtemp(join(tmpdir(), "maa-updater-test-"));
  const cli = resolve("node_modules/@tauri-apps/cli/tauri.js");
  const run = (...args) => execFileSync(process.execPath, [cli, "signer", ...args], { stdio: "pipe", windowsHide: true,
    env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "fixture-password" } });
  try {
    const key = join(root, "test.key");
    run("generate", "--ci", "-p", "fixture-password", "-w", key);
    const pubkey = (await readFile(`${key}.pub`, "utf8")).trim();
    const config = join(root, "config.json");
    await writeFile(config, JSON.stringify({ version: "1.3.0", plugins: { updater: { pubkey } } }));
    for (const platform of ["windows-x64", "macos-arm64"]) {
      const dir = join(root, platform); await mkdir(dir);
      for (const name of Object.values(artifacts(platform, "1.3.0"))) {
        const path = join(dir, name);
        await writeFile(path, `signed fixture ${platform} ${name}`);
        run("sign", "-f", key, path);
      }
      const input = join(dir, "registration.json");
      await writeFile(input, JSON.stringify({ version: "1.3.0", changelog: "更新说明\n", extra: {
        updater: { manifestUrl: `https://github.com/bgluminous/my-ai-assistant/releases/download/v1.3.0/updater-${platform}.json` },
      } }));
      const manifest = await buildManifest(platform, dir, input, config);
      assert.equal(manifest.version, "1.3.0");
      assert.deepEqual(await buildManifest(platform, dir, input, config), manifest);
      const manifestPath = join(dir, `updater-${platform}.json`);
      await writeFile(manifestPath, "{}");
      await assert.rejects(buildManifest(platform, dir, input, config), /E_MANIFEST_CONFLICT/);
      await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
      assert.throws(() => validateManifest({ ...manifest, version: "1.4.0" }, platform, "1.3.0"), /E_MANIFEST_IDENTITY/);
      const [target, name] = Object.entries(artifacts(platform, "1.3.0"))[0];
      const signature = manifest.platforms[target].signature;
      assert.throws(() => verifySignature(Buffer.from("tampered"), signature, pubkey), /E_SIGNATURE_INVALID/);
      assert.throws(() => validateManifest({ ...manifest, platforms: {} }, platform, "1.3.0"), /E_MANIFEST_IDENTITY/);
      await writeFile(join(dir, name), "tampered");
      await assert.rejects(buildManifest(platform, dir, input, config), /E_SIGNATURE_INVALID/);
      await rm(join(dir, `${name}.sig`));
      await assert.rejects(buildManifest(platform, dir, input, config), /ENOENT/);
    }
  } finally {
    assert.equal(dirname(root), resolve(tmpdir()));
    assert(root.includes("maa-updater-test-"));
    await rm(root, { recursive: true, force: true });
  }
});
