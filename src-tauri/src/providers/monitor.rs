//! 极简探针 (`monitor-probe/monitor`).
//!
//! Only four read endpoints exist, and the hub documents them as the whole
//! contract a third-party client may rely on:
//!
//! ```text
//! GET /api/me                    site name, sign-in state, public-page switch
//! GET /api/nodes                 nodes + live metrics + cumulative traffic
//! GET /api/nodes/{id}/metrics    history and ping records
//! GET /api/ws                    pushes the /api/nodes frame every 2s
//! ```
//!
//! Two shapes differ from Komari and drive most of this module:
//!
//! * The live socket *pushes*. There is nothing to send and no interval to
//!   negotiate — the hub ticks every two seconds for every viewer.
//! * `/api/nodes` already carries every node's latest report, so the polling
//!   path is one request rather than one per node.
//!
//! Only anonymous access is supported. The hub has no API key: the sole
//! alternative is an admin session cookie, and an authenticated `/api/nodes`
//! includes every node's plaintext agent token. Holding that to draw a tray
//! icon is not a trade worth making, so a site with its public page switched
//! off is reported as unsupported rather than prompting for a password.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tauri::AppHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::engine::{clear_error, Publisher, WS_READ_TIMEOUT};
use crate::models::{
    normalize_base, now_ms, origin_of, MonitorSnapshot, NodeSnapshot, PingPoint, PingSummary,
    Settings,
};
use crate::providers::FetchError;

/// Which period `NodeSnapshot::traffic_used` covers here: the hub bills a
/// calendar month that restarts on each node's own reset day.
const TRAFFIC_PERIOD: &str = "本月";

/// Shown whenever the hub answers 401. The only fix is on the hub's side, so
/// the message says what to change rather than offering a credential field.
pub const NEEDS_PUBLIC_PAGE: &str = "站点未开启公开页；Hotaru 仅支持可匿名访问的极简探针站点";

/// Above this, the poll interval is honoured over HTTP instead of riding the
/// socket. The hub pushes every 2s and that cadence cannot be negotiated, so a
/// viewer who asked for slower updates to save bandwidth would not get them.
/// `/api/nodes` is served from a 1.9s snapshot cache, so polling it is cheap.
const WS_MAX_INTERVAL_SECS: u64 = 3;

/// `uuid` prefix for this backend.
///
/// Node ids here are small integers, while `hidden_nodes` and `pinned_uuid`
/// outlive a change of backend. Without a prefix, switching a Komari site for
/// a 极简探针 one would silently apply the old site's choices to whichever
/// nodes happen to hold ids 1, 2, 3.
const UUID_PREFIX: &str = "m:";

fn uuid_of(id: i64) -> String {
    format!("{UUID_PREFIX}{id}")
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// `GET /api/nodes`, and every frame `GET /api/ws` pushes — the hub renders one
/// payload and serves it to both.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Frame {
    pub nodes: Vec<Node>,
}

/// Only the fields the popover draws. The anonymous view omits addresses,
/// hostname, remark and token entirely, so nothing here is optional for the
/// wrong reason.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Node {
    pub id: i64,
    pub name: String,
    pub online: bool,
    /// ISO 3166-1 alpha-2, which is what the popover's flag lookup wants.
    pub country: String,
    pub os: String,
    /// `null` for an offline node, and also for one whose agent has connected
    /// but not yet reported.
    pub metrics: Option<Metrics>,
    /// Accumulated by the hub rather than read off the kernel, so a reboot does
    /// not reset them.
    pub total_rx: u64,
    pub total_tx: u64,
    /// Bytes this billing month, already reduced by `traffic_mode`.
    pub month_used: u64,
    pub traffic_limit: u64,
    /// `sum` | `max` | `up` | `down`.
    pub traffic_mode: String,
    /// A bare date, `2026-09-26`.
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Metrics {
    /// Already a percentage, 0–100.
    pub cpu: f64,
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    /// B/s. `rx` is inbound, so it is the node's download.
    pub net_rx: f64,
    pub net_tx: f64,
    pub tcp: u64,
    pub udp: u64,
    pub uptime: u64,
}

pub fn parse_frame(text: &str) -> Result<Vec<NodeSnapshot>, String> {
    let frame: Frame =
        serde_json::from_str(text).map_err(|e| format!("解析 /api/nodes 失败: {e}"))?;
    let mut out: Vec<NodeSnapshot> = frame.nodes.iter().map(node_to_snapshot).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn node_to_snapshot(n: &Node) -> NodeSnapshot {
    let m = n.metrics.clone().unwrap_or_default();
    NodeSnapshot {
        uuid: uuid_of(n.id),
        name: if n.name.trim().is_empty() {
            format!("节点 {}", n.id)
        } else {
            n.name.clone()
        },
        online: n.online,
        region: n.country.clone(),
        os: n.os.clone(),
        // The hub has no groups or tags. A country is the one facet every node
        // carries, and it is the axis a fleet spread across providers is
        // actually read along.
        group: n.country.clone(),
        tags: Vec::new(),
        latency: None,
        loss: None,
        cpu_usage: m.cpu,
        ram_used: m.mem_used,
        ram_total: m.mem_total,
        swap_used: m.swap_used,
        swap_total: m.swap_total,
        disk_used: m.disk_used,
        disk_total: m.disk_total,
        net_up: m.net_tx,
        net_down: m.net_rx,
        total_up: n.total_tx,
        total_down: n.total_rx,
        traffic_limit: n.traffic_limit,
        traffic_limit_type: n.traffic_mode.clone(),
        // The hub already reduced the month by `traffic_mode`; recomputing it
        // from the lifetime totals would bill a quota that resets monthly
        // against a counter that never does.
        traffic_used: n.month_used,
        traffic_period: TRAFFIC_PERIOD.to_string(),
        expired_at: n.expires_at.clone(),
        tcp: m.tcp,
        udp: m.udp,
        uptime_secs: m.uptime,
        has_metrics: n.metrics.is_some(),
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Run a 极简探针 session until the configuration changes (`Ok`) or the
/// connection fails (`Err`).
pub async fn run(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    if s.poll_interval_secs <= WS_MAX_INTERVAL_SECS {
        ws_session(app, s, rx, publisher).await
    } else {
        http_loop(app, client, s, rx, publisher).await
    }
}

/// Read the pushed snapshot stream.
///
/// The hub sends and never asks, so unlike Komari there is nothing to write and
/// no tick of our own: the loop is a plain read with a timeout. A frame every
/// two seconds sits well inside [`WS_READ_TIMEOUT`], and the hub sends no
/// keepalive pings on this route because the data is keepalive enough.
async fn ws_session(
    app: &AppHandle,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let url = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1)
        + "/api/ws";
    let mut request = url
        .clone()
        .into_client_request()
        .map_err(|e| format!("构造 WS 请求失败: {e}"))?;
    let origin =
        HeaderValue::from_str(&origin_of(&base)).map_err(|_| "后端地址含非法字符".to_string())?;
    request.headers_mut().insert("origin", origin);

    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| {
            // tungstenite surfaces the rejected handshake's status, which is the
            // one failure with an answer the user can act on.
            if matches!(&e, tokio_tungstenite::tungstenite::Error::Http(r)
            if r.status() == reqwest::StatusCode::UNAUTHORIZED)
            {
                NEEDS_PUBLIC_PAGE.to_string()
            } else {
                format!("连接失败: {e}")
            }
        })?;
    let (_write, mut read) = ws.split();

    clear_error(app);
    log::info!("WS 已连接（极简探针）: {url}");

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            msg = tokio::time::timeout(WS_READ_TIMEOUT, read.next()) => {
                let msg = match msg {
                    Ok(Some(Ok(m))) => m,
                    Ok(Some(Err(e))) => return Err(format!("读取失败: {e}")),
                    Ok(None) => return Err("连接已被服务端关闭".into()),
                    Err(_) => return Err("读取超时".into()),
                };
                if let Message::Text(txt) = msg {
                    let nodes = parse_frame(&txt)?;
                    publisher.publish(app, MonitorSnapshot {
                        backend_ok: true,
                        error: None,
                        nodes,
                        last_update_ms: now_ms(),
                    });
                }
            }
        }
    }
}

async fn http_loop(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let mut interval = tokio::time::interval(Duration::from_secs(s.poll_interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    log::info!("HTTP 轮询模式（极简探针）: {base}");
    clear_error(app);

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            _ = interval.tick() => {
                // One request for the whole fleet: `/api/nodes` carries every
                // node's latest report, served out of the hub's own 1.9s cache.
                let text = fetch_nodes_text(client, &base, "")
                    .await
                    .map_err(|e| e.explain(NEEDS_PUBLIC_PAGE))?;
                let nodes = parse_frame(&text)?;
                publisher.publish(app, MonitorSnapshot {
                    backend_ok: true,
                    error: None,
                    nodes,
                    last_update_ms: now_ms(),
                });
            }
        }
    }
}

/// `GET /api/nodes` as text, so the caller can parse it — or sniff which
/// backend answered.
///
/// `api_key` exists for that second use: a Komari site needs its Bearer header
/// to answer at all, and this hub ignores headers it does not authenticate by.
/// The 极简探针 session itself always passes an empty one.
pub async fn fetch_nodes_text(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<String, FetchError> {
    let mut req = client.get(format!("{base}/api/nodes"));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| FetchError::Other(format!("请求 /api/nodes 失败: {e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| FetchError::Other(e.to_string()))?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(FetchError::Unauthorized);
    }
    if !status.is_success() {
        return Err(FetchError::Other(format!(
            "请求 /api/nodes 失败: HTTP {status}"
        )));
    }
    Ok(text)
}

/// `GET /api/me` — used only for the site's own name in the connection test.
pub async fn fetch_site_name(client: &reqwest::Client, base: &str) -> Option<String> {
    let resp = client.get(format!("{base}/api/me")).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("site_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Ping records
// ---------------------------------------------------------------------------

/// `GET /api/nodes/{id}/metrics?series=ping`.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct PingBody {
    ping: Vec<PingRow>,
    /// Probe id -> display name, for every task assigned to the node.
    probes: BTreeMap<String, String>,
    /// Probe id -> percentage lost over the whole window. Only probes that lost
    /// something appear; a missing key means none did.
    loss: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct PingRow {
    task_id: u64,
    /// Epoch seconds.
    ts: u64,
    /// Median of the bucket, `null` when every probe in it was lost.
    latency: Option<f64>,
    /// Percentage lost within this bucket, absent when none were.
    loss: Option<f64>,
}

/// Fetch one node's ping window.
///
/// `points` is reported rather than requested: the hub only ever thins further,
/// and 60 over an hour lands on its one-minute grid, which is the finest it
/// stores. `series=ping` drops the resource half of the response.
pub async fn fetch_ping(
    client: &reqwest::Client,
    base: &str,
    uuid: &str,
    hours: u64,
) -> Option<(Vec<PingPoint>, PingSummary)> {
    let id = uuid.strip_prefix(UUID_PREFIX)?;
    let url = format!(
        "{base}/api/nodes/{id}/metrics?hours={hours}&points={}&series=ping",
        hours * 60
    );
    let resp = client.get(url).send().await.ok()?;
    // 503 is the hub's history gate refusing another concurrent scan rather
    // than an error; the caller keeps what it has and tries again next round.
    if !resp.status().is_success() {
        return None;
    }
    Some(parse_ping_body(&resp.text().await.ok()?))
}

fn parse_ping_body(text: &str) -> (Vec<PingPoint>, PingSummary) {
    let Ok(body) = serde_json::from_str::<PingBody>(text) else {
        return (Vec::new(), PingSummary::default());
    };
    let mut points: Vec<PingPoint> = body
        .ping
        .iter()
        .map(|r| PingPoint {
            t: r.ts * 1000,
            // The popover's chart reads a lost sample off the sign; the hub
            // says so with a null median instead.
            v: r.latency.unwrap_or(-1.0),
            task_id: r.task_id,
            loss: Some(match r.loss {
                Some(pct) => pct / 100.0,
                None if r.latency.is_none() => 1.0,
                None => 0.0,
            }),
        })
        .collect();
    points.sort_by_key(|p| p.t);
    (points, summarize(&body))
}

/// Latency is the mean of each probe's newest answer, matching how the Komari
/// path reads it. Loss comes from the hub's own window figures instead of the
/// samples: failed probes are not in `ping` at all, so counting them there
/// would report every node as lossless. Probes the hub assigned but left out of
/// `loss` lost nothing, and count as zero rather than being skipped.
fn summarize(body: &PingBody) -> PingSummary {
    let mut latest: BTreeMap<u64, &PingRow> = BTreeMap::new();
    for row in body.ping.iter().filter(|r| r.latency.is_some()) {
        let slot = latest.entry(row.task_id).or_insert(row);
        if row.ts >= slot.ts {
            *slot = row;
        }
    }
    let answered: Vec<f64> = latest.values().filter_map(|r| r.latency).collect();

    // `probes` is the roster; `ping` is the fallback for a hub that ever stops
    // sending one, so a node with records is never reported as loss-unknown.
    let mut tasks: Vec<String> = body.probes.keys().cloned().collect();
    if tasks.is_empty() {
        tasks = body.ping.iter().map(|r| r.task_id.to_string()).collect();
        tasks.sort();
        tasks.dedup();
    }

    PingSummary {
        latency: if answered.is_empty() {
            None
        } else {
            Some(answered.iter().sum::<f64>() / answered.len() as f64)
        },
        loss: if tasks.is_empty() {
            None
        } else {
            Some(
                tasks
                    .iter()
                    .map(|t| body.loss.get(t).copied().unwrap_or(0.0) / 100.0)
                    .sum::<f64>()
                    / tasks.len() as f64,
            )
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Two nodes trimmed out of a real `/api/nodes` response, keeping one
    /// online node and one offline node with `metrics: null`.
    fn sample_frame() -> &'static str {
        r#"{"admin":false,"nodes":[
          {"id":1,"name":"LAS C.Std.B - Pro","online":true,"country":"US",
           "os":"Debian GNU/Linux 13 (trixie)","expires_at":"2026-09-26",
           "traffic_limit":0,"traffic_mode":"sum","traffic_reset_day":26,
           "total_rx":2033943981,"total_tx":2404081446,
           "month_rx":2033029951,"month_tx":2403238915,"month_used":4436268866,
           "metrics":{"cpu":1.0025062561035156,"disk_total":31620823040,
             "disk_used":3142778880,"load":[0.0,0.04,0.04],
             "mem_total":4112384000,"mem_used":848678912,
             "net_rx":11505,"net_tx":53735,"procs":129,
             "swap_total":0,"swap_used":0,"tcp":17,"udp":6,"uptime":2318724}},
          {"id":9,"name":"BreadCloud LAX Bite","online":false,"country":"US",
           "os":"Ubuntu 24.04","expires_at":"2027-09-04",
           "traffic_limit":2147483648000,"traffic_mode":"sum",
           "total_rx":10,"total_tx":20,"month_used":30,"metrics":null}
        ]}"#
    }

    #[test]
    fn frame_maps_onto_the_shared_snapshot() {
        let nodes = parse_frame(sample_frame()).unwrap();
        assert_eq!(nodes.len(), 2);
        let n = nodes.iter().find(|n| n.uuid == "m:1").unwrap();

        assert_eq!(n.name, "LAS C.Std.B - Pro");
        assert!(n.online && n.has_metrics);
        // The flag lookup wants a two-letter code, and grouping is by country.
        assert_eq!(n.region, "US");
        assert_eq!(n.group, "US");
        assert!(n.tags.is_empty());
        assert_eq!(n.os, "Debian GNU/Linux 13 (trixie)");
        assert!((n.cpu_usage - 1.0025062561035156).abs() < 1e-9);
        assert_eq!(n.ram_used, 848_678_912);
        assert_eq!(n.disk_total, 31_620_823_040);
        // rx is inbound: the node's download, and tx its upload.
        assert!((n.net_up - 53_735.0).abs() < 1e-9);
        assert!((n.net_down - 11_505.0).abs() < 1e-9);
        assert_eq!(n.total_up, 2_404_081_446);
        assert_eq!(n.total_down, 2_033_943_981);
        assert_eq!(n.tcp, 17);
        assert_eq!(n.udp, 6);
        assert_eq!(n.uptime_secs, 2_318_724);
        assert_eq!(n.expired_at.as_deref(), Some("2026-09-26"));
    }

    #[test]
    fn quota_is_billed_against_the_month_not_the_lifetime_totals() {
        let nodes = parse_frame(sample_frame()).unwrap();
        let n = nodes.iter().find(|n| n.uuid == "m:9").unwrap();
        // The hub already applied `traffic_mode`; the lifetime totals (10 + 20)
        // are shown elsewhere but never billed, since the quota resets monthly.
        assert_eq!(n.traffic_used, 30);
        assert_eq!(n.traffic_limit, 2_147_483_648_000);
        assert_eq!(n.traffic_period, "本月");
        assert_eq!(n.total_up, 20);
        assert_eq!(n.total_down, 10);
    }

    #[test]
    fn offline_node_reports_no_metrics_instead_of_zeroes() {
        let nodes = parse_frame(sample_frame()).unwrap();
        let n = nodes.iter().find(|n| n.uuid == "m:9").unwrap();
        assert!(!n.online);
        assert!(!n.has_metrics);
        assert_eq!(n.cpu_usage, 0.0);
    }

    /// The state the hub's own docs warn about: connected, but the agent has
    /// not sent its first report. Rendering that as 0% CPU would drag the
    /// fleet average down every time a node reconnects.
    #[test]
    fn connected_but_silent_node_is_online_without_metrics() {
        let nodes =
            parse_frame(r#"{"nodes":[{"id":4,"name":"fresh","online":true,"metrics":null}]}"#)
                .unwrap();
        assert!(nodes[0].online);
        assert!(!nodes[0].has_metrics);
    }

    #[test]
    fn nameless_node_falls_back_to_its_id() {
        let nodes = parse_frame(r#"{"nodes":[{"id":7,"name":"  ","online":true}]}"#).unwrap();
        assert_eq!(nodes[0].name, "节点 7");
        assert_eq!(nodes[0].uuid, "m:7");
    }

    fn sample_ping() -> &'static str {
        r#"{
          "ping":[
            {"task_id":1,"ts":1790064060,"latency":1},
            {"task_id":3,"ts":1790064060,"latency":141},
            {"task_id":1,"ts":1790064120,"latency":2},
            {"task_id":3,"ts":1790064120,"latency":null},
            {"task_id":4,"ts":1790064120,"latency":130,"loss":25}
          ],
          "probes":{"1":"Cloudflare","3":"浙江联通","4":"浙江移动"},
          "loss":{"3":50.0}
        }"#
    }

    #[test]
    fn ping_rows_carry_their_bucket_loss() {
        let (points, _) = parse_ping_body(sample_ping());
        assert_eq!(points.len(), 5);
        // Epoch seconds on the wire, milliseconds everywhere in this app.
        assert_eq!(points[0].t, 1_790_064_060_000);
        // A null median means the whole bucket was lost; the chart reads that
        // off the sign, so it has to become a negative latency.
        let dead = points.iter().find(|p| p.task_id == 3 && p.v < 0.0).unwrap();
        assert_eq!(dead.v, -1.0);
        assert_eq!(dead.loss, Some(1.0));
        // A partially lost bucket still answered, and reports a percentage.
        let partial = points.iter().find(|p| p.task_id == 4).unwrap();
        assert!((partial.v - 130.0).abs() < 1e-9);
        assert_eq!(partial.loss, Some(0.25));
        // A clean bucket is an explicit zero, not an absent figure.
        assert_eq!(points[0].loss, Some(0.0));
    }

    #[test]
    fn ping_summary_uses_the_windows_own_loss_figures() {
        let (_, summary) = parse_ping_body(sample_ping());
        // Newest answered sample per probe: task 1 at 2ms, task 3 at 141ms
        // (its newer bucket was a total loss), task 4 at 130ms.
        let mean = (2.0 + 141.0 + 130.0) / 3.0;
        assert!((summary.latency.unwrap() - mean).abs() < 1e-9);
        // Percentages on the wire, a share here. Probes 1 and 4 are absent
        // from `loss`, which means they lost nothing — not that they are
        // unknown, so they count as zero across three probes.
        assert!((summary.loss.unwrap() - 0.5 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ping_summary_without_records_knows_nothing() {
        let (points, summary) = parse_ping_body(r#"{"ping":[],"probes":{},"loss":{}}"#);
        assert!(points.is_empty());
        assert_eq!(summary, PingSummary::default());
        // Malformed bodies are treated the same way rather than failing the
        // whole refresh round.
        assert_eq!(parse_ping_body("not json").1, PingSummary::default());
    }
}
