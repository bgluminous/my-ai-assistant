use super::*;

fn auth() -> Credential {
    Credential {
        server: SERVER.into(),
        workspace_id: "wrk_test".into(),
        workspace_name: "Fixture".into(),
        user_id: "user_test".into(),
        email: "fixture@example.invalid".into(),
        access: "fixture-access".into(),
        refresh: "fixture-refresh".into(),
        expires_at: Utc::now().timestamp_millis() + 3600_000,
    }
}

#[test]
fn credentials_are_encrypted_bound_to_official_workspace_and_portable() {
    let c = auth();
    let blob = c.seal().unwrap();
    assert!(!blob.contains("fixture-access"));
    assert_eq!(decode(&blob, "wrk_test").unwrap().refresh, c.refresh);
    assert!(decode(&blob, "wrk_other").is_none());
    let mut c = c;
    c.server = "https://example.invalid/console".into();
    assert!(c.seal().is_err());
    assert!(!valid_workspace("../escape"));
}
#[test]
fn refresh_response_requires_complete_tokens_and_has_bounded_expiry() {
    let mut c = auth();
    assert!(apply_token(&mut c, &json!({"access_token":"new","expires_in":3600})).is_err());
    assert_eq!(c.access, "fixture-access");
    apply_token(
        &mut c,
        &json!({"access_token":"new","refresh_token":"rotated","expires_in":3600}),
    )
    .unwrap();
    assert_eq!(c.refresh, "rotated");
    assert!(apply_token(
        &mut c,
        &json!({"access_token":"x","refresh_token":"y","expires_in":-1})
    )
    .is_err());
}
#[test]
fn all_plan_tiers_and_explicit_cancellation_replace_cached_subscription() {
    for plan in ["go", "go-plus", "go-teams"] {
        let mut s = json!({"plan":"old"});
        merge_subscription(
            &mut s,
            &json!({"hasGoSubscription":true}),
            &json!({
            "product":plan,"memberCount":3,"cancelAtPeriodEnd":true,"access":{"startsAt":"2026-11-01","endsAt":"2026-12-01"}}),
        );
        assert_eq!(s["plan"], plan);
        assert_eq!(s["memberCount"], 3);
        assert_eq!(s["planWillRenew"], false);
        merge_subscription(&mut s, &json!({"hasGoSubscription":true}), &Value::Null);
        assert_eq!(s["plan"], plan);
        merge_subscription(&mut s, &json!({"hasGoSubscription":false}), &Value::Null);
        assert_eq!(s["plan"], "payg");
        assert!(s["billingCycleEnd"].is_null());
    }
}
#[test]
fn quotas_keep_server_percentages_and_reset_units() {
    let meter = json!({"percent":35.5,"resetsAt":"2026-11-10T00:00:00Z","status":"ok"});
    let v =
        quota_windows(&json!({"usage":{"rolling":meter,"weekly":meter,"monthly":meter}})).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert_eq!(v[0]["usedPercent"], 35.5);
    assert_eq!(v[0]["resetAt"], 1794268800i64);
    assert!(quota_windows(&json!({"usage":{}})).is_err());
}
#[test]
fn amounts_parse_decimal_microcents_including_zero_and_negative_balance() {
    assert_eq!(dollars(&json!("100000000")), Some(1.0));
    assert_eq!(dollars(&json!("0")), Some(0.0));
    assert_eq!(dollars(&json!("-125000000")), Some(-1.25));
    assert_eq!(dollars(&json!(null)), None);
    assert_eq!(dollars(&json!("1.2")), None);
}
#[test]
fn cancelled_attempt_cannot_be_claimed_again() {
    let id = accounts::new_id();
    let cancelled = Arc::new(AtomicBool::new(false));
    pending().lock().unwrap().insert(
        id.clone(),
        Pending {
            device: "fixture".into(),
            deadline: Utc::now().timestamp_millis() + 60_000,
            next_poll: Utc::now().timestamp_millis() + 30_000,
            interval: 5,
            expected: None,
            note: None,
            cancelled: cancelled.clone(),
            polling: false,
            token: None,
        },
    );
    assert!(claim_poll(&id).unwrap().is_none());
    opencode_oauth_cancel(id.clone()).unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(claim_poll(&id).is_err());
}

// 仅测试构建替换传输；没有夹具时直接失败，测试不会访问真实授权接口。
pub(crate) type Transport =
    Arc<dyn Fn(&str, &str, &str) -> Result<(u16, Value), String> + Send + Sync>;
fn fixture_transport() -> &'static Mutex<Option<Transport>> {
    static TRANSPORT: OnceLock<Mutex<Option<Transport>>> = OnceLock::new();
    TRANSPORT.get_or_init(|| Mutex::new(None))
}
pub(crate) fn set_transport(value: Option<Transport>) {
    *fixture_transport().lock().unwrap() = value;
}
pub(crate) async fn transport(
    method: &str,
    path: &str,
    secret: &str,
) -> Result<(u16, Value), String> {
    let handler = fixture_transport()
        .lock()
        .unwrap()
        .clone()
        .ok_or("test_transport_missing")?;
    // 让并发刷新确实重叠，而不是顺序完成。
    tokio::task::yield_now().await;
    handler(method, path, secret)
}
#[test]
fn partial_failures_keep_last_known_values_and_report_each_permission_state() {
    let mut status = json!({"plan":"go-plus","balanceUsd":12.5,"windows":[{"usedPercent":50}]});
    let mut fields = json!({"balance":{"state":"ok","at":1000}});
    merge_field(
        &mut status,
        &mut fields,
        &json!({}),
        "balance",
        Err("opencode_http_403".into()),
    );
    assert_eq!(status["balanceUsd"], 12.5);
    assert_eq!(fields["balance"]["state"], "forbidden");
    assert_eq!(fields["balance"]["at"], 1000);
    merge_field(
        &mut status,
        &mut fields,
        &json!({}),
        "quota",
        Err("opencode_http_401".into()),
    );
    assert_eq!(fields["quota"]["state"], "unauthorized");
    assert_eq!(status["windows"][0]["usedPercent"], 50);
    merge_field(
        &mut status,
        &mut fields,
        &json!({}),
        "subscription",
        Err("opencode_network_error".into()),
    );
    assert_eq!(status["plan"], "go-plus");
    assert_eq!(fields["subscription"]["state"], "unavailable");
    merge_field(
        &mut status,
        &mut fields,
        &json!({"hasGoSubscription":true}),
        "subscription",
        Ok(Value::Null),
    );
    assert_eq!(fields["subscription"]["state"], "unavailable");
}
#[test]
fn expired_and_concurrent_polls_do_not_exchange_a_device_code_twice() {
    let id = accounts::new_id();
    let mut p = Pending {
        device: "fixture".into(),
        deadline: Utc::now().timestamp_millis() + 60_000,
        next_poll: 0,
        interval: 5,
        expected: None,
        note: None,
        cancelled: Arc::new(AtomicBool::new(false)),
        polling: false,
        token: None,
    };
    pending().lock().unwrap().insert(id.clone(), p.clone());
    assert!(claim_poll(&id).unwrap().is_some());
    assert!(claim_poll(&id).unwrap().is_none());
    opencode_oauth_cancel(id.clone()).unwrap();
    p.deadline = Utc::now().timestamp_millis() - 1;
    pending().lock().unwrap().insert(id.clone(), p);
    assert!(claim_poll(&id).is_err());
}

#[test]
fn member_visibility_cannot_be_mislabelled_as_workspace_total() {
    assert!(workspace_usage_allowed(&json!({"role":"owner"})));
    assert!(workspace_usage_allowed(&json!({"role":"admin"})));
    assert!(!workspace_usage_allowed(
        &json!({"role":"member","singleMember":true})
    ));
    assert!(!workspace_usage_allowed(&json!({})));
}

#[test]
fn authorization_links_resolve_relative_paths_and_preserve_device_query() {
    let path = "/console/device?user_code=TEST-CODE&client_id=opencode-cli";
    let full = format!("https://opencode.ai{path}");
    assert_eq!(authorization_url(path).unwrap(), full);
    assert_eq!(authorization_url(&full).unwrap(), full);
    assert_eq!(
        authorization_url("device?user_code=TEST-CODE").unwrap(),
        "https://opencode.ai/console/device?user_code=TEST-CODE"
    );
}

#[test]
fn authorization_links_reject_nonofficial_origins_and_embedded_credentials() {
    for bad in [
        "",
        " ",
        "https://example.invalid/device",
        "//example.invalid/device",
        "http://opencode.ai/console/device",
        "https://opencode.ai:444/console/device",
        "https://user@opencode.ai/console/device",
        "javascript:alert(1)",
    ] {
        assert!(
            authorization_url(bad).is_err(),
            "unexpected authorization URL: {bad}"
        );
    }
}
