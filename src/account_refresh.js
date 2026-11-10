// 主窗口的唯一后台刷新调度器。账户任务与共享用量任务分别计时，使用一个唤醒定时器。
// 注入时钟与刷新入口，便于用假时钟验证到期、重入及配置热更新，不访问真实账户。
export function createAccountRefreshScheduler({
  refreshAccount, refreshShared, getOrder, isBlocked = () => false, isRefreshing = () => false,
  onRunningChange = () => {}, onError = () => {}, now = () => Date.now(),
  setTimer = setTimeout, clearTimer = clearTimeout,
}) {
  const entries = new Map();
  let shared = null;
  let timer = null;
  let running = false;
  let initialized = false;
  let disposed = false;

  const minutesOf = (value) => Number.isInteger(value) && value > 0 ? value : 0;
  const nextAt = (minutes, startup = false) => minutes > 0 ? now() + (startup ? 3000 : minutes * 60_000) : Infinity;

  function arm() {
    if (timer != null) clearTimer(timer);
    timer = null;
    if (disposed || running) return;
    const next = Math.min(shared?.at ?? Infinity, ...[...entries.values()].map(entry => entry.at));
    if (!Number.isFinite(next)) return;
    // 已到期但被切换、导入或其他窗口占用的任务稍后重试，不以零延迟循环占用线程。
    timer = setTimer(run, Math.min(2_147_483_647, Math.max(1000, next - now())));
  }

  async function run() {
    timer = null;
    if (disposed || running) return;
    if (isBlocked()) { arm(); return; }
    const ids = getOrder().filter(id => entries.get(id)?.at <= now() && !isRefreshing(id));
    const sharedEntry = shared?.at <= now() ? shared : null;
    if (!ids.length && !sharedEntry) { arm(); return; }
    running = true;
    onRunningChange(true);
    const sharedJob = sharedEntry ? (async () => {
      try { await refreshShared(); }
      catch (error) { onError(error); }
      finally { if (shared === sharedEntry) shared.at = nextAt(shared.minutes); }
    })() : Promise.resolve();
    try {
      for (const id of ids) {
        const entry = entries.get(id);
        // 排队期间可能被删除、关闭、改期或由其他窗口提前刷新。
        if (disposed || !entry || entry.at > now() || isRefreshing(id)) continue;
        try { await refreshAccount(id); }
        catch (error) { onError(error); }
        finally { if (entries.get(id) === entry) entry.at = nextAt(entry.minutes); }
      }
      await sharedJob;
    } finally {
      running = false;
      onRunningChange(false);
      arm();
    }
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
        if (!previous || previous.minutes !== minutes || previous.override !== override) {
          entries.set(account.id, { minutes, override, at: nextAt(minutes, startup) });
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
