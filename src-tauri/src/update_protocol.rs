//! 公共版本索引与 Tauri 清单的边界；不接受前端提供的下载地址。
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RELEASES: &str = "https://github.com/bgluminous/my-ai-assistant/releases";
pub const REGISTRY: &str = "https://raw.githubusercontent.com/bgluminous/open-source-registry/main";
pub const PRICING_PAGE: &str = "https://github.com/bgluminous/open-source-registry/blob/main/resources/my-ai-assistant/pricing.json";
pub const START_DELAY_SECS: u64 = 30;
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub project_code: String,
    pub platform_code: String,
    pub version: String,
    pub version_code: u32,
    pub changelog: Option<String>,
    pub download_url: Option<String>,
    pub extra: Option<Value>,
    pub force_update: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryVersion {
    pub version: String,
    pub version_code: u32,
}

pub fn validate_history(mut versions: Vec<HistoryVersion>) -> Result<Vec<HistoryVersion>, &'static str> {
    if versions.iter().any(|v| version_code(&v.version) != Some(v.version_code)) {
        return Err("index_version");
    }
    versions.sort_by_key(|v| std::cmp::Reverse(v.version_code));
    if versions.windows(2).any(|pair| pair[0].version_code == pair[1].version_code) {
        return Err("index_version");
    }
    Ok(versions)
}

pub fn version_code(version: &str) -> Option<u32> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3 { return None; }
    let mut nums = [0u32; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || (part.len() > 1 && part.starts_with('0')) || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        nums[i] = part.parse().ok()?;
        if nums[i] > 99 { return None; }
    }
    Some(10_000_000 + nums[0] * 131_072 + nums[1] * 1_024 + nums[2] * 8 + 1)
}

pub fn release_page(version: Option<&str>) -> String {
    version.filter(|v| version_code(v).is_some())
        .map(|v| format!("{RELEASES}/tag/v{v}"))
        .unwrap_or_else(|| RELEASES.into())
}
pub fn asset_url(version: &str, name: &str) -> String {
    format!("{RELEASES}/download/v{version}/{name}")
}
pub fn manifest_url(version: &str, platform: &str) -> String {
    asset_url(version, &format!("updater-{platform}.json"))
}
impl Release {
    pub fn validate(&self, platform: &str) -> Result<(), &'static str> {
        if self.project_code != "my-ai-assistant" || self.platform_code != platform {
            return Err("index_identity");
        }
        if version_code(&self.version) != Some(self.version_code) { return Err("index_version"); }
        if let Some(url) = &self.download_url {
            let name = match platform {
                "windows-x64" => format!("my-ai-assistant_{}_x64-setup.exe", self.version),
                "macos-arm64" => format!("my-ai-assistant_{}_aarch64.dmg", self.version),
                _ => return Err("unsupported_platform"),
            };
            if *url != asset_url(&self.version, &name) { return Err("index_url"); }
        }
        if let Some(value) = self.extra.as_ref().and_then(|v| v.get("updater")) {
            if value.get("manifestUrl").and_then(Value::as_str) != Some(manifest_url(&self.version, platform).as_str()) {
                return Err("manifest_url");
            }
        }
        Ok(())
    }
    pub fn has_manifest(&self) -> bool {
        self.extra.as_ref().and_then(|v| v.pointer("/updater/manifestUrl")).and_then(Value::as_str).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_rejects_duplicates_and_sorts_valid_versions_numerically() {
        let entry = |version: &str| HistoryVersion {
            version: version.into(), version_code: version_code(version).unwrap(),
        };
        let versions = validate_history(vec![entry("1.9.0"), entry("1.10.0"), entry("1.3.1")]).unwrap();
        assert_eq!(versions.iter().map(|v| v.version.as_str()).collect::<Vec<_>>(), ["1.10.0", "1.9.0", "1.3.1"]);
        assert!(validate_history(Vec::new()).unwrap().is_empty());
        assert!(validate_history(vec![entry("1.3.1"), entry("1.3.1")]).is_err());
        assert!(validate_history(vec![HistoryVersion { version: "../../latest".into(), version_code: 1 }]).is_err());
        let mut invalid = entry("1.3.1");
        invalid.version_code += 1;
        assert!(validate_history(vec![invalid]).is_err());
    }
    fn fixture() -> Release {
        serde_json::from_value(serde_json::json!({
            "projectCode":"my-ai-assistant", "platformCode":"windows-x64",
            "version":"1.3.0", "versionCode":version_code("1.3.0"),
            "changelog":"更新说明", "downloadUrl":null, "extra":null, "forceUpdate":false
        })).unwrap()
    }
    #[test]
    fn canonical_versions_and_numeric_order() {
        assert_eq!(version_code("1.3.0"), Some(10_134_145));
        for invalid in ["01.3.0", "1.3", "1.3.0-beta", "v1.3.0", "1.3.100", "-1.3.0", "1.3.0 ", "1.3.0/path"] {
            assert!(version_code(invalid).is_none(), "{invalid}");
        }
        assert!(version_code("1.10.0") > version_code("1.9.99"));
        assert_eq!(START_DELAY_SECS, 30);
        assert_eq!(CHECK_INTERVAL_SECS, 86_400);
    }
    #[test]
    fn legacy_index_and_identity_validation() {
        let mut r = fixture();
        assert!(r.validate("windows-x64").is_ok());
        assert!(!r.has_manifest());
        assert!(r.validate("macos-arm64").is_err());
        r.version_code += 1;
        assert_eq!(r.validate("windows-x64"), Err("index_version"));
    }
    #[test]
    fn urls_are_bound_to_project_platform_and_version() {
        let mut r = fixture();
        for url in ["http://github.com/fake", "https://github.com/another/repo/releases/latest", "javascript:alert(1)", "https://github.com/bgluminous/my-ai-assistant/releases/download/v1.2.0/updater-windows-x64.json"] {
            r.extra = Some(serde_json::json!({"updater":{"manifestUrl":url}}));
            assert_eq!(r.validate("windows-x64"), Err("manifest_url"));
        }
        r.extra = Some(serde_json::json!({"updater":{"manifestUrl":manifest_url("1.3.0", "windows-x64")}}));
        assert!(r.validate("windows-x64").is_ok());
        assert_eq!(release_page(Some("oops")), RELEASES);
        assert_eq!(release_page(Some("1.3.0")), format!("{RELEASES}/tag/v1.3.0"));
    }
}
