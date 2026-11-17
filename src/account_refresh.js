// 批量任务按供应商并行、组内顺序执行；跨窗口互斥由后端供应商队列保证。
export async function refreshByProvider(accounts, refreshAccount, onProgress = () => {}, onProviderChange = () => {}) {
  const groups = new Map();
  for (const account of accounts) {
    if (!groups.has(account.kind)) groups.set(account.kind, []);
    groups.get(account.kind).push(account.id);
  }
  let completed = 0;
  const failures = [];
  await Promise.all([...groups].map(async ([kind, ids]) => {
    onProviderChange(kind, true);
    try {
      for (const id of ids) {
        try { await refreshAccount(id); }
        catch (error) { failures.push({ id, error }); }
        finally { onProgress(++completed, accounts.length); }
      }
    } finally { onProviderChange(kind, false); }
  }));
  return failures;
}

// 主窗口唯一的唤醒定时器。供应商与共享用量任务独立运行、计时，慢队列不阻塞其它队列。
// 注入时钟与刷新入口，便于用假时钟验证到期、重入及配置热更新，不访问真实账户。
export function createAccountRefreshScheduler({
  refreshAccount, refreshShared, getOrder, isBlocked = () => false, isProviderBlocked = () => false, isRefreshing = () => false,
  onRunningChange = () => {}, onError = () => {}, now = () => Date.now(),
  setTimer = setTimeout, clearTimer = clearTimeout,
}) {
  const entries = new Map();
  const running = new Set();
  let shared = null;
  let timer = null;
  let sharedRunning = false;
  let initialized = false;
  let disposed = false;

  const minutesOf = (value) => Number.isInteger(value) && value > 0 ? value : 0;
  const nextAt = (minutes, startup = false) => minutes > 0 ? now() + (startup ? 3000 : minutes * 60_000) : Infinity;
  const notify = () => { if (!disposed) onRunningChange([...running], sharedRunning); };

  function arm() {
    if (timer != null) clearTimer(timer);
    timer = null;
    if (disposed) return;
    const next = Math.min(sharedRunning ? Infinity : shared?.at ?? Infinity,
      ...[...entries.values()].filter(entry => !running.has(entry.kind)).map(entry => entry.at));
    if (!Number.isFinite(next)) return;
    // 已到期但被切换、导入或其他窗口占用的任务稍后重试，不以零延迟循环占用线程。
    timer = setTimer(run, Math.min(2_147_483_647, Math.max(1000, next - now())));
  }

  async function runProvider(kind, ids) {
    running.add(kind);
    notify();
    try {
      for (const id of ids) {
        const entry = entries.get(id);
        // 排队期间可能被删除、关闭、改期或由其他窗口提前刷新。
        if (disposed || isBlocked() || isProviderBlocked(kind)) break;
        if (!entry || entry.kind !== kind || entry.at > now() || isRefreshing(id)) continue;
        try { await refreshAccount(id); }
        catch (error) { onError(error); }
        finally { if (entries.get(id) === entry) entry.at = nextAt(entry.minutes); }
      }
    } finally {
      running.delete(kind);
      notify();
      arm();
    }
  }

  async function runShared(entry) {
    sharedRunning = true;
    notify();
    try { await refreshShared(); }
    catch (error) { onError(error); }
    finally {
      if (shared === entry) shared.at = nextAt(shared.minutes);
      sharedRunning = false;
      notify();
      arm();
    }
  }

  function run() {
    timer = null;
    if (disposed) return;
    if (isBlocked()) { arm(); return; }
    const groups = new Map();
    for (const id of getOrder()) {
      const entry = entries.get(id);
      if (!entry || entry.at > now() || running.has(entry.kind) || isProviderBlocked(entry.kind) || isRefreshing(id)) continue;
      if (!groups.has(entry.kind)) groups.set(entry.kind, []);
      groups.get(entry.kind).push(id);
    }
    const jobs = [];
    if (!sharedRunning && shared?.at <= now()) jobs.push(runShared(shared));
    for (const [kind, ids] of groups) jobs.push(runProvider(kind, ids));
    // 不等本批全部完成才重新唤醒；空闲供应商的新到期任务可以独立开始。
    arm();
    return Promise.allSettled(jobs);
  }

  return {
    update(accounts, globalMinutes) {
      if (disposed) return;
      const global = minutesOf(globalMinutes);
      const startup = !initialized;
      initialized = true;
      const ids = new Set(accounts.map(account => account.id));
      for (const id of entries.keys()) if (!ids.has(id)) entries.delete(id);
      for (const account of accounts) {
        const override = account.refreshIntervalMinutes ?? null;
        const minutes = override == null ? global : minutesOf(override);
        const previous = entries.get(account.id);
        if (!previous || previous.minutes !== minutes || previous.override !== override || previous.kind !== account.kind) {
          entries.set(account.id, { kind: account.kind, minutes, override, at: nextAt(minutes, startup) });
        }
      }
      if (!shared || shared.minutes !== global) shared = { minutes: global, at: nextAt(global, startup) };
      arm();
    },
    completed(id) {
      const entry = entries.get(id);
      if (entry) entry.at = nextAt(entry.minutes);
      arm();
    },
    dispose() {
      disposed = true;
      if (timer != null) clearTimer(timer);
      timer = null;
      entries.clear();
      shared = null;
    },
  };
}
