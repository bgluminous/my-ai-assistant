// Real MAA Rust client -> temporary HTTPS proxy -> xil-information-service -> isolated MySQL tables.
// TEST_MYSQL_URL must point to an authorized development database. Never load application.json.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer as httpServer, request as httpRequest } from "node:http";
import { createServer as httpsServer } from "node:https";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { once } from "node:events";

const repo = resolve(process.argv[2] || "");
const maa = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
if (!process.argv[2] || !process.env.TEST_MYSQL_URL) throw new Error("Pass the server repository and TEST_MYSQL_URL explicitly");
const endpoint = process.env.TEST_MYSQL_URL;
assert.equal(new URL(endpoint).pathname, "/dev", "Use the authorized /dev database with random table prefixes");
const requireServer = createRequire(join(repo, "package.json"));
process.env.TSX_DISABLE_CACHE = "1";
const { register } = await import(pathToFileURL(requireServer.resolve("tsx/esm/api")).href);
register();
await import(pathToFileURL(join(repo, "test/setup.ts")).href);
const [{ isolatedMysql }, { runMigrations }, { MysqlCodexUsageMapper }, { MysqlCodexUsageAccountMapper }, { syncApp }] = await Promise.all([
  import(pathToFileURL(join(repo, "test/support/isolated-mysql.ts")).href),
  import(pathToFileURL(join(repo, "src/db/migrate.ts")).href),
  import(pathToFileURL(join(repo, "src/mapper/codex-usage-mapper.ts")).href),
  import(pathToFileURL(join(repo, "src/mapper/codex-usage-account-mapper.ts")).href),
  import(pathToFileURL(join(repo, "test/support/codex-usage-app.ts")).href),
]);

const safeEnv = { ...process.env, CARGO_PROFILE_DEV_DEBUG: "0", CARGO_PROFILE_TEST_DEBUG: "0" };
delete safeEnv.TEST_MYSQL_URL;
let child;
async function processRun(command, args, options = {}) {
  const { stream = false, ...spawnOptions } = options;
  child = spawn(command, args, { cwd: maa, env: safeEnv, windowsHide: true, stdio: ["ignore", "pipe", "pipe"], ...spawnOptions });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (data) => { stdout += data; if (stream) process.stdout.write(data); });
  child.stderr.on("data", (data) => { stderr += data; });
  const [code] = await once(child, "close");
  child = undefined;
  if (code !== 0) {
    // Subprocesses never receive the database credential. Do not print server/driver error objects.
    process.stderr.write(stderr);
    if (!stream) process.stdout.write(stdout);
    throw new Error("Test subprocess failed");
  }
  return stdout;
}

const build = await processRun("cargo", ["test", "--offline", "--locked", "--no-run", "--message-format=json",
  "--manifest-path", "scripts/codex-sync-tests/Cargo.toml", "--target-dir", "src-tauri/target", "-j", "1"]);
const executable = build.split("\n").filter(Boolean).map((line) => JSON.parse(line))
  .find((item) => item.reason === "compiler-artifact" && item.executable && item.target.name === "maa_codex_sync_tests")?.executable;
assert.ok(executable, "Rust test executable missing");

const temporary = await mkdtemp(join(tmpdir(), "maa-server-integration-"));
const controlKey = randomBytes(32).toString("hex");
let database;
let backend;
let proxy;
const ports = [];
let dropPush = false;
let dropChangeAt = 0;
let changeCount = 0;
let legacyMode = false;
let closed = false;

async function closeServer(server) {
  if (!server?.listening) return;
  server.closeAllConnections();
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}
async function listen(server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  ports.push(server.address().port);
  return server;
}
async function startBackend() {
  const mapper = new MysqlCodexUsageMapper(database.pool);
  const accounts = new MysqlCodexUsageAccountMapper(database.pool);
  const previous = process.cwd();
  process.chdir(repo);
  try { backend = httpServer(syncApp(mapper, accounts)); }
  finally { process.chdir(previous); }
  await listen(backend);
}
async function cleanup() {
  if (closed) return;
  closed = true;
  child?.kill();
  const failures = [];
  async function attempt(stage, work) {
    try { await work(); }
    catch { failures.push(stage); }
  }
  await attempt("HTTPS listener", () => closeServer(proxy));
  await attempt("HTTP listener", () => closeServer(backend));
  if (database) await attempt("isolated MySQL tables", async () => {
    const { database: name, prefix } = database;
    await database.close();
    const { createConnection } = requireServer("mysql2/promise");
    const connection = await createConnection(endpoint);
    try {
      const [rows] = await connection.query("SELECT TABLE_NAME FROM information_schema.tables WHERE TABLE_SCHEMA = ?", [name]);
      assert.equal(rows.filter((row) => row.TABLE_NAME.startsWith(prefix)).length, 0, "Test tables remain");
    } finally { await connection.end(); }
    console.log("CLEANUP: isolated MySQL tables removed and verified");
  });
  const { Socket } = await import("node:net");
  for (const port of ports) {
    await attempt("listener verification", () => new Promise((resolve, reject) => {
      const socket = new Socket();
      socket.setTimeout(3000, () => { socket.destroy(); reject(new Error("Listener check timed out")); });
      socket.once("error", () => { socket.destroy(); resolve(); });
      socket.once("connect", () => { socket.destroy(); reject(new Error("Test listener remains open")); });
      socket.connect(port, "127.0.0.1");
    }));
  }
  // mkdtemp returns an exact directory created by this run; never delete a repository or shared path.
  await attempt("temporary files", async () => {
    assert.ok(temporary.startsWith(join(tmpdir(), "maa-server-integration-")));
    await rm(temporary, { recursive: true });
  });
  if (failures.length) {
    console.error(`CLEANUP FAILED: ${failures.join(", ")}`);
    process.exitCode = 1;
  } else {
    console.log("CLEANUP: listeners closed; TLS material and local test files removed");
  }
}
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => {
  cleanup().finally(() => process.exit(130));
});

try {
  // Generate an ephemeral CA/leaf in memory, without installing a certificate into Windows trust stores.
  await processRun("pwsh", ["-NoProfile", "-NonInteractive", "-Command", `
    $ErrorActionPreference = 'Stop'
    $rsa = [System.Security.Cryptography.RSA]::Create(2048)
    $hash = [System.Security.Cryptography.HashAlgorithmName]::SHA256
    $padding = [System.Security.Cryptography.RSASignaturePadding]::Pkcs1
    $request = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=MAA integration test', $rsa, $hash, $padding)
    $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true, $false, 0, $true))
    $root = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-5), [DateTimeOffset]::UtcNow.AddDays(1))
    $leafKey = [System.Security.Cryptography.RSA]::Create(2048)
    $leafRequest = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=localhost', $leafKey, $hash, $padding)
    $san = [System.Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
    $san.AddDnsName('localhost')
    $san.AddIpAddress([System.Net.IPAddress]::Parse('127.0.0.1'))
    $leafRequest.CertificateExtensions.Add($san.Build())
    $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false, $false, 0, $true))
    $oids = [System.Security.Cryptography.OidCollection]::new()
    [void]$oids.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
    $leafRequest.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($oids, $false))
    $serial = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(16)
    $leaf = $leafRequest.Create($root, [DateTimeOffset]::UtcNow.AddMinutes(-5), [DateTimeOffset]::UtcNow.AddHours(12), $serial)
    [System.IO.File]::WriteAllText((Join-Path $env:MAA_TEST_TLS_DIR 'ca.pem'), $root.ExportCertificatePem())
    [System.IO.File]::WriteAllText((Join-Path $env:MAA_TEST_TLS_DIR 'cert.pem'), $leaf.ExportCertificatePem())
    [System.IO.File]::WriteAllText((Join-Path $env:MAA_TEST_TLS_DIR 'key.pem'), $leafKey.ExportPkcs8PrivateKeyPem())
    $leaf.Dispose(); $root.Dispose(); $rsa.Dispose(); $leafKey.Dispose()
  `], { env: { ...safeEnv, MAA_TEST_TLS_DIR: temporary } });

  database = await isolatedMysql(endpoint);
  assert.match(database.version, /^8\./);
  console.log(`MySQL ${database.version}: isolated random-prefix tables`);
  await runMigrations({
    async query(sql, params) { const [rows] = await database.pool.query(sql, params); return rows; },
    async execute(sql, params) { await database.pool.query(sql, params); },
  });
  await startBackend();
  const tls = { key: await readFile(join(temporary, "key.pem")), cert: await readFile(join(temporary, "cert.pem")) };
  proxy = await listen(httpsServer(tls, async (req, res) => {
    if (req.url === "/_test/control" && req.method === "POST") {
      if (req.headers.authorization !== `Bearer ${controlKey}`) { res.writeHead(403).end(); return; }
      try {
        const chunks = [];
        let size = 0;
        for await (const chunk of req) { size += chunk.length; if (size > 4096) throw new Error(); chunks.push(chunk); }
        const body = JSON.parse(Buffer.concat(chunks).toString());
        switch (body.action) {
          case "drop-push-response": dropPush = true; break;
          case "drop-second-page": dropChangeAt = 2; changeCount = 0; break;
          case "restart": await closeServer(backend); await startBackend(); break;
          case "epoch":
            assert.match(body.accountId, /^[a-f0-9]{32}$/);
            await database.pool.query("UPDATE codex_usage_space SET epoch = ? WHERE id = (SELECT space FROM codex_usage_account WHERE id = ?)",
              [Buffer.from(randomBytes(16).toString("hex")), body.accountId]);
            break;
          case "fail-write":
            assert.match(body.deviceId, /^[a-f0-9]{32}$/);
            await database.pool.query(`ALTER TABLE codex_usage_source ADD CONSTRAINT ${database.prefix}reject_write CHECK (device_id <> '${body.deviceId}')`);
            break;
          case "restore-write":
            await database.pool.query(`ALTER TABLE codex_usage_source DROP CHECK ${database.prefix}reject_write`);
            break;
          case "legacy-mode":
            assert.equal(typeof body.enabled, "boolean");
            legacyMode = body.enabled;
            break;
          default: throw new Error();
        }
        res.setHeader("Content-Type", "application/json");
        res.end('{"ok":true}');
      } catch { res.writeHead(500).end('{"ok":false}'); }
      return;
    }
    if (!req.url.startsWith("/maa/")) { res.writeHead(404).end(); return; }
    const path = req.url.slice(4);
    if (legacyMode && path.startsWith("/api/v1/codex-usage/changes?") &&
        new URL(path, "http://localhost").searchParams.has("excludeDeviceId")) {
      res.writeHead(400, { "Content-Type": "application/json" }).end('{"code":"invalid_request"}');
      return;
    }
    const upstream = httpRequest({ host: "127.0.0.1", port: backend.address().port,
      method: req.method, path, headers: req.headers }, (response) => {
      const isPush = path === "/api/v1/codex-usage/push";
      const isChanges = path.startsWith("/api/v1/codex-usage/changes?");
      if (legacyMode && path === "/api/v1/codex-usage/handshake" && response.statusCode === 200) {
        const chunks = [];
        response.on("data", chunk => chunks.push(chunk));
        response.on("end", () => {
          const handshake = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          delete handshake.capabilities;
          const body = Buffer.from(JSON.stringify(handshake));
          const headers = { ...response.headers, "content-length": body.length };
          delete headers["transfer-encoding"];
          res.writeHead(200, headers).end(body);
        });
        response.on("error", () => res.destroy());
        return;
      }
      const loseResponse = response.statusCode === 200 &&
        ((isPush && dropPush) || (isChanges && dropChangeAt > 0 && ++changeCount === dropChangeAt));
      if (loseResponse) {
        if (isPush) dropPush = false;
        else dropChangeAt = 0;
        response.resume();
        response.once("end", () => res.destroy());
      } else {
        res.writeHead(response.statusCode, response.headers);
        response.pipe(res);
      }
    });
    upstream.on("error", () => res.destroy());
    req.pipe(upstream);
  }));
  console.log("Running actual MAA Rust sync over HTTPS /maa prefix against the server's HTTP routes and MySQL mappers");
  await processRun(executable, ["live_server_interoperability", "--ignored", "--nocapture", "--test-threads=1"], {
    stream: true,
    env: { ...safeEnv, MAA_TEST_SERVER_URL: `https://127.0.0.1:${proxy.address().port}/maa`,
      MAA_TEST_CA: join(temporary, "ca.pem"), MAA_TEST_ROOT: temporary, MAA_TEST_CONTROL_KEY: controlKey },
  });
} catch (error) {
  console.error(`Integration failed: ${error?.code || (error?.message === "Test subprocess failed" ? error.message : "setup or assertion failure")}`);
  process.exitCode = 1;
} finally {
  await cleanup();
}
