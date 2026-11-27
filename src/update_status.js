import { translate } from "./i18n.js";
const updateErrors = {
  "timeout": "update.error.timeout",
  "network": "update.error.network",
  "not_published": "update.error.notPublished",
  "http": "update.error.http",
  "manifest_missing": "update.error.manifestMissing",
  "stale_index": "update.error.staleIndex",
  "invalid_json": "update.error.invalidJson",
  "index_identity": "update.error.invalidJson",
  "index_version": "update.error.invalidJson",
  "index_url": "update.error.invalidJson",
  "manifest_url": "update.error.invalidJson",
  "manifest_invalid": "update.error.invalidJson",
  "signature": "update.error.signature",
  "download": "update.error.download",
  "install": "update.error.install",
  "update_busy": "update.error.updateBusy",
  "manual_only": "update.error.manualOnly",
  "no_update": "update.error.noUpdate",
  "unsupported_platform": "update.error.unsupportedPlatform",
  "cancelled": "update.error.cancelled",
  "open_failed": "update.error.openFailed",
  "invalid_link": "update.error.invalidLink",
  "settings": "update.error.settings"
};

export function updateErrorText(error) {
  if (!error?.message) return "";
  if (error.code === "install_restore" && error.recoveryPath) {
    return translate("native.installationFailedThePreviousApplicationIsAt", error.recoveryPath);
  }
  return Object.hasOwn(updateErrors, error.code) ? translate(updateErrors[error.code]) : error.message;
}

// 纯展示逻辑：失败与「已是最新」是不同状态，不清掉上次已知的新版本。
export function updatePresentation(view) {
  const phase = view.phase || "idle";
  const busy = ["checking", "downloading", "waiting", "installing"].includes(phase);
  let text = {
    idle: translate("updater.notCheckedYet"), checking: translate("updateStatus.checkingForUpdates"), upToDate: translate("updateStatus.youReUpToDate"),
    available: translate("updateStatus.versionAvailable", view.latestVersion || ""),
    downloading: translate("updateStatus.downloadingUpdate"), waiting: translate("updateStatus.waitingForCurrentOperationsToFinish"),
    installing: translate("updateStatus.installingUpdateTheAppWillRestartAutomatically"), cancelled: translate("update.error.cancelled"),
    error: translate("updateStatus.updateCheckOrInstallationFailedTryAgain"),
  }[phase] || translate("updater.notCheckedYet");
  if (view.error?.message) text = updateErrorText(view.error);
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
  if (code.includes("timeout")) return translate("updateStatus.pricingRequestTimedOutCheckYourNetwork");
  if (code === "http_status_404") return translate("updateStatus.pricingIsUnpublishedOrTheUrlIs");
  if (code.startsWith("http_status_")) return translate("updateStatus.pricingServiceTemporarilyUnavailableTryAgainLater");
  if (code.startsWith("parse_failed") || code.startsWith("invalid_price") || code === "remote_table_empty") {
    return translate("updateStatus.pricingFailedValidationLocalCacheAndCustom");
  }
  if (code === "invalid_update_url") return translate("updateStatus.invalidPricingUrlEnterAnHttpOr");
  if (code === "update_in_progress") return translate("shared.anUpdateIsBeingPreparedTryAgain");
  if (code === "update_required") return translate("updateStatus.updateTheAppInAboutFirst");
  return translate("updateStatus.couldNotLoadPricesCheckYourNetwork");
}
