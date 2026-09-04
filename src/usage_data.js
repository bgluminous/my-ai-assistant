import { invoke, emit } from "./shared.js";

// 主窗口用量页与托盘总览共用的聚合缓存。
// 内存 Map 仅本 WebView 有效；localStorage 同 origin 下主窗口 / 托盘互通。
// Cursor 键：agg:${accountId}:${rangeKey}，rangeKey 为 7 / 30 / 0（全部）、today:YYYY-MM-DD
//   （进行中的今天，数据随时间增长）或 day:YYYY-MM-DD（已结束的完整自然日，如「昨天」）。
// Codex 键：scan:${rangeKey}:${home}，rangeKey 为 7 / 30 / all 或 today: / day: 同上。
// Claude 键：cscan:${rangeKey}:${home}，rangeKey 同 Codex（本地 Claude Code 会话扫描）。

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
const inflight = new Map();

export function setUsageCacheTtlMs(ms) {
  const n = Number(ms);
  ttlMs = Number.isFinite(n) && n > 0 ? n : DEFAULT_USAGE_TTL_MS;
}

export function isUsageCacheFresh(at) {
  return Number.isFinite(at) && at > 0 && Date.now() - at < ttlMs;
}

function clearUsageMemoryCache() {
  memAgg.clear();
  for (const src of SCAN_SOURCES) src.mem.clear();
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

/** 相对今天偏移 offsetDays 天的本地自然日 0 点（按日历日推算，夏令时切换日也正确）。 */
export function dayStartMs(offsetDays = 0, d = new Date()) {
  const x = d instanceof Date ? d : new Date(d);
  return new Date(x.getFullYear(), x.getMonth(), x.getDate() + offsetDays).getTime();
}

/** 本地日历日加减：返回 d 所在日往后 n 天的本地 0 点（Date）。 */
export function addLocalDays(d, n) {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
}

/** YYYY-MM-DD -> 该本地日 0 点的 Date。 */
export function parseYmd(ymd) {
  const [y, m, d] = String(ymd).split("-").map(Number);
  return new Date(y, m - 1, d);
}

export function todayRangeKey(ymd = localYmd()) {
  return `today:${ymd}`;
}

/** 已结束的完整自然日的缓存键（与 today: 区分：今天的缓存是进行中的部分数据）。 */
export function dayRangeKey(ymd) {
  return `day:${ymd}`;
}

function dailyOn(agg, ymd) {
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
  if (rest.startsWith("agg:")) {
    memAgg.delete(rest.slice(4));
    return;
  }
  for (const src of SCAN_SOURCES) {
    if (rest.startsWith(src.prefix)) {
      src.mem.delete(rest.slice(src.prefix.length));
      return;
    }
  }
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

/** 优先今日键，其次 7/30/全部（用 daily 切片出当天）。 */
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

/**
 * 已结束的自然日（如「昨天」）只能用完整数据：优先 day: 键；其次是在该日结束之后才拉取的
 * 7/30/全部序列（其 daily 切片已包含整天）。当天进行中拉的 today: 键与更早拉取的序列都是
 * 半天数据，不作回退。
 */
function pastDayHit(get, ymd, seriesRanges, hasData) {
  const dayKey = dayRangeKey(ymd);
  const day = get(dayKey);
  if (day) return { entry: day, rangeKey: dayKey };
  const dayEnd = addLocalDays(parseYmd(ymd), 1).getTime();
  for (const rk of seriesRanges) {
    const entry = get(rk);
    if (entry && hasData(entry) && entry.at >= dayEnd) return { entry, rangeKey: rk };
  }
  return null;
}

export function peekAggForPastDay(accountId, ymd) {
  return pastDayHit((rk) => getCachedAgg(accountId, rk), ymd, PEEK_DAY_RANGES, (e) => !!e.agg);
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

/**
 * 扫描范围 -> 缓存键：只有起点 = 进行中的今天（today:，跨零点自动失效）；
 * 起点 + 终点 = 已结束的完整自然日（day:，按起点所在日命名）；否则为天数 / all。
 */
function scanRangeKey(days, sinceMs, untilMs) {
  if (sinceMs != null && Number.isFinite(Number(sinceMs))) {
    const ymd = localYmd(Number(sinceMs));
    return untilMs != null ? dayRangeKey(ymd) : todayRangeKey(ymd);
  }
  return days == null || days === 0 ? "all" : String(days);
}

/**
 * 本地会话扫描来源（本地 Codex / Claude Code）的缓存读取、按日探查与拉取。两者只差内存表、
 * 存储键前缀（scan: / cscan:）与后端命令名，其余逻辑完全一致，由此工厂各生成一组函数。
 */
function makeScanSource({ prefix, command }) {
  const mem = new Map();
  const hasData = (entry) => !!(entry.scan && entry.scan.aggregate);

  const getCached = (rangeKey, home) => {
    const key = `${rangeKey}:${home || ""}`;
    let entry = mem.get(key) || null;
    if (!entry) {
      const stored = cacheLoad(`${prefix}${key}`);
      if (stored && hasData(stored)) {
        entry = stored;
        mem.set(key, entry);
      }
    }
    return entry;
  };

  /** 优先今日键，其次 7/30/全部（用 daily 切片出当天）。 */
  const peekForDay = (ymd, home) => {
    const h = home || "";
    const todayKey = todayRangeKey(ymd);
    const today = getCached(todayKey, h);
    if (today) return { entry: today, rangeKey: todayKey };
    for (const rk of PEEK_SCAN_RANGES) {
      const entry = getCached(rk, h);
      if (entry && hasData(entry)) return { entry, rangeKey: rk };
    }
    return null;
  };

  const peekForPastDay = (ymd, home) =>
    pastDayHit((rk) => getCached(rk, home || ""), ymd, PEEK_SCAN_RANGES, hasData);

  /** 优先 30/7/全部的按日序列，供托盘近 7 日柱图；没有再退回今日键。 */
  const peekSeries = (ymd, home) => {
    const h = home || "";
    for (const rk of ["30", "7", "all"]) {
      const entry = getCached(rk, h);
      if (entry && hasData(entry) && Array.isArray(entry.scan.aggregate.daily) && entry.scan.aggregate.daily.length) {
        return { entry, rangeKey: rk };
      }
    }
    return peekForDay(ymd, h);
  };

  const fetch = ({ days, sinceMs, untilMs, home, force } = {}) => {
    const h = home || "";
    const rangeKey = scanRangeKey(days, sinceMs, untilMs);
    const key = `${rangeKey}:${h}`;
    const cached = getCached(rangeKey, h);
    if (!force && cached && isUsageCacheFresh(cached.at)) return Promise.resolve(cached);
    const inflightKey = `${prefix}${key}`;
    if (inflight.has(inflightKey)) return inflight.get(inflightKey);
    const p = withTimeout(
      invoke(command, {
        days: days == null || days === 0 ? null : days,
        sinceMs: sinceMs ?? null,
        untilMs: untilMs ?? null,
        home: h || null,
      }),
      command
    )
      .then((scan) => {
        const entry = { scan, at: Date.now() };
        mem.set(key, entry);
        cacheStore(`${prefix}${key}`, entry);
        return entry;
      })
      .catch((error) => {
        throw attachUsageCache(error, cached);
      })
      .finally(() => inflight.delete(inflightKey));
    inflight.set(inflightKey, p);
    return p;
  };

  return { mem, prefix, getCached, peekForDay, peekForPastDay, peekSeries, fetch };
}

const codexScan = makeScanSource({ prefix: "scan:", command: "codex_scan_sessions" });
const claudeScan = makeScanSource({ prefix: "cscan:", command: "claude_scan_sessions" });
const SCAN_SOURCES = [codexScan, claudeScan];

export const getCachedScan = codexScan.getCached;
export const peekScanForDay = codexScan.peekForDay;
export const peekScanForPastDay = codexScan.peekForPastDay;
export const peekScanSeries = codexScan.peekSeries;
export const fetchCodexScan = codexScan.fetch;

export const getCachedClaudeScan = claudeScan.getCached;
export const peekClaudeScanForDay = claudeScan.peekForDay;
export const peekClaudeScanForPastDay = claudeScan.peekForPastDay;
export const peekClaudeScanSeries = claudeScan.peekSeries;
export const fetchClaudeScan = claudeScan.fetch;

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

/** 从内存键 ${accountId}:${rangeKey} 取回账户 id（带日期的 today: / day: 键自身含冒号）。 */
function aggAccountIdFromMemKey(key) {
  const m = /:(?:today|day):/.exec(key);
  if (m) return key.slice(0, m.index);
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

/** 单日键（today: / day:）：聚合范围就是那一天，合计即当日合计。 */
function isSingleDayKey(rangeKey) {
  return /^(?:today|day):/.test(String(rangeKey || ""));
}

/** 从任意范围的聚合里切出某一天的 token / 等价费用（无按日数据时，仅单日键可用合计）。 */
export function sliceDay(agg, ymd, rangeKey) {
  if (!agg) return { tokens: 0, usd: 0 };
  const day = dailyOn(agg, ymd);
  if (day) return { tokens: Number(day.tokens) || 0, usd: Number(day.equivalentUsd) || 0 };
  if (isSingleDayKey(rangeKey)) {
    return { tokens: Number(agg.totalTokens) || 0, usd: Number(agg.totalEquivalentUsd) || 0 };
  }
  return { tokens: 0, usd: 0 };
}

/** 当天各模型用量：优先 daily[].models，单日键则退回合计 models。 */
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
  if (isSingleDayKey(rangeKey) && Array.isArray(agg.models)) {
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
