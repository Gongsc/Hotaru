use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;

use crate::engine;
use crate::models::{
    normalize_base, now_ms, BackendKind, MonitorSnapshot, NetPoint, PingPoint, Settings,
};
use crate::providers::{self, Provider};
use crate::settings;
use crate::state::AppState;
use crate::windows;

const GITHUB_REPOSITORY_URL: &str = "https://github.com/Gongsc/hotaru";
const GITHUB_RELEASES_URL: &str = "https://github.com/Gongsc/hotaru/releases/latest";
const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Gongsc/hotaru/releases/latest";

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    pub repository_url: &'static str,
}

#[tauri::command]
pub fn get_app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        repository_url: GITHUB_REPOSITORY_URL,
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    name: Option<String>,
    published_at: Option<String>,
}

#[derive(Serialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub release_name: Option<String>,
    pub published_at: Option<String>,
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateCheckResult, String> {
    let current = app.package_info().version.clone();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("无法初始化更新检查: {e}"))?;
    let response = client
        .get(GITHUB_LATEST_RELEASE_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, format!("Hotaru/{current}"))
        .header("X-GitHub-Api-Version", "2026-03-10")
        .send()
        .await
        .map_err(|e| format!("无法连接 GitHub: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("GitHub 仓库尚未发布 Release".into());
    }
    if !status.is_success() {
        return Err(format!("GitHub 返回 HTTP {status}"));
    }
    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("GitHub Release 响应格式无效: {e}"))?;
    let latest = parse_release_version(&release.tag_name)?;
    Ok(UpdateCheckResult {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        update_available: latest > current,
        release_url: release.html_url,
        release_name: release.name,
        published_at: release.published_at,
    })
}

fn parse_release_version(tag: &str) -> Result<semver::Version, String> {
    semver::Version::parse(tag.trim().trim_start_matches(['v', 'V']))
        .map_err(|_| format!("无法识别 Release 版本号：{tag}"))
}

fn github_page_url(page: &str) -> Option<&'static str> {
    match page {
        "repository" => Some(GITHUB_REPOSITORY_URL),
        "releases" => Some(GITHUB_RELEASES_URL),
        _ => None,
    }
}

#[tauri::command]
pub fn open_github_page(page: String) -> Result<(), String> {
    let url = github_page_url(&page).ok_or_else(|| "不支持的 GitHub 页面".to_string())?;
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return Err("当前平台不支持打开外部链接".into());
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    result
        .map(|_| ())
        .map_err(|e| format!("无法打开系统浏览器: {e}"))
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().clone()
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    let previous = state.settings.read().clone();
    let s = settings.sanitized();
    settings::save(&app, &s)?;
    // A different site (or different credentials) means a different set of
    // nodes. Drop the old snapshot and history right away, otherwise the
    // popover keeps listing the previous backend's nodes — and charting their
    // history — until the engine finishes reconnecting.
    let target_changed = previous.backend_url != s.backend_url
        || previous.backend_kind != s.backend_kind
        || previous.api_key != s.api_key
        || previous.accept_invalid_certs != s.accept_invalid_certs;
    *state.settings.write() = s.clone();
    if target_changed {
        state.net_history.clear();
        state.ping_records.write().clear();
        let connecting = MonitorSnapshot::offline("正在连接后端…");
        *state.snapshot.write() = connecting.clone();
        let _ = app.emit("monitor://reset", ());
        let _ = app.emit("monitor://update", connecting);
    }
    state.bump_config_epoch();
    windows::sync_theme(&app, s.theme);
    let _ = app.emit("theme://changed", s.theme);
    // The popover is hidden rather than reloaded between opens, so it needs a
    // nudge to pick up things like the hidden-node set while it stays alive.
    let _ = app.emit("settings://changed", ());
    windows::sync_panel_url(&app, &s.backend_url);
    crate::tray::refresh(&app);
    Ok(s)
}

/// Force the monitor engine to restart its session, which re-reads
/// `/api/nodes` immediately. Between restarts that list is only refreshed once
/// a minute, so a node added or renamed on the backend would otherwise keep
/// showing the stale set in the tray popover long after the settings window's
/// picker has been refreshed by hand.
#[tauri::command]
pub fn resync_nodes(state: State<'_, AppState>) {
    state.bump_config_epoch();
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> MonitorSnapshot {
    state.snapshot.read().clone()
}

/// Target point count per series returned by `get_net_history`; larger
/// histories are bucket-averaged so the JSON stays small.
const HISTORY_MAX_SERIES_POINTS: usize = 720;

#[derive(Serialize)]
pub struct NetHistoryPayload {
    pub aggregate: Vec<NetPoint>,
    pub nodes: BTreeMap<String, Vec<NetPoint>>,
}

#[tauri::command]
pub fn get_net_history(state: State<'_, AppState>, range_secs: u64) -> NetHistoryPayload {
    let range_secs = range_secs.clamp(60, crate::state::HISTORY_RETENTION_MS / 1000);
    let since = now_ms().saturating_sub(range_secs * 1000);
    let frames = state.net_history.since(since);

    let mut nodes: BTreeMap<String, Vec<NetPoint>> = BTreeMap::new();
    for f in &frames {
        for (uuid, up, down, online) in &f.nodes {
            nodes.entry(uuid.clone()).or_default().push(NetPoint {
                t: f.t,
                up: *up,
                down: *down,
                online: *online,
            });
        }
    }

    NetHistoryPayload {
        aggregate: downsample(
            &frames
                .iter()
                .map(|f| NetPoint {
                    t: f.t,
                    up: f.nodes.iter().map(|(_, up, _, _)| up).sum(),
                    down: f.nodes.iter().map(|(_, _, down, _)| down).sum(),
                    online: true,
                })
                .collect::<Vec<_>>(),
        ),
        nodes: nodes
            .into_iter()
            .map(|(uuid, pts)| (uuid, downsample(&pts)))
            .collect(),
    }
}

/// Bucket-average a series down to at most `HISTORY_MAX_SERIES_POINTS` points.
fn downsample(points: &[NetPoint]) -> Vec<NetPoint> {
    if points.len() <= HISTORY_MAX_SERIES_POINTS {
        return points.to_vec();
    }
    let step = points.len().div_ceil(HISTORY_MAX_SERIES_POINTS);
    points
        .chunks(step)
        .map(|chunk| {
            let n = chunk.len() as f64;
            NetPoint {
                t: chunk[chunk.len() / 2].t,
                up: chunk.iter().map(|p| p.up).sum::<f64>() / n,
                down: chunk.iter().map(|p| p.down).sum::<f64>() / n,
                online: chunk.iter().any(|p| p.online),
            }
        })
        .collect()
}

/// One row of the settings window's node picker.
#[derive(Serialize)]
pub struct NodeOption {
    pub uuid: String,
    pub name: String,
    pub tags: Vec<String>,
}

/// Fetch the node list straight from the backend, using the settings currently
/// entered in the form rather than the saved ones — so nodes can be picked
/// before the connection is ever saved. Unlike `get_snapshot` this also lists
/// nodes that have never reported.
#[tauri::command]
pub async fn list_nodes(settings: Settings) -> Result<Vec<NodeOption>, String> {
    let s = settings.sanitized();
    let (_, _, nodes) = probe(&s).await?;
    Ok(nodes)
}

#[derive(Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub node_count: usize,
    pub version: String,
    pub message: String,
}

#[tauri::command]
pub async fn test_connection(settings: Settings) -> Result<TestResult, String> {
    let s = settings.sanitized();
    let (provider, version, nodes) = probe(&s).await?;
    let count = nodes.len();
    Ok(TestResult {
        ok: true,
        node_count: count,
        message: match version.is_empty() {
            true => format!("连接成功（{}），发现 {count} 个节点", provider.label()),
            false => format!(
                "连接成功（{} · {version}），发现 {count} 个节点",
                provider.label()
            ),
        },
        version,
    })
}

/// One `/api/nodes` round trip serving both commands above: which backend
/// answered, something to show as its version, and the node list.
///
/// The response body is what identifies the backend, so detection costs no
/// extra request — and `list_nodes` must work before the connection has ever
/// been saved, which rules out reading the engine's resolved provider here.
async fn probe(s: &Settings) -> Result<(Provider, String, Vec<NodeOption>), String> {
    let base = normalize_base(&s.backend_url)?;
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(s.accept_invalid_certs)
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;

    // Carries the API Key so a private Komari site answers rather than 401s;
    // a 极简探针 hub authenticates by cookie and ignores the header.
    let text = providers::monitor::fetch_nodes_text(&client, &base, &s.api_key)
        .await
        .map_err(|e| e.explain(unauthorized_hint(s.backend_kind)))?;

    let looks_like = providers::sniff(&text);
    let provider = match s.backend_kind {
        BackendKind::Komari => Provider::Komari,
        BackendKind::Monitor => Provider::Monitor,
        BackendKind::Auto => {
            looks_like.ok_or_else(|| "响应既不是 Komari 也不是极简探针的 /api/nodes".to_string())?
        }
    };
    // Picking the wrong type by hand is easy now that there is a picker, and
    // the parse error it produces ("missing field `status`") reads like a
    // broken site rather than a setting to change.
    if let Some(actual) = looks_like.filter(|a| *a != provider) {
        return Err(format!(
            "后端类型选的是 {}，但这个站点看起来是 {}",
            provider.label(),
            actual.label()
        ));
    }

    match provider {
        Provider::Komari => {
            let mut nodes: Vec<NodeOption> = providers::komari::parse_nodes(&text)?
                .into_values()
                .map(|info| NodeOption {
                    name: komari_display_name(&info),
                    tags: providers::komari::split_tags(&info.tags),
                    uuid: info.uuid,
                })
                .collect();
            nodes.sort_by(|a, b| a.name.cmp(&b.name));
            Ok((provider, komari_version(&client, &base).await, nodes))
        }
        Provider::Monitor => {
            let nodes = providers::monitor::parse_frame(&text)?
                .into_iter()
                .map(|n| NodeOption {
                    uuid: n.uuid,
                    name: n.name,
                    tags: n.tags,
                })
                .collect();
            // The hub has no /api/version; its own name is the useful thing to
            // echo back, and seeing it confirms the site really is a hub.
            let name = providers::monitor::fetch_site_name(&client, &base)
                .await
                .unwrap_or_default();
            Ok((provider, name, nodes))
        }
    }
}

/// What a 401 means, as far as the configured kind reveals.
fn unauthorized_hint(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Komari => {
            "HTTP 401：站点为私有模式，需要 API Key（管理后台 → 设置 → API Key）"
        }
        BackendKind::Monitor => providers::monitor::NEEDS_PUBLIC_PAGE,
        BackendKind::Auto => providers::UNAUTHORIZED_UNKNOWN_KIND,
    }
}

async fn komari_version(client: &reqwest::Client, base: &str) -> String {
    match client.get(format!("{base}/api/version")).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Same fallback the Komari provider uses, so the picker and the popover agree
/// on the label of a node whose name the backend left empty.
fn komari_display_name(info: &providers::komari::ClientInfo) -> String {
    if info.name.trim().is_empty() {
        format!("节点 {}", &info.uuid[..info.uuid.len().min(8)])
    } else {
        info.name.clone()
    }
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enable: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    let result = if enable {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|e| e.to_string())
}

// `(async)` is load-bearing on both window-opening commands: a plain
// `#[tauri::command]` body runs inline on the main thread, inside the calling
// webview's WebView2 IPC callback. `run_on_main_thread` then sees the main
// thread and also runs inline, so the window is built from within that
// callback — and building a webview pumps a nested message loop
// (`webview2_com::wait_with_pump`), which WebView2 does not allow while it is
// dispatching an event. The controller never finishes initializing (white
// window) and the pump never returns (the whole app hangs). Running the
// command off the main thread makes `run_on_main_thread` queue the work on the
// event loop proxy instead, so creation happens between callbacks.
#[tauri::command(async)]
pub fn open_panel_cmd(app: AppHandle) {
    windows::open_panel(&app);
}

#[tauri::command(async)]
pub fn open_settings_cmd(app: AppHandle) {
    windows::open_settings(&app);
}

/// Dismiss the popover from the page (Esc, or after it opens another window).
///
/// The page could hide its own window, but then Rust never learns the popover
/// left the screen and the menu bar item stays looking selected.
#[tauri::command]
pub fn hide_chart(app: AppHandle) {
    windows::close_chart(&app);
}

#[tauri::command]
pub fn set_chart_pinned(
    app: AppHandle,
    state: State<'_, AppState>,
    pinned: bool,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("chart") {
        window
            .set_always_on_top(pinned)
            .map_err(|e| e.to_string())?;
    }
    state
        .chart_pinned
        .store(pinned, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn get_chart_pinned(state: State<'_, AppState>) -> bool {
    state
        .chart_pinned
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Resize the popover once the page knows its content height. Rust owns the
/// clamp and the reposition: it knows which edge is anchored to the tray icon
/// and grows the window away from it.
#[tauri::command]
pub fn resize_chart(app: AppHandle, height: f64) -> f64 {
    windows::resize_chart(&app, height)
}

/// Serve the ping records the monitor already keeps for this node. The engine
/// refreshes them in the background like every other node figure, so expanding
/// a card no longer waits on a request; only a node the loop has not covered
/// yet falls through to a live fetch.
#[tauri::command]
pub async fn get_ping_records(
    state: State<'_, AppState>,
    uuid: String,
    hours: u64,
) -> Result<Vec<PingPoint>, String> {
    if let Some(cached) = state.ping_records.read().get(&uuid) {
        return Ok(cached.points.clone());
    }
    let provider = state
        .provider
        .read()
        .ok_or_else(|| "尚未连接后端".to_string())?;
    let s = state.settings.read().clone().sanitized();
    let base = normalize_base(&s.backend_url)?;
    let hours = hours.clamp(1, 24);
    let client = engine::build_client(&s);
    provider
        .fetch_ping(&client, &base, &s.api_key, &uuid, hours)
        .await
        .map(|(points, _)| points)
        .ok_or_else(|| "无法获取 ping 记录".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: u64, up: f64) -> NetPoint {
        NetPoint {
            t,
            up,
            down: up,
            online: true,
        }
    }

    #[test]
    fn downsample_short_series_unchanged() {
        let pts: Vec<NetPoint> = (0..100).map(|i| pt(i * 1000, i as f64)).collect();
        assert_eq!(downsample(&pts), pts);
    }

    #[test]
    fn downsample_buckets_average() {
        let pts: Vec<NetPoint> = (0..1000).map(|i| pt(i * 1000, i as f64)).collect();
        let out = downsample(&pts);
        assert!(out.len() <= HISTORY_MAX_SERIES_POINTS);
        // 1000 -> step 2 -> 500 buckets averaging pairs (i, i+1)
        assert_eq!(out.len(), 500);
        assert!((out[0].up - 0.5).abs() < 1e-9);
        assert!((out[1].up - 2.5).abs() < 1e-9);
    }

    #[test]
    fn release_versions_use_semver_ordering() {
        assert_eq!(
            parse_release_version("v1.2.3").unwrap(),
            semver::Version::new(1, 2, 3)
        );
        assert!(parse_release_version("V2.0.0").unwrap() > semver::Version::new(1, 99, 99));
        assert!(
            parse_release_version("v1.2.3").unwrap()
                > parse_release_version("v1.2.3-rc.1").unwrap()
        );
        assert!(parse_release_version("latest").is_err());
    }

    #[test]
    fn github_links_are_allowlisted() {
        assert_eq!(github_page_url("repository"), Some(GITHUB_REPOSITORY_URL));
        assert_eq!(github_page_url("releases"), Some(GITHUB_RELEASES_URL));
        assert_eq!(github_page_url("https://example.com"), None);
    }
}
