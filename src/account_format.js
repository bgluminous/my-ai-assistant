// 账户数据的纯格式化助手：套餐名 / 月费、Token 打码、相对时间、到期倒计时、
// 超额与 Credits 文案。无状态、不触碰 DOM，主窗口账户页、用量页、快照与托盘面板共用；
// 托盘面板因此无需引入整个账户管理模块。

const MEMBERSHIP_LABELS = {
  free: "Free",
  pro: "Pro",
  pro_plus: "Pro+",
  ultra: "Ultra",
  enterprise: "Enterprise",
  business: "Business",
  team: "Team",
};

// Codex 套餐名映射（chatgpt_plan_type -> 展示名），未知值原样显示
const CODEX_PLAN_LABELS = {
  plus: "Plus",
  pro: "Pro",
  team: "Team",
  enterprise: "Enterprise",
  business: "Business",
  free: "Free",
  edu: "Edu",
};

// Claude 订阅类型映射（subscription_type -> 展示名），未知值原样显示
const CLAUDE_PLAN_LABELS = {
  free: "Free",
  pro: "Pro",
  max: "Max",
  team: "Team",
  enterprise: "Enterprise",
};

// Cursor 套餐月费（美元 / 月），用量统计页用于对比等价 API 费用；未收录的套餐（如企业定制）视为未知
const CURSOR_PLAN_USD = {
  free: 0,
  pro: 20,
  pro_plus: 60,
  ultra: 200,
  business: 40,
  team: 40,
};

/** Cursor 套餐月费（USD/月）；未知套餐返回 null。 */
export function planMonthlyUsd(membershipType) {
  const key = String(membershipType ?? "").trim().toLowerCase();
  return Object.prototype.hasOwnProperty.call(CURSOR_PLAN_USD, key) ? CURSOR_PLAN_USD[key] : null;
}

export function membershipLabel(value) {
  const key = String(value ?? "").toLowerCase();
  if (!key) return "";
  return MEMBERSHIP_LABELS[key] || String(value);
}

export function codexPlanLabel(value) {
  const key = String(value ?? "").trim().toLowerCase();
  if (!key) return "";
  return CODEX_PLAN_LABELS[key] || String(value);
}

export function claudePlanLabel(value) {
  const key = String(value ?? "").trim().toLowerCase();
  if (!key) return "";
  return CLAUDE_PLAN_LABELS[key] || String(value);
}

/**
 * Cursor 账户的展示身份：primary = 手填备注 > 刷新返回的用户名 > 刷新返回的邮箱 > 自动备注；
 * email = 刷新返回的邮箱，与主显示相同（备注为空时自动落到邮箱）则为空串，避免同一身份显示两遍。
 * 用量页筛选 chip 与托盘账户行共用，主显示旁 / 下方以小字补充邮箱。
 */
export function cursorIdentity(account) {
  const note = String((account && account.note) || "").trim();
  const status = (account && account.status) || null;
  const name = String((status && status.name) || "").trim();
  const email = String((status && status.email) || "").trim();
  const primary = (account && account.noteAuto === false ? note : "") || name || email || note || "未命名账户";
  return { primary, email: email && email !== primary ? email : "" };
}

/**
 * 账户排序键：付费套餐按到期时间升序（临期 / 已到期靠前），没有到期信息的付费套餐居中，
 * 尚无状态的其次，Free 垫底；同一档保持原有顺序（sort 稳定）。
 */
function planSortKey(account) {
  const status = account.status || null;
  const rawPlan = account.kind === "cursor" ? status && status.membershipType : status && status.plan;
  const plan = String(rawPlan ?? "").trim().toLowerCase();
  if (plan === "free") return { rank: 3, end: 0 };
  // Claude 接口不提供订阅起止；Cursor 取本期计费周期截止，Codex 取订阅到期
  const endIso = !status || account.kind === "claude"
    ? null
    : account.kind === "codex" ? status.planActiveUntil : status.billingCycleEnd;
  const end = endIso ? Date.parse(endIso) : NaN;
  if (Number.isFinite(end)) return { rank: 0, end };
  return { rank: plan ? 1 : 2, end: 0 };
}

/** 同一类型账户的组内排序比较器，主窗口账户页与托盘面板共用，保证两处顺序一致。 */
export function compareAccounts(a, b) {
  const ka = planSortKey(a);
  const kb = planSortKey(b);
  return ka.rank - kb.rank || ka.end - kb.end || 0;
}

/** 保留两端、中间省略的打码 token（列表与托盘展示用）。 */
export function maskToken(token) {
  const t = String(token || "").trim();
  if (!t) return "—";
  if (t.length > 20) return `${t.slice(0, 12)}…${t.slice(-4)}`;
  return `${t.slice(0, 4)}…`;
}

export function relativeFromMs(ms) {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return new Date(ms).toLocaleDateString("zh-CN");
}

/** 账户上次刷新时间的相对文案（如 5 分钟前）。 */
export function relativeFromUnixSeconds(seconds) {
  const n = Number(seconds);
  if (seconds == null || !Number.isFinite(n) || n <= 0) return "—";
  return relativeFromMs(n * 1000);
}

/** 到期剩余时长的紧凑文本（Nd / Nh / <1h）；已过期返回 expired=true。 */
export function remainInfo(endMs) {
  const remainMs = endMs - Date.now();
  if (remainMs <= 0) return { text: "已到期", expired: true };
  const days = Math.floor(remainMs / 86_400_000);
  const hours = Math.floor(remainMs / 3_600_000);
  return { text: days >= 1 ? `${days}d` : hours >= 1 ? `${hours}h` : "<1h", expired: false };
}

/** 美元金额文本（千分位 + 两位小数），无效值返回 —。 */
export function usdText(usd) {
  const n = Number(usd);
  if (!Number.isFinite(n)) return "—";
  return `$${n.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** 美分 -> 美元文本（千分位 + 两位小数），无效值返回 —。 */
export function centsText(cents) {
  const n = Number(cents);
  if (!Number.isFinite(n)) return "—";
  return usdText(n / 100);
}

/**
 * ChatGPT extra usage：wham/usage 的 credits.balance 是 credit 点数（可带小数），
 * 不是美元。官网余额 = floor(点数) × $0.04（本机实测 2838.51523 → $113.52）。
 */
const CODEX_CREDIT_USD = 0.04;

function creditsNumber(value) {
  if (typeof value === "number" || typeof value === "string") {
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

/**
 * 从 credits 取出点数与美元余额。结构为
 * `{ has_credits, unlimited, balance }`；兼容 available / remaining 别名。
 */
export function parseCreditsUsd(credits) {
  if (credits == null) return { unlimited: false, units: null, usd: null };
  let raw = creditsNumber(credits);
  let unlimited = false;
  if (raw == null && typeof credits === "object") {
    unlimited = credits.unlimited === true;
    for (const key of ["balance", "available", "remaining"]) {
      raw = creditsNumber(credits[key]);
      if (raw != null) break;
    }
  }
  if (raw == null) return { unlimited, units: null, usd: null };
  const units = Math.floor(raw);
  return { unlimited, units, usd: units * CODEX_CREDIT_USD };
}

/**
 * 托盘第二行：Cursor 超额紧凑文案。未开启或账户无效返回 null。
 * 有上限时 title 为「上限 $x.xx」，供悬停展示。
 */
export function onDemandBrief(status) {
  const onDemand = status && status.alive !== false ? status.onDemand || null : null;
  if (!onDemand || !onDemand.enabled) return null;
  const limitCents = Number(onDemand.limit);
  return {
    text: `超额 ${centsText(onDemand.used)}`,
    title: Number.isFinite(limitCents) && limitCents > 0 ? `上限 ${centsText(limitCents)}` : "",
  };
}

/**
 * 托盘第二行：Codex Credits 余额紧凑文案。解析不出返回 null。
 * 无限显示「余额 无限」；点数放进 title。
 */
export function creditsBrief(status) {
  const credits = status && status.alive !== false ? status.credits : null;
  if (credits == null) return null;
  const { unlimited, units, usd } = parseCreditsUsd(credits);
  if (!unlimited && usd == null) return null;
  return {
    text: unlimited ? "余额 无限" : `余额 ${usdText(usd)}`,
    title: units != null ? `${units.toLocaleString("en-US")} 点` : "",
  };
}

/**
 * ChatGPT 剩余额度重置次数的紧凑文案（账户页摘要小字与托盘副行共用）。
 * 接口未提供（套餐不含该权益或字段缺失）或账户无效时返回 null；为 0 也显示，提示已用完。
 * 返回：
 * - text：「剩余重置 N 次」；
 * - expiresText：最早一次过期时间的短文案（「最早 MM-DD 过期」，已过期则「最早 MM-DD 已过期」），
 *   次数为 0 或没有过期数据时为空串；expired 标记最早一次是否已过期（数据陈旧，应刷新）；
 * - title：悬停说明，含完整过期日期时间。
 */
export function resetCreditsBrief(status) {
  if (!status || status.alive === false) return null;
  const n = Number(status.resetCreditsAvailable);
  if (status.resetCreditsAvailable == null || !Number.isFinite(n)) return null;
  const titleLines = ["ChatGPT 额度重置：消耗一次可立即重置 5 小时与每周额度窗口"];
  let expiresText = "";
  let expired = false;
  const expiresMs = n > 0 && status.resetCreditsExpiresAt ? Date.parse(status.resetCreditsExpiresAt) : NaN;
  if (Number.isFinite(expiresMs)) {
    const d = new Date(expiresMs);
    const mmdd = `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
    expired = expiresMs <= Date.now();
    expiresText = expired ? `最早 ${mmdd} 已过期` : `最早 ${mmdd} 过期`;
    const full = d.toLocaleString("zh-CN", { hour12: false, month: "long", day: "numeric", hour: "2-digit", minute: "2-digit" });
    titleLines.push(expired ? `最早一次已于 ${full} 过期，请刷新查看最新次数` : `最早一次将于 ${full} 过期`);
  }
  return {
    count: n,
    text: `剩余重置 ${n} 次`,
    expiresText,
    expired,
    title: titleLines.join("\n"),
  };
}
