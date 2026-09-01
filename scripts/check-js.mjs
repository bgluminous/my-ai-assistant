import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

const dir = new URL("../src/", import.meta.url);
const files = readdirSync(dir).filter((f) => f.endsWith(".js"));
let failures = 0;
for (const file of files) {
  const content = readFileSync(new URL(file, dir), "utf8");
  const tmp = join(tmpdir(), `check-${Date.now()}-${file}.mjs`);
  writeFileSync(tmp, content, "utf8");
  try {
    execFileSync(process.execPath, ["--check", tmp], { stdio: "pipe" });
    console.log("ok  ", file);
  } catch (error) {
    failures++;
    const msg = (error.stderr ? error.stderr.toString() : error.message).split("\n").slice(0, 3).join(" ");
    console.log("FAIL", file, "->", msg);
  }
}
console.log(failures === 0 ? "ALL_JS_OK" : `JS_FAILURES=${failures}`);
process.exit(failures === 0 ? 0 : 1);
