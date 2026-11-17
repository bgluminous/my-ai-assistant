//! Each distinct local database is addressed explicitly by an owned native server.
//! This avoids `api` silently attaching to another installation's background service.
//! The server is retained during OAuth and stopped on completion/cancellation.
use crate::{accounts, local_client as common, opencode, opencode_local as local};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct NativeServer {
    child: Child,
    url: String,
    password: String,
    _dir: tempfile::TempDir,
    #[cfg(windows)]
    _job: OwnedJob,
}
impl Drop for NativeServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        self._job.close();
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .output();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && self.child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl NativeServer {
    fn request(
        &self,
        method: &str,
        path: &str,
        data: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "opencode_cli_failed")?;
        let method = reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
            .map_err(|_| "opencode_cli_failed")?;
        let mut request = client
            .request(method, format!("{}{path}", self.url))
            .basic_auth("opencode", Some(&self.password));
        if let Some(data) = data {
            request = request.json(&data);
        }
        let response = request.send().map_err(|_| "opencode_cli_failed")?;
        if !response.status().is_success() {
            return Err(format!(
                "opencode_client_http_{}",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        response
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "opencode_cli_failed")?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("opencode_cli_output_too_large".into());
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "opencode_cli_invalid_response")?;
        Ok(unwrap_data(value))
    }
    fn start(exe: &Path, db: &Path) -> Result<Self, String> {
        let port = TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "opencode_client_port_failed")?
            .local_addr()
            .map_err(|_| "opencode_client_port_failed")?
            .port();
        let dir = tempfile::tempdir().map_err(|_| "opencode_cli_failed")?;
        let mut random = [0u8; 32];
        getrandom::getrandom(&mut random).map_err(|_| "random_failed")?;
        let password: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let mut command = Command::new(exe);
        #[cfg(target_os = "macos")]
        {
            // Positional arguments keep paths out of shell source. The monitor owns
            // only this helper and stops it if the assistant is terminated abruptly.
            command = Command::new("/bin/sh");
            command.args(["-c", "parent=$1; shift; \"$@\" & child=$!; trap 'kill \"$child\" 2>/dev/null' EXIT TERM INT; while kill -0 \"$parent\" 2>/dev/null && kill -0 \"$child\" 2>/dev/null; do sleep 1; done; kill \"$child\" 2>/dev/null; wait \"$child\"", "opencode-helper", &std::process::id().to_string()]).arg(exe);
        }
        command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .env("OPENCODE_DB", db)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().map_err(|_| "opencode_cli_failed")?;
        #[cfg(windows)]
        let job = match OwnedJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut server = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
            password,
            _dir: dir,
            #[cfg(windows)]
            _job: job,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if server
                .child
                .try_wait()
                .map_err(|_| "opencode_cli_failed")?
                .is_some()
            {
                return Err("opencode_cli_failed".into());
            }
            if let Ok(info) = server.request("get", "/api/info", None, Duration::from_millis(500)) {
                if info["pid"].as_u64() == Some(u64::from(server.child.id()))
                    || cfg!(target_os = "macos")
                        && info["version"]
                            .as_str()
                            .is_some_and(|v| v.starts_with("2."))
                {
                    return Ok(server);
                }
                return Err("opencode_client_service_mismatch".into());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err("opencode_cli_timeout".into())
    }
}

// Kill-on-close also applies if the GUI crashes: Windows closes this handle.
#[cfg(windows)]
struct OwnedJob(isize);
#[cfg(windows)]
impl OwnedJob {
    fn close(&mut self) {
        #[link(name = "kernel32")]
        extern "system" {
            fn CloseHandle(handle: isize) -> i32;
        }
        if self.0 != 0 {
            unsafe {
                CloseHandle(self.0);
            }
            self.0 = 0;
        }
    }
    fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct Basic {
            process_time: i64,
            job_time: i64,
            flags: u32,
            min: usize,
            max: usize,
            count: u32,
            affinity: usize,
            priority: u32,
            scheduling: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Extended {
            basic: Basic,
            io: [u64; 6],
            process_memory: usize,
            job_memory: usize,
            peak_process: usize,
            peak_job: usize,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> isize;
            fn SetInformationJobObject(
                job: isize,
                class: i32,
                info: *const std::ffi::c_void,
                len: u32,
            ) -> i32;
            fn AssignProcessToJobObject(job: isize, process: *mut std::ffi::c_void) -> i32;
        }
        let job = Self(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
        if job.0 == 0 {
            return Err("opencode_helper_lifetime_failed".into());
        }
        let mut limits = Extended::default();
        limits.basic.flags = 0x2000;
        let configured = unsafe {
            SetInformationJobObject(
                job.0,
                9,
                &limits as *const _ as *const _,
                std::mem::size_of::<Extended>() as u32,
            )
        };
        if configured == 0 || unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) } == 0
        {
            return Err("opencode_helper_lifetime_failed".into());
        }
        Ok(job)
    }
}
#[cfg(windows)]
impl Drop for OwnedJob {
    fn drop(&mut self) {
        self.close();
    }
}
fn unwrap_data(value: Value) -> Value {
    value.get("data").cloned().unwrap_or(value)
}
struct Target {
    exe: PathBuf,
    db: PathBuf,
    labels: Vec<String>,
    server: Option<NativeServer>,
    before: Option<Attempt>,
}
impl Target {
    fn api(&mut self, method: &str, path: &str, data: Option<Value>) -> Result<Value, String> {
        #[cfg(test)]
        {
            let body = data.as_ref().map(Value::to_string);
            let mut args = vec!["api", method, path];
            if let Some(body) = body.as_deref() {
                args.extend(["--data", body]);
            }
            if let Some(result) = local::tests::run_fixture(&args) {
                let text = result?;
                return if text.trim().is_empty() {
                    Ok(Value::Null)
                } else {
                    serde_json::from_str(&text)
                        .map(unwrap_data)
                        .map_err(|_| "opencode_cli_invalid_response".into())
                };
            }
        }
        if self.server.is_none() {
            self.server = Some(NativeServer::start(&self.exe, &self.db)?);
        }
        self.server.as_ref().ok_or("opencode_cli_failed")?.request(
            method,
            path,
            data,
            Duration::from_secs(45),
        )
    }
    fn active(&self) -> Result<Option<String>, String> {
        use rusqlite::OptionalExtension;
        let db = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| "opencode_database_unavailable")?;
        db.query_row("SELECT id FROM credential WHERE integration_id='opencode' AND active=1 ORDER BY time_created DESC LIMIT 1",[],|r|r.get(0)).optional().map_err(|_|"opencode_database_unavailable".into())
    }
    fn credentials(&mut self) -> Result<Vec<local::LocalIdentity>, String> {
        let body = self.api("get", "/api/credential", None)?;
        Ok(body
            .as_array()
            .ok_or("opencode_cli_invalid_response")?
            .iter()
            .filter(|v| v["integrationID"] == "opencode")
            .filter_map(|v| local::identity(opencode::text(v, "id"), &v["value"]))
            .collect())
    }
    fn activate(&mut self, id: &str) -> Result<(), String> {
        if !opencode::valid_workspace(id) {
            return Err("opencode_cli_invalid_response".into());
        }
        self.api("post", &format!("/api/credential/{id}/activate"), None)?;
        if self.active()?.as_deref() != Some(id) {
            return Err("opencode_switch_not_confirmed".into());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Attempt {
    id: String,
    previous: Option<String>,
    known: Vec<String>,
    deadline: i64,
}
struct Job {
    account: String,
    workspace: String,
    targets: Vec<Target>,
    index: usize,
    clients: Vec<Value>,
    attempt: Option<Attempt>,
    desktop: Option<PathBuf>,
    was_running: bool,
    launch_finalized: bool,
    _operation: crate::operations::Operation<'static>,
}
fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn ensure_idle() -> Result<(), String> {
    if jobs().lock().map_err(|_| "state_lock_poisoned")?.is_empty() {
        Ok(())
    } else {
        Err("opencode_client_auth_pending".into())
    }
}

fn add_target(targets: &mut Vec<Target>, exe: PathBuf, db: PathBuf, label: &str) {
    if let Some(existing) = targets.iter_mut().find(|t| common::same_path(&t.db, &db)) {
        existing.labels.push(label.into());
    } else {
        targets.push(Target {
            exe,
            db,
            labels: vec![label.into()],
            server: None,
            before: None,
        });
    }
}
fn resolve_targets(selection: Option<common::SwitchTarget>) -> Result<(Vec<Target>, Option<PathBuf>), String> {
    let cfg = local::config();
    let mut targets = Vec::new();
    #[cfg(test)]
    if local::tests::run_fixture(&["--version"]).is_some() {
        common::selected_clients(selection,true,false)?;
        add_target(
            &mut targets,
            PathBuf::from("fixture-cli"),
            PathBuf::from(&cfg.db_path),
            "cli",
        );
        return Ok((targets, None));
    }
    let desktop = if selection==Some(common::SwitchTarget::Cli) {None} else {local::desktop_executable()?};
    let cli = if selection==Some(common::SwitchTarget::Desktop) {None} else {local::standalone_cli()?};
    common::selected_clients(selection,cli.is_some(),desktop.is_some())?;
    // Prefer the bundled engine when a shared database is encountered.
    if let Some(app) = &desktop {
        let cli = local::bundled_cli(app).ok_or("opencode_desktop_cli_not_found")?;
        let db = local::database_for(&cli, &cfg.desktop_db_path)?;
        add_target(&mut targets, cli, db, "desktop");
    }
    if let Some(cli) = cli {
        let db = local::database_for(&cli, &cfg.db_path)?;
        add_target(&mut targets, cli, db, "cli");
    }
    if targets.is_empty() {
        return Err("client_not_installed".into());
    }
    Ok((targets, desktop))
}
fn restore(target: &mut Target, attempt: &Attempt) -> Result<(), String> {
    if let Some(previous) = &attempt.previous {
        return target.activate(previous);
    }
    if let Some(active) = target.active()? {
        // With no previous login, remove only the newly-created native credential.
        if attempt.known.contains(&active) {
            return Err("opencode_switch_restore_failed".into());
        }
        target.api("delete", &format!("/api/credential/{active}"), None)?;
        if target.active()?.is_some() {
            return Err("opencode_switch_restore_failed".into());
        }
    }
    Ok(())
}
fn finish_target(job: &mut Job, result: Result<(), String>) {
    for label in &job.targets[job.index].labels {
        job.clients
            .push(common::client_result(label, result.clone()));
    }
    job.index += 1;
    job.attempt = None;
}
fn completed(job: &mut Job, key: &str) -> Value {
    // A cancellation may leave later independent targets untouched.
    for target in &job.targets {
        for label in &target.labels {
            if !job.clients.iter().any(|v| v["target"] == label.as_str()) {
                job.clients.push(common::client_result(
                    label,
                    Err("client_switch_cancelled".into()),
                ));
            }
        }
    }
    if !job.launch_finalized {
        job.launch_finalized = true;
        if let Some(app) = &job.desktop {
            let should_launch = job.was_running
                || job
                    .clients
                    .iter()
                    .any(|v| v["target"] == "desktop" && v["status"] == "success");
            if should_launch && !local::launch_desktop(app) {
                if let Some(index) = job
                    .targets
                    .iter()
                    .position(|t| t.labels.iter().any(|v| v == "desktop"))
                {
                    let target = &mut job.targets[index];
                    if let Some(before) = target.before.clone() {
                        if restore(target, &before).is_err() {
                            job.index = index;
                            job.attempt = Some(before);
                            return json!({"state":"pending","id":key,"error":"opencode_switch_restore_failed",
                            "instructions":"Desktop 启动失败，请取消以恢复原登录。","clients":job.clients});
                        }
                        for label in &target.labels {
                            if let Some(client) = job
                                .clients
                                .iter_mut()
                                .find(|v| v["target"] == label.as_str())
                            {
                                *client = common::client_result(
                                    label,
                                    Err("client_launch_failed_rolled_back".into()),
                                );
                            }
                        }
                        if job.was_running {
                            let _ = local::launch_desktop(app);
                        }
                    } else if let Some(client) =
                        job.clients.iter_mut().find(|v| v["target"] == "desktop")
                    {
                        *client =
                            common::client_result("desktop", Err("client_launch_failed".into()));
                    }
                }
            }
        }
    }
    crate::audit::log(
        "client_switch_result",
        "OpenCode 本机客户端切换结果".into(),
        Some(json!({"id":job.account,"kind":"opencode","clients":job.clients})),
    );
    common::switch_result(std::mem::take(&mut job.clients), None)
}
fn validate_account(job: &Job) -> Result<(), String> {
    let account = accounts::account_snapshot(&job.account)?;
    if account.kind != "opencode" || account.token != job.workspace {
        return Err("account_changed".into());
    }
    Ok(())
}
fn advance(job: &mut Job, key: &str) -> Result<Value, String> {
    while job.index < job.targets.len() {
        validate_account(job)?;
        let target = &mut job.targets[job.index];
        let credentials = match target.credentials() {
            Ok(v) => v,
            Err(e) => {
                finish_target(job, Err(e));
                continue;
            }
        };
        let previous = match target.active() {
            Ok(v) => v,
            Err(error) => {
                finish_target(job, Err(error));
                continue;
            }
        };
        let known: Vec<String> = credentials
            .iter()
            .map(|c| c.credential_id.clone())
            .collect();
        let before = Attempt {
            id: String::new(),
            previous: previous.clone(),
            known: known.clone(),
            deadline: chrono::Utc::now().timestamp_millis() + 15 * 60_000,
        };
        target.before = Some(before.clone());
        if let Some(login) = credentials.iter().find(|c| c.workspace_id == job.workspace) {
            let result = target.activate(&login.credential_id);
            if result.is_err() {
                job.attempt = Some(before.clone());
                restore(target, &before)?;
            }
            finish_target(job, result);
            continue;
        }
        let response = match target.api(
            "post",
            "/api/integration/opencode/connect/oauth",
            Some(json!({"methodID":"device"})),
        ) {
            Ok(v) => v,
            Err(error) => {
                finish_target(job, Err(error));
                continue;
            }
        };
        let id = opencode::text(&response, "attemptID").to_string();
        if !opencode::valid_workspace(&id) {
            return Err("opencode_cli_invalid_response".into());
        }
        job.attempt = Some(Attempt {
            id,
            previous,
            known,
            deadline: response
                .pointer("/time/expires")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis() + 15 * 60_000),
        });
        let url = opencode::authorization_url(opencode::text(&response, "url"))?;
        let opened = opencode::open_browser(&url);
        return Ok(
            json!({"state":"pending","id":key,"url":url,"instructions":response["instructions"],"opened":opened,"clients":job.clients}),
        );
    }
    Ok(completed(job, key))
}
pub async fn start(id: String, close_desktop: bool, selection: Option<common::SwitchTarget>) -> Result<Value, String> {
    ensure_idle()?;
    let result = tauri::async_runtime::spawn_blocking(move||{
        let account=accounts::account_snapshot(&id)?;
        if account.kind!="opencode"{return Err("not_opencode_account".into());}
        let (targets,desktop)=resolve_targets(selection)?;
        let running=desktop.as_ref().is_some_and(|p|local::desktop_running(p));
        if running&&!close_desktop{return Err("client_close_required".into());}
        let operation = crate::operations::begin()?;
        if let Some(app)=&desktop {if running&&!local::close_desktop(app){return Err("client_close_failed".into());}}
        let mut job=Job{account:id,workspace:account.token,targets,index:0,clients:Vec::new(),attempt:None,desktop,was_running:running,launch_finalized:false,_operation:operation};
        let key=accounts::new_id();
        let mut result=advance(&mut job,&key);
        if result.is_err() && job.attempt.is_some() {
            let attempt=job.attempt.as_ref().ok_or("opencode_oauth_expired")?;
            let target=&mut job.targets[job.index];
            let restored=cancel_attempt(target,attempt).and_then(|_|restore(target,attempt));
            if restored.is_ok(){job.attempt=None;}
            else {result=Ok(json!({"state":"pending","id":key,"error":"opencode_switch_restore_failed","instructions":"授权未能启动，请取消以恢复原登录。"}));}
        }
        if result.is_err() && job.attempt.is_none() {
            let error=result.as_ref().err().cloned().unwrap_or_default();
            for target in job.targets.iter().skip(job.index) {for label in &target.labels {
                job.clients.push(common::client_result(label,Err(error.clone())));
            }}
            result=Ok(completed(&mut job,&key));
        }
        if job.attempt.is_some(){jobs().lock().map_err(|_|"state_lock_poisoned")?.insert(key,job);}
        result
    }).await.map_err(|_|"opencode_cli_failed")??;
    if let Some(id) = result["id"]
        .as_str()
        .filter(|_| result["state"] == "pending")
    {
        let id = id.to_string();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let expired = jobs().lock().ok().and_then(|all| {
                    all.get(&id).map(|job| {
                        job.attempt
                            .as_ref()
                            .is_some_and(|a| chrono::Utc::now().timestamp_millis() >= a.deadline)
                            || accounts::account_snapshot(&job.account).is_err()
                    })
                });
                match expired {
                    None => break,
                    Some(true) => {
                        let _ = local::opencode_client_oauth_poll(id.clone(), true).await;
                    }
                    Some(false) => {}
                }
            }
        });
    }
    Ok(result)
}
pub async fn poll(id: String, cancel: bool) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(mut job) = jobs()
            .lock()
            .map_err(|_| "state_lock_poisoned")?
            .remove(&id)
        else {
            return if cancel {
                Ok(json!({"state":"cancelled"}))
            } else {
                Err("opencode_oauth_expired".into())
            };
        };
        let result = (|| {
            let attempt = job.attempt.as_ref().ok_or("opencode_oauth_expired")?;
            let expired = chrono::Utc::now().timestamp_millis() >= attempt.deadline;
            let changed = validate_account(&job).is_err();
            let target = &mut job.targets[job.index];
            if attempt.id.is_empty() {
                // No OAuth remains: only a failed native activation/launch rollback.
                restore(target, attempt)?;
                for label in &target.labels {
                    if let Some(client) = job
                        .clients
                        .iter_mut()
                        .find(|v| v["target"] == label.as_str())
                    {
                        *client =
                            common::client_result(label, Err("client_switch_cancelled".into()));
                    } else {
                        job.clients.push(common::client_result(
                            label,
                            Err("client_switch_cancelled".into()),
                        ));
                    }
                }
                job.attempt = None;
                if job.was_running {
                    if let Some(app) = &job.desktop {
                        let _ = local::launch_desktop(app);
                    }
                }
                let mut result = completed(&mut job, &id);
                if result["state"] == "complete" {
                    result["state"] = json!("cancelled");
                }
                return Ok(result);
            }
            let response = target.api(
                if cancel || expired || changed {
                    "delete"
                } else {
                    "get"
                },
                &format!("/api/integration/opencode/connect/oauth/{}", attempt.id),
                None,
            )?;
            if !cancel && !expired && !changed && response["status"] == "pending" {
                return Ok(json!({"state":"pending","id":id}));
            }
            let switched = if !cancel && !expired && !changed && response["status"] == "complete" {
                target.credentials().and_then(|all| {
                    let login = all
                        .iter()
                        .find(|c| c.workspace_id == job.workspace)
                        .ok_or("opencode_workspace_mismatch")?;
                    target.activate(&login.credential_id)
                })
            } else {
                Err(if changed {
                    "account_changed"
                } else if expired {
                    "opencode_oauth_expired"
                } else {
                    "opencode_client_auth_failed"
                }
                .into())
            };
            if switched.is_err() {
                restore(target, attempt)?;
            }
            finish_target(&mut job, switched);
            if cancel || expired || changed {
                let mut result = completed(&mut job, &id);
                if result["state"] == "complete" {
                    result["state"] = json!("cancelled");
                }
                return Ok(result);
            }
            advance(&mut job, &id)
        })();
        let result = if result.is_err() && job.attempt.is_none() {
            Ok(completed(&mut job, &id))
        } else {
            result
        };
        if job.attempt.is_some() {
            jobs()
                .lock()
                .map_err(|_| "state_lock_poisoned")?
                .insert(id, job);
        }
        result
    })
    .await
    .map_err(|_| "opencode_cli_failed")?
}

fn cancel_attempt(target: &mut Target, attempt: &Attempt) -> Result<(), String> {
    if !attempt.id.is_empty() {
        target.api(
            "delete",
            &format!("/api/integration/opencode/connect/oauth/{}", attempt.id),
            None,
        )?;
    }
    Ok(())
}

/// Normal application exit: cancel outstanding native authorization, restore the
/// previous activation where available, then drop every owned helper process.
pub fn shutdown() {
    let Ok(mut all) = jobs().lock() else { return };
    for (_, mut job) in all.drain() {
        if let Some(attempt) = job.attempt.take() {
            if let Some(server) = job.targets[job.index].server.as_ref() {
                let timeout = Duration::from_secs(1);
                if !attempt.id.is_empty() { let _ = server.request(
                    "delete",
                    &format!("/api/integration/opencode/connect/oauth/{}", attempt.id),
                    None,
                    timeout,
                ); }
                if let Some(previous) = attempt.previous {
                    let _ = server.request(
                        "post",
                        &format!("/api/credential/{previous}/activate"),
                        None,
                        timeout,
                    );
                } else if let Ok(Some(active))=job.targets[job.index].active() {
                    if !attempt.known.contains(&active) {
                        let _=server.request("delete",&format!("/api/credential/{active}"),None,timeout);
                    }
                }
            }
        }
        if job.was_running {
            if let Some(app) = &job.desktop {
                let _ = local::launch_desktop(app);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn shared_database_has_one_activation_target() {
        let mut targets = Vec::new();
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("opencode.db");
        fs::write(&db, []).unwrap();
        add_target(&mut targets, "desktop-cli".into(), db.clone(), "desktop");
        add_target(&mut targets, "cli".into(), db, "cli");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].labels, vec!["desktop", "cli"]);
    }
    #[test]
    fn native_api_envelopes_are_normalized() {
        assert_eq!(
            unwrap_data(json!({"data":[{"id":"cred"}]})),
            json!([{"id":"cred"}])
        );
        assert_eq!(
            unwrap_data(json!({"location":{},"data":{"status":"complete"}}))["status"],
            "complete"
        );
    }
    #[tokio::test]
    async fn failed_desktop_launch_restores_only_the_affected_database() {
        let _fixture = local::tests::native_fixture_lock().lock().await;
        for shared in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let db_path = root.path().join("desktop.db");
            let db = rusqlite::Connection::open(&db_path).unwrap();
            db.execute_batch("CREATE TABLE credential(id TEXT,integration_id TEXT,active INTEGER,time_created INTEGER); INSERT INTO credential VALUES('previous','opencode',0,1),('target','opencode',1,2);").unwrap();
            let file = db_path.clone();
            local::tests::set_runner(Some(std::sync::Arc::new(move |args| {
                assert_eq!(
                    &args[..3],
                    &["api", "post", "/api/credential/previous/activate"]
                );
                let db = rusqlite::Connection::open(&file).unwrap();
                db.execute("UPDATE credential SET active=(id='previous')", [])
                    .unwrap();
                Ok("null".into())
            })));
            local::tests::set_launch_fixture(Some(false));
            let mut targets = Vec::new();
            add_target(&mut targets, "fixture".into(), db_path.clone(), "desktop");
            add_target(
                &mut targets,
                "fixture".into(),
                if shared {
                    db_path
                } else {
                    root.path().join("cli.db")
                },
                "cli",
            );
            targets[0].before = Some(Attempt {
                id: String::new(),
                previous: Some("previous".into()),
                known: vec!["previous".into(), "target".into()],
                deadline: 0,
            });
            let mut job = Job {
                account: "fixture".into(),
                workspace: "wrk_target".into(),
                index: targets.len(),
                targets,
                clients: vec![
                    common::client_result("cli", Ok(())),
                    common::client_result("desktop", Ok(())),
                ],
                attempt: None,
                desktop: Some(root.path().join("Desktop.exe")),
                was_running: false,
                launch_finalized: false,
                _operation: crate::operations::begin().unwrap(),
            };
            let result = completed(&mut job, "operation");
            assert_eq!(result["switched"], false);
            assert_eq!(
                result["clients"][0]["status"],
                if shared { "failed" } else { "success" }
            );
            assert_eq!(
                result["clients"][1]["error"],
                "client_launch_failed_rolled_back"
            );
            assert_eq!(
                db.query_row("SELECT id FROM credential WHERE active=1", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "previous"
            );
            local::tests::set_runner(None);
            local::tests::set_launch_fixture(None);
        }
    }
}
