use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::settings;

/// 代理配置。mode 取值："system"（跟随系统/环境变量）| "direct"（直连不使用代理）| "custom"（自定义地址）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub mode: String,
    #[serde(default)]
    pub url: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        ProxyConfig {
            mode: "system".to_string(),
            url: String::new(),
        }
    }
}

/// 当前生效的代理配置（HTTP 客户端每次构建时读取）。
pub fn current() -> ProxyConfig {
    settings::read(|s| sanitize(s.proxy.clone())).unwrap_or_default()
}

pub(crate) fn sanitize(cfg: ProxyConfig) -> ProxyConfig {
    let mode = match cfg.mode.trim() {
        "custom" => "custom",
        "direct" => "direct",
        _ => "system",
    };
    ProxyConfig {
        mode: mode.to_string(),
        url: cfg.url.trim().to_string(),
    }
}

fn validate(cfg: &ProxyConfig) -> Result<(), String> {
    if cfg.mode == "custom" {
        if cfg.url.is_empty() {
            return Err("empty_proxy_url".into());
        }
        reqwest::Proxy::all(&cfg.url).map_err(|_| "invalid_proxy_url".to_string())?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyView {
    pub mode: String,
    pub url: String,
    pub path: String,
}

fn view(cfg: &ProxyConfig) -> ProxyView {
    ProxyView {
        mode: cfg.mode.clone(),
        url: cfg.url.clone(),
        path: settings::path_display(),
    }
}

#[tauri::command]
pub fn proxy_get() -> Result<ProxyView, String> {
    settings::ensure_loaded()?;
    Ok(view(&current()))
}

#[tauri::command]
pub fn proxy_set(config: ProxyConfig) -> Result<ProxyView, String> {
    let cfg = sanitize(config);
    validate(&cfg)?;
    settings::mutate(|s| {
        s.proxy = cfg.clone();
        Ok(())
    })?;
    Ok(view(&cfg))
}

/// 代理测试的出口探测地址：返回 `{"code":0,"data":{"ip","country","province","city","isp",...}}`。
const PROXY_TEST_URL: &str = "https://inf.xil.to/ip";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub ok: bool,
    pub ms: u64,
    pub ip: Option<String>,
    /// 出口 IP 的地区（国家 / 省 / 市，空段与重复段已去掉）。
    pub region: Option<String>,
    pub isp: Option<String>,
    pub status: Option<u16>,
    pub error: Option<String>,
}

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 用给定（可能尚未保存的）配置试连一次，返回出口 IP、地区、ISP 与耗时，便于用户确认代理可用。
#[tauri::command]
pub async fn proxy_test(config: ProxyConfig) -> Result<ProxyTestResult, String> {
    let cfg = sanitize(config);
    validate(&cfg)?;
    let client = crate::http::build_client(&cfg);
    let started = Instant::now();
    let resp = client
        .get(PROXY_TEST_URL)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    let ms = started.elapsed().as_millis() as u64;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let data = r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("data").cloned());
            let (ip, region, isp) = match &data {
                Some(d) => {
                    let mut parts: Vec<String> = Vec::new();
                    for key in ["country", "province", "city"] {
                        if let Some(s) = json_str(d, key) {
                            if !parts.contains(&s) {
                                parts.push(s);
                            }
                        }
                    }
                    let region = (!parts.is_empty()).then(|| parts.join(" "));
                    (json_str(d, "ip"), region, json_str(d, "isp"))
                }
                None => (None, None, None),
            };
            Ok(ProxyTestResult {
                ok: (200..300).contains(&status),
                ms,
                ip,
                region,
                isp,
                status: Some(status),
                error: None,
            })
        }
        Err(e) => Ok(ProxyTestResult {
            ok: false,
            ms,
            ip: None,
            region: None,
            isp: None,
            status: None,
            error: Some(e.to_string()),
        }),
    }
}
