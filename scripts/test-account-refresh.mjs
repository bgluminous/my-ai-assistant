// node --test scripts/test-account-refresh.mjs
// 执行真实调度器，使用假时钟和隔离的刷新入口，不访问账户、网络或持久化配置。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("../src/account_refresh.js", import.meta.url), "utf8");
const { createAccountRefreshScheduler } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const minute = 60_000;

function fixture(accounts, global = 0) {
  let time = 0, timerId = 0, order = accounts.map(account => account.id), blocked = false;
  const timers = new Map(), refreshing = new Set(), calls = [], errors = [], running = [];
  const hooks = { account: async () => {}, shared: async () => {} };
  const scheduler = createAccountRefreshScheduler({
    now: () => time,
    setTimer: (callback, delay) => { const id = ++timerId; timers.set(id, { callback, at: time + delay }); return id; },
    clearTimer: id => timers.delete(id),
    getOrder: () => order,
    isBlocked: () => blocked,
    isRefreshing: id => refreshing.has(id),
    refreshAccount: async id => { calls.push({ id, at: time }); await hooks.account(id); },
    refreshShared: async () => { calls.push({ id: "shared", at: time }); await hooks.shared(); },
    onRunningChange: value => running.push(value),
    onError: error => errors.push(error),
  });
  scheduler.update(accounts, global);
  const next = () => [...timers].sort((a, b) => a[1].at - b[1].at)[0];
  return {
    scheduler, timers, refreshing, calls, errors, running, hooks,
    update(list, minutes) { order = list.map(account => account.id); scheduler.update(list, minutes); },
    block(value) { blocked = value; },
    elapse(ms) { time += ms; },
    startNext() { const [id, task] = next(); time = task.at; timers.delete(id); return task.callback(); },
    async advance(ms) {
      const until = time + ms;
      let turns = 0;
      while (next()?.[1].at <= until) {
        assert.ok(++turns < 2000, "scheduler must not spin");
        const [id, task] = next(); timers.delete(id); time = task.at;
        await task.callback();
      }
      time = Math.max(until, time);
    },
  };
}

test("startup and later deadlines respect overrides, disabled accounts and display order", async () => {
  const accounts = [{ id: "opencode", refreshIntervalMinutes: 5 }, { id: "cursor" },
    { id: "claude", refreshIntervalMinutes: 0 }, { id: "codex", refreshIntervalMinutes: null }];
  const f = fixture(accounts, 15);
  assert.equal(f.timers.size, 1);
  await f.advance(2999); assert.equal(f.calls.length, 0);
  await f.advance(1);
  assert.deepEqual(f.calls.map(call => call.id), ["shared", "opencode", "cursor", "codex"]);
  await f.advance(5 * minute);
  assert.equal(f.calls.at(-1).id, "opencode");
  assert.equal(f.calls.filter(call => call.id === "shared").length, 1);
  await f.advance(10 * minute);
  assert.deepEqual(f.calls.filter(call => call.at === 903000).map(call => call.id), ["shared", "opencode", "cursor", "codex"]);
  assert.equal(f.timers.size, 1);
  f.scheduler.dispose(); assert.equal(f.timers.size, 0);
});

test("independent accounts run with global disabled; shared jobs run without accounts", async () => {
  const f = fixture([{ id: "inherited" }, { id: "custom", refreshIntervalMinutes: 5 }]);
  await f.advance(3000);
  assert.deepEqual(f.calls.map(call => call.id), ["custom"]);
  await f.advance(5 * minute);
  assert.deepEqual(f.calls.map(call => call.id), ["custom", "custom"]);
  const shared = fixture([], 5);
  await shared.advance(3000);
  assert.deepEqual(shared.calls.map(call => call.id), ["shared"]);
  shared.update([], 0); assert.equal(shared.timers.size, 0);
});

test("only affected deadlines restart on settings changes and metadata updates preserve deadlines", async () => {
  const accounts = [{ id: "inherited" }, { id: "custom", refreshIntervalMinutes: 5 }];
  const f = fixture(accounts, 5);
  await f.advance(3000); f.calls.length = 0;
  await f.advance(minute);
  f.update(accounts.map(account => ({ ...account, note: "new name" })), 5);
  await f.advance(4 * minute);
  assert.deepEqual(f.calls.map(call => call.id), ["shared", "inherited", "custom"]);
  f.calls.length = 0;
  f.update([{ id: "inherited" }, { id: "custom", refreshIntervalMinutes: 15 }], 0);
  await f.advance(15 * minute - 1); assert.equal(f.calls.length, 0);
  await f.advance(1); assert.deepEqual(f.calls.map(call => call.id), ["custom"]);
  f.update([{ id: "custom", refreshIntervalMinutes: null }], 0);
  assert.equal(f.timers.size, 0);
});

test("failure retries after a full interval from completion; manual completion moves the next deadline", async () => {
  const f = fixture([{ id: "a", refreshIntervalMinutes: 5 }]);
  f.hooks.account = async () => { f.elapse(2000); throw new Error("offline"); };
  await f.advance(3000); assert.equal(f.errors.length, 1);
  await f.advance(5 * minute - 1); assert.equal(f.calls.length, 1);
  await f.advance(1); assert.equal(f.calls.length, 2);
  f.hooks.account = async () => {};
  await f.advance(minute);
  f.scheduler.completed("a");
  await f.advance(5 * minute - 1); assert.equal(f.calls.length, 2);
  await f.advance(1); assert.equal(f.calls.length, 3);
});

test("busy operations defer work and another window can complete an account without a duplicate request", async () => {
  const f = fixture([{ id: "a", refreshIntervalMinutes: 5 }, { id: "b", refreshIntervalMinutes: 5 }]);
  f.block(true); await f.advance(3000); assert.equal(f.calls.length, 0);
  f.block(false); f.refreshing.add("a");
  await f.advance(1000);
  assert.deepEqual(f.calls.map(call => call.id), ["b"]);
  f.scheduler.completed("a"); f.refreshing.delete("a");
  await f.advance(5 * minute - 1); assert.equal(f.calls.length, 1);
  await f.advance(1); assert.deepEqual(f.calls.slice(1).map(call => call.id), ["a", "b"]);
});

test("queued accounts are rechecked and removing in-flight accounts never resurrects timers", async () => {
  const f = fixture([{ id: "a", refreshIntervalMinutes: 5 }, { id: "b", refreshIntervalMinutes: 5 }]);
  let resolve;
  f.hooks.account = () => new Promise(done => { resolve = done; });
  const first = f.startNext();
  assert.deepEqual(f.calls.map(call => call.id), ["a"]);
  f.update([], 0);
  resolve(); await first;
  assert.deepEqual(f.calls.map(call => call.id), ["a"]);
  assert.equal(f.timers.size, 0);
  assert.deepEqual(f.running, [true, false]);
});
