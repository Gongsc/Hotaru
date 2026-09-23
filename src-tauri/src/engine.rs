//! The monitoring loop: pick a backend, run its session, publish what it
//! reports.
//!
//! Everything backend-specific lives in [`crate::providers`]; what stays here
//! is the part both share — watching the settings, backing off, reporting
//! errors, folding ping figures into the snapshot and rate-limiting the tray.

use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tauri::{AppHandle, Emitter, Manager};

use crate::models::{
    normalize_base, MonitorSnapshot, NetFrame, NodeSnapshot, PingData, PingSummary, Settings,
};
use crate::providers::Provider;
use crate::state::AppState;
use crate::tray;

const EVENT: &str = "monitor://update";
pub const NODES_REFRESH: Duration = Duration::from_secs(60);
const TRAY_MIN_INTERVAL: Duration = Duration::from_millis(1000);
pub const WS_READ_TIMEOUT: Duration = Duration::from_secs(15);
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(engine(app));
}

async fn engine(app: AppHandle) {
    let mut rx = app.state::<AppState>().config_epoch_tx.subscribe();
    loop {
        let settings = app.state::<AppState>().settings.read().clone().sanitized();
        if settings.backend_url.is_empty() {
            set_error(&app, "未配置后端地址，请在设置中填写");
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        }
        let client = build_client(&settings);
        match session(&app, &client, &settings, &mut rx).await {
            Ok(()) => continue, // settings changed -> reconnect with new config
            Err(e) => set_error(&app, &e),
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONNECT_BACKOFF) => {}
            _ = rx.changed() => {}
        }
    }
}

/// Resolve the backend, remember it for the ping loop and the popover's
/// on-demand fetch, then hand over to it.
async fn session(
    app: &AppHandle,
    client: &reqwest::Client,
    settings: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
) -> Result<(), String> {
    let base = normalize_base(&settings.backend_url)?;
    let provider = Provider::resolve(client, settings, &base).await?;
    *app.state::<AppState>().provider.write() = Some(provider);
    log::info!("后端类型: {}", provider.label());
    let mut publisher = Publisher::new();
    provider
        .run(app, client, settings, rx, &mut publisher)
        .await
}

pub fn build_client(s: &Settings) -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(s.accept_invalid_certs)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// Pushes snapshots to the popover, the stored state and the tray.
///
/// Carries the tray's rate limit, which is per session rather than global: a
/// provider ticking faster than once a second must not redraw the menu bar
/// that often, and the limit has to survive across ticks.
pub struct Publisher {
    last_tray: Instant,
}

impl Publisher {
    pub fn new() -> Self {
        Self {
            // Set in the past so the first snapshot of a session reaches the
            // tray immediately instead of waiting out the interval.
            last_tray: Instant::now() - TRAY_MIN_INTERVAL,
        }
    }

    pub fn publish(&mut self, app: &AppHandle, mut snap: MonitorSnapshot) {
        // Latency and loss ride along with every other figure, so a node card
        // has them the moment it opens instead of firing its own request. Read
        // out of the ping cache before touching the snapshot lock: the ping
        // loop takes them in the other order.
        let summaries = ping_summaries(app);
        {
            let st = app.state::<AppState>();
            merge_ping(&mut snap.nodes, &summaries);
            *st.snapshot.write() = snap.clone();
            let nodes = snap
                .nodes
                .iter()
                .map(|n| (n.uuid.clone(), n.net_up, n.net_down, n.online))
                .collect();
            st.net_history.push(NetFrame {
                t: snap.last_update_ms,
                nodes,
            });
        }
        let _ = app.emit(EVENT, &snap);
        if self.last_tray.elapsed() >= TRAY_MIN_INTERVAL {
            self.last_tray = Instant::now();
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || tray::apply(&app2));
        }
    }
}

impl Default for Publisher {
    fn default() -> Self {
        Self::new()
    }
}

pub fn set_error(app: &AppHandle, msg: &str) {
    {
        let st = app.state::<AppState>();
        let mut snap = st.snapshot.write();
        snap.backend_ok = false;
        snap.error = Some(msg.to_string());
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || tray::apply(&app2));
}

pub fn clear_error(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        let st = app2.state::<AppState>();
        let mut snap = st.snapshot.write();
        snap.backend_ok = true;
        snap.error = None;
    });
}

// ---------------------------------------------------------------------------
// Ping cache
// ---------------------------------------------------------------------------

/// How often the ping cache is refreshed. Far slower than the snapshot poll:
/// the backends' own ping tasks run on the order of a minute, so anything
/// quicker would just be extra requests for identical records.
const PING_REFRESH: Duration = Duration::from_secs(60);
/// Retry gap while there is nothing to fetch yet (engine still connecting).
const PING_RETRY: Duration = Duration::from_secs(2);
/// Window of ping history kept, matching the popover's 1-hour quality grid.
pub const PING_HOURS: u64 = 1;

/// Keep the ping cache warm for whichever nodes the snapshot currently holds.
/// Neither backend takes more than one node per request, so this is one
/// request per node — issued at whatever concurrency that backend tolerates,
/// and only once a minute.
pub fn spawn_ping_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let filled = refresh_ping_cache(&app).await;
            // Nothing fetched means there was nothing to fetch yet — no backend
            // configured, or the snapshot has no nodes because the engine is
            // still connecting. Look again shortly instead of idling a whole
            // minute, or the first minute after launch (and every backend
            // switch, which resets the snapshot) would leave the cards without
            // latency while CPU and memory are already there.
            tokio::time::sleep(if filled { PING_REFRESH } else { PING_RETRY }).await;
        }
    });
}

/// Refresh the ping cache for the nodes the snapshot currently holds. Returns
/// whether anything was stored.
async fn refresh_ping_cache(app: &AppHandle) -> bool {
    let settings = app.state::<AppState>().settings.read().clone().sanitized();
    let Ok(base) = normalize_base(&settings.backend_url) else {
        return false;
    };
    let Some(provider) = *app.state::<AppState>().provider.read() else {
        return false; // the engine has not identified the backend yet
    };
    let uuids: Vec<String> = app
        .state::<AppState>()
        .snapshot
        .read()
        .nodes
        .iter()
        .map(|n| n.uuid.clone())
        .collect();
    if uuids.is_empty() {
        return false;
    }
    let client = build_client(&settings);
    let (client, base, key) = (&client, base.as_str(), settings.api_key.as_str());
    let fetched: Vec<Option<(String, PingData)>> = futures_util::stream::iter(uuids)
        .map(|uuid| async move {
            let (points, summary) = provider
                .fetch_ping(client, base, key, &uuid, PING_HOURS)
                .await?;
            Some((uuid, PingData { points, summary }))
        })
        .buffer_unordered(provider.ping_concurrency())
        .collect()
        .await;
    let fresh: std::collections::BTreeMap<_, _> = fetched.into_iter().flatten().collect();
    if fresh.is_empty() {
        // Reachable backend with no ping tasks at all: stop retrying every few
        // seconds, there is nothing to find.
        return true;
    }
    *app.state::<AppState>().ping_records.write() = fresh;
    // The snapshot in memory predates this fetch; fold the new figures into it
    // and push, rather than making the popover wait for the next poll tick.
    let summaries = ping_summaries(app);
    let st = app.state::<AppState>();
    let updated = {
        let mut snap = st.snapshot.write();
        merge_ping(&mut snap.nodes, &summaries);
        snap.clone()
    };
    let _ = app.emit(EVENT, updated);
    true
}

/// Latest summary per uuid. Cloned out so the ping and snapshot locks are never
/// held at once — `publish` and this path would otherwise take them in opposite
/// orders.
fn ping_summaries(app: &AppHandle) -> std::collections::BTreeMap<String, PingSummary> {
    app.state::<AppState>()
        .ping_records
        .read()
        .iter()
        .map(|(uuid, data)| (uuid.clone(), data.summary.clone()))
        .collect()
}

fn merge_ping(
    nodes: &mut [NodeSnapshot],
    summaries: &std::collections::BTreeMap<String, PingSummary>,
) {
    for node in nodes {
        if let Some(s) = summaries.get(&node.uuid) {
            node.latency = s.latency;
            node.loss = s.loss;
        }
    }
}
