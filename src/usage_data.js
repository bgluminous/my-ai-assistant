import { invoke, emit } from "./shared.js";

// 主窗口用量页与托盘总览共用的聚合缓存。
// 内存 Map 仅本 WebView 有效；localStorage 同 origin 下主窗口 / 托盘互通。
// Cursor 键：agg:${accountId}:${rangeKey}，rangeKey 为 7 / 30 / 0（全部）或 today:YYYY-MM-DD。
// Codex 键：scan:${rangeKey}:${home}，rangeKey 为 7 / 30 / all 或 today:YYYY-MM-DD。

export const USAGE_CACHE_PREFIX = "usage-cache:v4:";
export const USAGE_CACHE_EVENT = "usage-cache-changed";

// 旧版缓存一次性清理：聚合口径随版本演进（v3 合并思考等级、v4 小版本号点号归一），
// 旧条目继续保留只会与新数据混排。
try {
  for (let i = localStorage.length - 1; i >= 0; i -= 1) {
    const k = localStorage.key(i);
    if (k && (k.startsWith("usage-cache:v2:") || k.startsWith("usage-cache:v3:"))) {
      localStorage.removeItem(k);
    }
  }
} catch { /* ignore */ }
// 本窗口的写入者标识：广播缓存变更时带上，接收方据此忽略自己窗口的写入
// （事件会回送到发出的窗口，不过滤会造成无意义的重渲染循环）。
export const USAGE_CACHE_ORIGIN = `w-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
export const DEFAULT_USAGE_TTL_MS = 5 * 60_000;
const PEEK_DAY_RANGES = ["7", "30", "0"];
const PEEK_SCAN_RANGES = ["7", "30", "all"];

let ttlMs = DEFAULT_USAGE_TTL_MS;
const memAgg = new Map();
const memScan = new Map();
const inflight = new Map();

export function setUsageCacheTtlMs(ms) {
  const n = Number(ms);
  ttlMs = Number.isFinite(n) && n > 0 ? n : DEFAULT_USAGE_TTL_MS;
}

export function usageCacheTtlMs() {
  return ttlMs;
}

export function isUsageCacheFresh(at) {
  return Number.isFinite(at) && at > 0 && Date.now() - at < ttlMs;
}

export function clearUsageMemoryCache() {
  memAgg.clear();
  memScan.clear();
}

export function localYmd(d = new Date()) {
  const x = d instanceof Date ? d : new Date(d);
  const p = (n) => String(n).padStart(2, "0");
  return `${x.getFullYear()}-${p(x.getMonth() + 1)}-${p(x.getDate())}`;
}

export function todayStartMs(d = new Date()) {
  const x = d instanceof Date ? d : new Date(d);
  return new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
}

export function todayRangeKey(ymd = localYmd()) {
  return `today:${ymd}`;
}

export function recentYmds(n, end = new Date()) {
  const end0 = new Date(end.getFullYear(), end.getMonth(), end.getDate());
  const labels = [];
  for (let i = n - 1; i >= 0; i -= 1) {
    const d = new Date(end0.getFullYear(), end0.getMonth(), end0.getDate() - i);
    labels.push(localYmd(d));
  }
  return labels;
}

export function dailyOn(agg, ymd) {
  const list = agg && Array.isArray(agg.daily) ? agg.daily : [];
  return list.find((d) => d && d.date === ymd) || null;
}

function notifyUsageCache(key) {
  emit(USAGE_CACHE_EVENT, { key, origin: USAGE_CACHE_ORIGIN });
}

function cacheStore(key, entry) {
  const full = USAGE_CACHE_PREFIX + key;
  try { localStorage.setItem(full, JSON.stringify(entry)); } catch { /* ignore */ }
  notifyUsageCache(full);
}

/** 对端写入 localStorage / 广播后，丢掉本窗口内存里的旧条目，下次 peek 从 storage 重读。 */
export function forgetUsageCacheFromEvent(key) {
  if (!key || key === "*" || key === USAGE_CACHE_PREFIX) {
    clearUsageMemoryCache();
    return;
  }
  if (!key.startsWith(USAGE_CACHE_PREFIX)) return;
  const rest = key.slice(USAGE_CACHE_PREFIX.length);
  if (rest.startsWith("agg:")) memAgg.delete(rest.slice(4));
  else if (rest.startsWith("scan:")) memScan.delete(rest.slice(5));
}

function cacheLoad(key) {
  try {
    const raw = localStorage.getItem(USAGE_CACHE_PREFIX + key);
    if (!raw) return null;
    const v = JSON.parse(raw);
    return v && Number.isFinite(v.at) ? v : null;
  } catch {
    return null;
  }
}

export function getCachedAgg(accountId, rangeKey) {
  const key = `${accountId}:${rangeKey}`;
  let entry = memAgg.get(key) || null;
  if (!entry) {
    const stored = cacheLoad(`agg:${key}`);
    if (stored && stored.agg && Array.isArray(stored.agg.models)) {
      entry = stored;
      memAgg.set(key, entry);
    }
  }
  return entry;
}

export function getCachedScan(rangeKey, home) {
  const key = `${rangeKey}:${home || ""}`;
  let entry = memScan.get(key) || null;
  if (!entry) {
    const stored = cacheLoad(`scan:${key}`);
    if (stored && stored.scan && stored.scan.aggregate) {
      entry = stored;
      memScan.set(key, entry);
    }
  }
  return entry;
}

/** 优先今日键，其次 7/30/90/全部（用 daily 切片出当天）。 */
export function peekAggForDay(accountId, ymd) {
  const todayKey = todayRangeKey(ymd);
  const today = getCachedAgg(accountId, todayKey);
  if (today) return { entry: today, rangeKey: todayKey };
  for (const rk of PEEK_DAY_RANGES) {
    const entry = getCachedAgg(accountId, rk);
    if (entry && entry.agg) return { entry, rangeKey: rk };
  }
  return null;
}

export function peekScanForDay(ymd, home) {
  const h = home || "";
  const todayKey = todayRangeKey(ymd);
  const today = getCachedScan(todayKey, h);
  if (today) return { entry: today, rangeKey: todayKey };
  for (const rk of PEEK_SCAN_RANGES) {
    const entry = getCachedScan(rk, h);
    if (entry && entry.scan && entry.scan.aggregate) return { entry, rangeKey: rk };
  }
  return null;
}

/** 优先 30/7/全部的按日序列，供托盘近 7 日柱图；没有再退回今日键。 */
export function peekAggSeries(accountId, ymd) {
  for (const rk of ["30", "7", "0"]) {
    const entry = getCachedAgg(accountId, rk);
    if (entry && entry.agg && Array.isArray(entry.agg.daily) && entry.agg.daily.length) {
      return { entry, rangeKey: rk };
    }
  }
  return peekAggForDay(accountId, ymd);
}

export function peekScanSeries(ymd, home) {
  const h = home || "";
  for (const rk of ["30", "7", "all"]) {
    const entry = getCachedScan(rk, h);
    if (entry && entry.scan && entry.scan.aggregate && Array.isArray(entry.scan.aggregate.daily)
      && entry.scan.aggregate.daily.length) {
      return { entry, rangeKey: rk };
    }
  }
  return peekScanForDay(ymd, h);
}

function attachUsageCache(error, cached) {
  const err = error instanceof Error ? error : new Error(String(error));
  if (cached) err.usageCache = cached;
  return err;
}

// 单次统计拉取的兜底超时。正常失败由后端 40s/请求超时保证会返回错误；
// 这里防的是命令异常（如 panic）导致 invoke 永不落定：一旦发生，inflight
// 去重会把挂起的 Promise 无限复用，界面永远停在「更新中」且刷新无效。
// 超时后转为普通错误，inflight 随之清理，下次刷新即可重试。
const FETCH_TIMEOUT_MS = 10 * 60_000;

function withTimeout(promise, label) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label}_timeout`)), FETCH_TIMEOUT_MS);
    promise.then(
      (value) => { clearTimeout(timer); resolve(value); },
      (error) => { clearTimeout(timer); reject(error); }
    );
  });
}

export function fetchCursorAggregate(account, rangeKey, { start, end, force } = {}) {
  const key = `${account.id}:${rangeKey}`;
  const cached = getCachedAgg(account.id, rangeKey);
  if (!force && cached && isUsageCacheFresh(cached.at)) return Promise.resolve(cached);
  const inflightKey = `agg:${key}`;
  if (inflight.has(inflightKey)) return inflight.get(inflightKey);
  const p = withTimeout(
    invoke("cursor_aggregate", {
      sessionToken: account.token,
      start: start ?? null,
      end: end ?? null,
      // 「全部」跨度的成功结果由后端顺手写入磁盘存档（usage-archive/<账户id>.json），
      // 作为账户失效后「生成快照」的兜底数据源。
      archiveAccountId: rangeKey === "0" ? account.id : null,
    }),
    "cursor_aggregate"
  )
    .then((agg) => {
      const entry = { agg, at: Date.now() };
      memAgg.set(key, entry);
      cacheStore(`agg:${key}`, entry);
      return entry;
    })
    .catch((error) => {
      throw attachUsageCache(error, cached);
    })
    .finally(() => inflight.delete(inflightKey));
  inflight.set(inflightKey, p);
  return p;
}

export function fetchCodexScan({ days, sinceMs, home, force } = {}) {
  const h = home || "";
  const rangeKey =
    sinceMs != null && Number.isFinite(Number(sinceMs))
      ? todayRangeKey(localYmd(Number(sinceMs)))
      : days == null || days === 0
        ? "all"
        : String(days);
  const key = `${rangeKey}:${h}`;
  const cached = getCachedScan(rangeKey, h);
  if (!force && cached && isUsageCacheFresh(cached.at)) return Promise.resolve(cached);
  const inflightKey = `scan:${key}`;
  if (inflight.has(inflightKey)) return inflight.get(inflightKey);
  const p = withTimeout(
    invoke("codex_scan_sessions", {
      days: days == null || days === 0 ? null : days,
      sinceMs: sinceMs ?? null,
      home: h || null,
    }),
    "codex_scan_sessions"
  )
    .then((scan) => {
      const entry = { scan, at: Date.now() };
      memScan.set(key, entry);
      cacheStore(`scan:${key}`, entry);
      return entry;
    })
    .catch((error) => {
      throw attachUsageCache(error, cached);
    })
    .finally(() => inflight.delete(inflightKey));
  inflight.set(inflightKey, p);
  return p;
}

export function purgeAccountCache(accountId) {
  const memPrefix = `${accountId}:`;
  for (const key of [...memAgg.keys()]) {
    if (key.startsWith(memPrefix)) memAgg.delete(key);
  }
  try {
    const prefix = `${USAGE_CACHE_PREFIX}agg:${accountId}:`;
    for (let i = localStorage.length - 1; i >= 0; i -= 1) {
      const k = localStorage.key(i);
      if (k && k.startsWith(prefix)) localStorage.removeItem(k);
    }
  } catch { /* ignore */ }
  notifyUsageCache(USAGE_CACHE_PREFIX);
}

function aggAccountIdFromMemKey(key) {
  const idx = key.indexOf(":today:");
  if (idx >= 0) return key.slice(0, idx);
  const last = key.lastIndexOf(":");
  return last >= 0 ? key.slice(0, last) : key;
}

export function purgeMissingAccounts(list) {
  const ids = new Set((list || []).map((a) => a.id));
  for (const key of [...memAgg.keys()]) {
    if (!ids.has(aggAccountIdFromMemKey(key))) memAgg.delete(key);
  }
  try {
    const prefix = `${USAGE_CACHE_PREFIX}agg:`;
    for (let i = localStorage.length - 1; i >= 0; i -= 1) {
      const k = localStorage.key(i);
      if (!k || !k.startsWith(prefix)) continue;
      const rest = k.slice(prefix.length);
      if (!ids.has(aggAccountIdFromMemKey(rest))) localStorage.removeItem(k);
    }
  } catch { /* ignore */ }
  notifyUsageCache(USAGE_CACHE_PREFIX);
}

/** 把某一天的切片写回 daily[]，避免今日数字与近 7 日柱的当日列不一致。 */
export function overlayDay(daily, ymd, slice) {
  const list = Array.isArray(daily) ? daily.map((d) => ({ ...d })) : [];
  const tokens = Number(slice && slice.tokens) || 0;
  const usd = Number(slice && slice.usd) || 0;
  const i = list.findIndex((d) => d && d.date === ymd);
  if (i >= 0) {
    list[i] = { ...list[i], tokens, equivalentUsd: usd };
    return list;
  }
  if (tokens > 0) list.push({ date: ymd, tokens, equivalentUsd: usd, actualUsd: usd });
  return list;
}

/** 从任意范围的聚合里切出某一天的 token / 等价费用（无按日数据时，仅今日键可用合计）。 */
export function sliceDay(agg, ymd, rangeKey) {
  if (!agg) return { tokens: 0, usd: 0 };
  const day = dailyOn(agg, ymd);
  if (day) return { tokens: Number(day.tokens) || 0, usd: Number(day.equivalentUsd) || 0 };
  if (String(rangeKey || "").startsWith("today:")) {
    return { tokens: Number(agg.totalTokens) || 0, usd: Number(agg.totalEquivalentUsd) || 0 };
  }
  return { tokens: 0, usd: 0 };
}

/** 当天各模型用量：优先 daily[].models，今日键则退回合计 models。 */
export function modelsOnDay(agg, ymd, rangeKey) {
  if (!agg) return [];
  const day = dailyOn(agg, ymd);
  if (day && Array.isArray(day.models) && day.models.length) {
    return day.models
      .map((m) => ({
        model: m.model,
        tokens: Number(m.tokens) || 0,
        usd: Number(m.equivalentUsd) || 0,
      }))
      .filter((m) => m.tokens > 0);
  }
  if (String(rangeKey || "").startsWith("today:") && Array.isArray(agg.models)) {
    return agg.models
      .map((m) => ({
        model: m.model,
        tokens: Number(m.totalTokens) || 0,
        usd: Number(m.equivalentUsd) || 0,
      }))
      .filter((m) => m.tokens > 0);
  }
  return [];
}
