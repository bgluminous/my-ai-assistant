// node --test scripts/test-account-refresh.mjs
// 执行真实调度器，使用假时钟和隔离的刷新入口，不访问账户、网络或持久化配置。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("../src/account_refresh.js", import.meta.url), "utf8");
const { createAccountRefreshScheduler, refreshByProvider } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);
const minute = 60_000;
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

function fixture(accounts, global = 0) {
  let time = 0, timerId = 0, order = accounts.map(account => account.id), blocked = false;
  const timers = new Map(), refreshing = new Set(), blockedProviders = new Set(), calls = [], errors = [], running = [];
  const hooks = { account: async () => {}, shared: async () => {} };
  const scheduler = createAccountRefreshScheduler({
    now: () => time,
    setTimer: (callback, delay) => { const id = ++timerId; timers.set(id, { callback, at: time + delay }); return id; },
    clearTimer: id => timers.delete(id),
    getOrder: () => order,
    isBlocked: () => blocked,
    isProviderBlocked: kind => blockedProviders.has(kind),
    isRefreshing: id => refreshing.has(id),
    refreshAccount: async id => { calls.push({ id, at: time }); await hooks.account(id); },
    refreshShared: async () => { calls.push({ id: "shared", at: time }); await hooks.shared(); },
    onRunningChange: (kinds, shared) => running.push({ kinds, shared }),
    onError: error => errors.push(error),
  });
  scheduler.update(accounts.map(account => ({ kind: "cursor", ...account })), global);
  const next = () => [...timers].sort((a, b) => a[1].at - b[1].at)[0];
  return {
    scheduler, timers, refreshing, blockedProviders, calls, errors, running, hooks,
    update(list, minutes) { order = list.map(account => account.id); scheduler.update(list.map(account => ({ kind: "cursor", ...account })), minutes); },
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
  assert.deepEqual(f.running, [{ kinds: ["cursor"], shared: false }, { kinds: [], shared: false }]);
});

test("all providers run concurrently and fast providers keep their own deadlines while one is blocked", async () => {
  const kinds = ["cursor", "codex", "claude", "opencode"];
  const accounts = kinds.flatMap(kind => [1, 2].map(n => ({ id: `${kind}-${n}`, kind, refreshIntervalMinutes: 5 })));
  const f = fixture(accounts, 5), slow = deferred();
  f.hooks.account = id => id === "cursor-1" ? slow.promise : Promise.resolve();
  const first = f.startNext();
  await tick();
  assert.deepEqual(f.calls.slice(0, 5).map(call => call.id), ["shared", ...kinds.map(kind => `${kind}-1`)]);
  assert.equal(f.calls.some(call => call.id === "cursor-2"), false);
  for (const kind of kinds.slice(1)) assert.ok(f.calls.some(call => call.id === `${kind}-2`));
  await f.advance(5 * minute);
  assert.equal(f.calls.filter(call => call.id === "codex-1").length, 2);
  assert.equal(f.calls.filter(call => call.id === "shared").length, 2);
  assert.equal(f.calls.filter(call => call.id === "cursor-1").length, 1);
  slow.resolve(); await first;
  assert.equal(f.calls.at(-1).id, "cursor-2");
  assert.deepEqual(f.running.at(-1), { kinds: [], shared: false });
});

test("provider blocking and shared work never suspend unrelated queues", async () => {
  const f = fixture([{ id: "a", kind: "cursor" }, { id: "b", kind: "codex" }], 5);
  const shared = deferred(); f.hooks.shared = () => shared.promise;
  f.blockedProviders.add("cursor");
  const first = f.startNext(); await tick();
  assert.deepEqual(f.calls.map(call => call.id), ["shared", "b"]);
  f.blockedProviders.delete("cursor");
  await f.advance(1000);
  assert.equal(f.calls.at(-1).id, "a");
  await f.advance(5 * minute);
  assert.equal(f.calls.filter(call => call.id === "b").length, 2);
  shared.resolve(); await first;
});

test("queued tasks recheck disabled and deleted accounts without resetting unrelated deadlines", async () => {
  const accounts = ["a", "b", "c"].map(id => ({ id, kind: "cursor", refreshIntervalMinutes: 5 }));
  const f = fixture(accounts), pending = deferred();
  f.hooks.account = id => id === "a" ? pending.promise : Promise.resolve();
  const first = f.startNext();
  f.update([{ ...accounts[0], note: "changed" }, { ...accounts[1], refreshIntervalMinutes: 0 }], 0);
  pending.resolve(); await first;
  assert.deepEqual(f.calls.map(call => call.id), ["a"]);
  await f.advance(5 * minute);
  assert.deepEqual(f.calls.map(call => call.id), ["a", "a"]);
  f.scheduler.dispose();
  assert.equal(f.timers.size, 0);
});

test("batch refresh preserves group order, continues after failure and reports completed progress", async () => {
  const slow = deferred(), calls = [], progress = [], states = [];
  const batch = [{ id: "a", kind: "cursor" }, { id: "b", kind: "codex" },
    { id: "c", kind: "cursor" }, { id: "d", kind: "codex" }];
  const job = refreshByProvider(batch, async id => {
    calls.push(id);
    if (id === "a") await slow.promise;
    if (id === "b") throw new Error("offline");
  }, (done, total) => progress.push([done, total]), (kind, active) => states.push([kind, active]));
  await tick();
  assert.deepEqual(calls, ["a", "b", "d"]);
  assert.deepEqual(progress, [[1, 4], [2, 4]]);
  assert.deepEqual(states.at(-1), ["codex", false]);
  slow.resolve();
  const failures = await job;
  assert.deepEqual(calls, ["a", "b", "d", "c"]);
  assert.deepEqual(failures.map(item => item.id), ["b"]);
  assert.deepEqual(progress, [[1, 4], [2, 4], [3, 4], [4, 4]]);
});
