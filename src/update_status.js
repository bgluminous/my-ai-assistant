// 纯展示逻辑：失败与「已是最新」是不同状态，不清掉上次已知的新版本。
export function updatePresentation(view) {
  const phase = view.phase || "idle";
  const busy = ["checking", "downloading", "waiting", "installing"].includes(phase);
  let text = {
    idle: "尚未检查更新", checking: "正在检查更新…", upToDate: "当前已是最新版本",
    available: `发现新版本 ${view.latestVersion || ""}`,
    downloading: "正在下载更新…", waiting: "等待正在进行的业务操作完成…",
    installing: "正在安装更新，完成后将自动重启…", cancelled: "已取消下载，当前版本保持不变。",
    error: "检查或更新失败，请重试。",
  }[phase] || "尚未检查更新";
  if (view.error?.message) text = view.error.message;
  const total = Number(view.totalBytes);
  const downloaded = Math.max(0, Number(view.downloadedBytes) || 0);
  const fraction = total > 0 ? Math.min(1, downloaded / total) : null;
  const percent = fraction === null ? null : Math.floor(fraction * 100);
  return { text, busy, percent, fraction, downloaded, failed: phase === "error",
    cancelable: phase === "downloading" || phase === "waiting",
    paused: phase === "waiting" || phase === "installing" };
}

export function pricingError(error) {
  const code = typeof error === "string" ? error : error?.message || "";
  if (code.includes("timeout")) return "连接价格表超时，请检查网络或代理后重试。";
  if (code === "http_status_404") return "价格表暂未发布或地址不存在，请检查来源或打开公共仓库。";
  if (code.startsWith("http_status_")) return "价格表服务暂时不可用，请稍后重试。";
  if (code.startsWith("parse_failed") || code.startsWith("invalid_price") || code === "remote_table_empty") {
    return "价格表内容校验失败，已保留本地缓存和自定义价格。";
  }
  if (code === "invalid_update_url") return "价格表地址无效，请填写 HTTP 或 HTTPS 地址。";
  if (code === "update_in_progress") return "应用正在准备安装更新，请稍后操作。";
  if (code === "update_required") return "请先在「关于」页完成应用更新。";
  return "无法获取价格表，请检查网络或代理后重试；本地缓存和自定义价格仍可使用。";
}
