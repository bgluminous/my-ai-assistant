import { errorCode, translate } from "./i18n.js";
import { resetError } from "./shared.js";

const errors = {
  sync_invalid_url: "syncErrors.enterAnHttpsServerUrlWithoutCredentials",
  sync_key_required: "syncErrors.enterASyncKeyOrClearThe",
  sync_invalid_key: "syncErrors.theSyncKeyMustContainVisibleAscii",
  sync_invalid_device: "syncErrors.useADeviceNameOfUpTo",
  sync_auth: "syncErrors.theSyncKeyIsInvalidOrLacks",
  sync_unauthorized: "syncErrors.theSyncKeyIsInvalidOrWas",
  sync_forbidden: "syncErrors.syncAccountDisabledAskTheAdministratorTo",
  sync_network: "syncErrors.syncServerUnreachableOrRequestTimedOut",
  sync_server: "syncErrors.syncServerTemporarilyUnavailableALaterRefresh",
  sync_rate_limited: "syncErrors.syncRequestsAreRateLimitedALater",
  sync_protocol_version: "syncErrors.incompatibleServerProtocolVersion1IsRequired",
  sync_protocol_invalid: "syncErrors.theServerResponseViolatesTheSyncProtocol",
  sync_response_too_large: "syncErrors.serverDataExceedsTheProtocolLimitsAutomatic",
  sync_invalid_record: "syncErrors.invalidUsageRecordThisPageWasNot",
  sync_reset_required: "syncErrors.theServerEpochOrCursorChangedSync",
  sync_not_configured: "syncErrors.syncServerAndKeyAreNotConfigured",
  sync_cancelled: "syncErrors.settingsOrCacheChangedThisSyncWas",
  sync_task_failed: "syncErrors.syncInterruptedRefreshToTryAgain",
};
export function syncError(error) {
  const code = errorCode(error);
  if (code.startsWith("sync_store_failed")) return translate("syncErrors.couldNotAccessTheUsageArchiveCheck");
  return (errors[code] && translate(errors[code])) || resetError(error);
}
