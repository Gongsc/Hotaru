//! Komari (`komari-monitor/komari`).
//!
//! Live data arrives over a *pull* WebSocket: the client sends `get` and reads
//! one snapshot back. `/api/nodes` carries only the static node list, so the
//! HTTP fallback needs one `/api/recent/{uuid}` request per node.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tauri::AppHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::engine::{clear_error, Publisher, NODES_REFRESH, WS_READ_TIMEOUT};
use crate::models::{
    normalize_base, now_ms, origin_of, ws_url_of, MonitorSnapshot, NodeSnapshot, PingPoint,
    PingSummary, Settings,
};

/// Which period `NodeSnapshot::traffic_used` covers here.
const TRAFFIC_PERIOD: &str = "累计";

/// How many `/api/recent/{uuid}` requests are in flight at once. Komari has no
/// endpoint that returns every node's latest report over HTTP, so this path
/// needs one request per node; issuing them sequentially made a refresh take
/// node_count × round-trip, which overruns the poll interval well before a
/// large fleet is reached.
const RECENT_CONCURRENCY: usize = 8;

#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub status: String,
    #[serde(default)]
    pub data: Option<T>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Komari keeps a node's tags in one `;`-separated string.
pub fn split_tags(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// `GET /api/nodes` list item — only the fields we display; tolerant to
/// snake_case and camelCase spellings across Komari versions.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[allow(dead_code)]
pub struct ClientInfo {
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub region: String,
    #[serde(default, alias = "memTotal")]
    pub mem_total: u64,
    #[serde(default, alias = "osAlias", alias = "osName")]
    pub os: String,
    /// A node's group, unlike its tags, is a single plain name.
    #[serde(default)]
    pub group: String,
    /// `;`-separated in Komari's API; see [`split_tags`].
    #[serde(default)]
    pub tags: String,
    #[serde(default, alias = "trafficLimit")]
    pub traffic_limit: u64,
    #[serde(default, alias = "trafficLimitType")]
    pub traffic_limit_type: String,
    #[serde(default, alias = "expiredAt")]
    pub expired_at: Option<String>,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            uuid: String::new(),
            name: String::new(),
            region: String::new(),
            mem_total: 0,
            os: String::new(),
            group: String::new(),
            tags: String::new(),
            traffic_limit: 0,
            traffic_limit_type: String::new(),
            expired_at: None,
        }
    }
}

/// WebSocket `{"status":"success","data":{"online":[...],"data":{...}}}`.
#[derive(Debug, Deserialize, Default)]
pub struct WsPayload {
    #[serde(default)]
    pub online: Vec<String>,
    #[serde(default)]
    pub data: std::collections::HashMap<String, Report>,
}

/// Live report (`/api/recent/{uuid}` item and `/api/clients` WS value).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Report {
    pub cpu: Option<Usage>,
    pub ram: Option<Mem>,
    pub swap: Option<Mem>,
    pub disk: Option<Mem>,
    pub network: Option<NetStat>,
    pub connections: Option<Conns>,
    #[serde(alias = "uptime")]
    pub uptime: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Usage {
    pub usage: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Mem {
    pub total: Option<u64>,
    pub used: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct NetStat {
    pub up: Option<f64>,
    pub down: Option<f64>,
    #[serde(default, alias = "totalUp")]
    pub total_up: Option<u64>,
    #[serde(default, alias = "totalDown")]
    pub total_down: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Conns {
    pub tcp: Option<u64>,
    pub udp: Option<u64>,
}

pub fn report_to_snapshot(info: &ClientInfo, online: bool, r: &Report) -> NodeSnapshot {
    NodeSnapshot {
        uuid: info.uuid.clone(),
        name: if info.name.is_empty() {
            format!("节点 {}", &info.uuid[..info.uuid.len().min(8)])
        } else {
            info.name.clone()
        },
        online,
        region: info.region.clone(),
        os: info.os.clone(),
        group: info.group.clone(),
        tags: split_tags(&info.tags),
        latency: None,
        loss: None,
        cpu_usage: r.cpu.as_ref().and_then(|c| c.usage).unwrap_or(0.0),
        ram_used: r.ram.as_ref().and_then(|m| m.used).unwrap_or(0),
        ram_total: r.ram.as_ref().and_then(|m| m.total).unwrap_or(0),
        swap_used: r.swap.as_ref().and_then(|m| m.used).unwrap_or(0),
        swap_total: r.swap.as_ref().and_then(|m| m.total).unwrap_or(0),
        disk_used: r.disk.as_ref().and_then(|m| m.used).unwrap_or(0),
        disk_total: r.disk.as_ref().and_then(|m| m.total).unwrap_or(0),
        net_up: r.network.as_ref().and_then(|n| n.up).unwrap_or(0.0),
        net_down: r.network.as_ref().and_then(|n| n.down).unwrap_or(0.0),
        total_up: r.network.as_ref().and_then(|n| n.total_up).unwrap_or(0),
        total_down: r.network.as_ref().and_then(|n| n.total_down).unwrap_or(0),
        traffic_limit: info.traffic_limit,
        traffic_limit_type: info.traffic_limit_type.clone(),
        traffic_used: traffic_used(
            &info.traffic_limit_type,
            r.network.as_ref().and_then(|n| n.total_up).unwrap_or(0),
            r.network.as_ref().and_then(|n| n.total_down).unwrap_or(0),
        ),
        traffic_period: TRAFFIC_PERIOD.to_string(),
        expired_at: info.expired_at.clone(),
        tcp: r.connections.as_ref().and_then(|c| c.tcp).unwrap_or(0),
        udp: r.connections.as_ref().and_then(|c| c.udp).unwrap_or(0),
        uptime_secs: r.uptime.unwrap_or(0),
        // Komari has no "connected but silent" state to report.
        has_metrics: true,
    }
}

/// Komari's quota runs against the lifetime counters it keeps per node, picked
/// by the node's billing mode. Unknown modes bill the larger direction, which
/// is what the popover did before this moved out of JavaScript.
pub fn traffic_used(limit_type: &str, total_up: u64, total_down: u64) -> u64 {
    match limit_type.to_ascii_lowercase().as_str() {
        "sum" => total_up.saturating_add(total_down),
        "min" => total_up.min(total_down),
        "up" => total_up,
        "down" => total_down,
        _ => total_up.max(total_down),
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Run a Komari session until the configuration changes (`Ok`) or the
/// connection fails (`Err`).
pub async fn run(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    // Komari's legacy `/api/clients` WebSocket accepts a Bearer API Key
    // during the handshake, but its hidden-node filtering only checks a
    // browser `session_token` cookie. Consequently hidden nodes are
    // omitted from the live payload and appear as empty/offline here.
    // With an API Key, use the authenticated REST path directly: both
    // `/api/nodes` and `/api/recent/{uuid}` receive the Bearer header.
    if requires_authenticated_http(s) {
        log::info!("已配置 API Key，使用认证 HTTP 轮询获取完整节点信息");
    } else {
        match ws_session(app, client, s, rx, publisher).await {
            Ok(()) => return Ok(()), // settings changed -> reconnect with new config
            Err(e) => {
                crate::engine::set_error(app, &format!("实时连接不可用（{e}），已回退 HTTP 轮询"))
            }
        }
    }
    http_loop(app, client, s, rx, publisher).await
}

fn requires_authenticated_http(s: &Settings) -> bool {
    !s.api_key.is_empty()
}

// ---------------------------------------------------------------------------
// WebSocket live session (pull model: send "get", read one snapshot)
// ---------------------------------------------------------------------------

async fn ws_session(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let url = ws_url_of(&s.backend_url)?;
    let mut request = url
        .clone()
        .into_client_request()
        .map_err(|e| format!("构造 WS 请求失败: {e}"))?;
    {
        let headers = request.headers_mut();
        if !s.api_key.is_empty() {
            let value = HeaderValue::from_str(&format!("Bearer {}", s.api_key))
                .map_err(|_| "API Key 含非法字符".to_string())?;
            headers.insert("authorization", value);
        }
        let origin = HeaderValue::from_str(&origin_of(&base))
            .map_err(|_| "后端地址含非法字符".to_string())?;
        headers.insert("origin", origin);
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("连接失败: {e}"))?;
    let (mut write, mut read) = ws.split();

    let mut nodes = fetch_nodes(client, &base, &s.api_key)
        .await
        .unwrap_or_default();
    let mut last_nodes = Instant::now();
    let mut interval = tokio::time::interval(Duration::from_secs(s.poll_interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    clear_error(app);
    log::info!("WS 已连接: {url}");

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            _ = interval.tick() => {
                if last_nodes.elapsed() >= NODES_REFRESH {
                    if let Ok(n) = fetch_nodes(client, &base, &s.api_key).await {
                        nodes = n;
                    }
                    last_nodes = Instant::now();
                }
                if write.send(Message::Text("get".into())).await.is_err() {
                    return Err("发送 WS 消息失败".into());
                }
                let msg = match tokio::time::timeout(WS_READ_TIMEOUT, read.next()).await {
                    Ok(Some(Ok(m))) => m,
                    Ok(Some(Err(e))) => return Err(format!("读取失败: {e}")),
                    Ok(None) => return Err("连接已被服务端关闭".into()),
                    Err(_) => return Err("读取超时".into()),
                };
                if let Message::Text(txt) = msg {
                    if let Some(snap) = parse_ws_text(&txt, &nodes) {
                        publisher.publish(app, snap);
                    }
                }
            }
        }
    }
}

pub fn parse_ws_text(txt: &str, nodes: &HashMap<String, ClientInfo>) -> Option<MonitorSnapshot> {
    let env: Envelope<WsPayload> = serde_json::from_str(txt).ok()?;
    let payload = env.data?;
    // 以已知节点列表为准:未出现在实时推送里的节点(如 HTTP 上报的
    // 路由器设备)按离线展示,而不是直接消失。
    let mut out: Vec<NodeSnapshot> = nodes
        .values()
        .map(|info| {
            let online = payload.online.iter().any(|o| o == &info.uuid);
            match payload.data.get(&info.uuid) {
                Some(rep) => report_to_snapshot(info, online, rep),
                None => report_to_snapshot(info, false, &Report::default()),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Some(MonitorSnapshot {
        backend_ok: true,
        error: None,
        nodes: out,
        last_update_ms: now_ms(),
    })
}

// ---------------------------------------------------------------------------
// HTTP polling fallback: /api/nodes + /api/recent/{uuid}
// ---------------------------------------------------------------------------

async fn http_loop(
    app: &AppHandle,
    client: &reqwest::Client,
    s: &Settings,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    publisher: &mut Publisher,
) -> Result<(), String> {
    let base = normalize_base(&s.backend_url)?;
    let mut nodes = fetch_nodes(client, &base, &s.api_key).await?;
    let mut last_nodes = Instant::now();
    // `interval` fires its first tick immediately, so a freshly (re)started loop
    // publishes right away instead of showing stale data for a whole poll
    // interval — which is what a `sleep` at the top of the loop used to do.
    let mut interval = tokio::time::interval(Duration::from_secs(s.poll_interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    log::info!("HTTP 轮询模式: {base}");
    clear_error(app);

    loop {
        tokio::select! {
            _ = rx.changed() => return Ok(()),
            _ = interval.tick() => {
                if last_nodes.elapsed() >= NODES_REFRESH {
                    nodes = fetch_nodes(client, &base, &s.api_key).await?;
                    last_nodes = Instant::now();
                }
                // The stream yields owned uuids and the block moves only Copy
                // references: a closure taking borrowed tuple items would have to
                // be generic over their lifetimes, which this call cannot express.
                let uuids: Vec<String> = nodes.keys().cloned().collect();
                let (known, base_url, key) = (&nodes, base.as_str(), s.api_key.as_str());
                let mut out: Vec<NodeSnapshot> = futures_util::stream::iter(uuids)
                    .map(|uuid| async move {
                        fetch_recent_snapshot(client, base_url, key, &uuid, &known[&uuid]).await
                    })
                    // Order does not matter, the list is sorted by name below.
                    .buffer_unordered(RECENT_CONCURRENCY)
                    .collect()
                    .await;
                out.sort_by(|a, b| a.name.cmp(&b.name));
                publisher.publish(
                    app,
                    MonitorSnapshot { backend_ok: true, error: None, nodes: out, last_update_ms: now_ms() },
                );
            }
        }
    }
}

fn offline_snapshot(info: &ClientInfo) -> NodeSnapshot {
    report_to_snapshot(info, false, &Report::default())
}

/// One node's latest report. Written as a function rather than an inline async
/// block so its return type stays generic over the borrowed arguments' lifetimes
/// — `buffer_unordered` needs that. Anything unexpected counts as offline: a
/// single unreachable node must not fail the whole refresh.
async fn fetch_recent_snapshot(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    uuid: &str,
    info: &ClientInfo,
) -> NodeSnapshot {
    let mut req = client.get(format!("{base}/api/recent/{uuid}"));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let Ok(resp) = req.send().await else {
        return offline_snapshot(info);
    };
    if !resp.status().is_success() {
        return offline_snapshot(info);
    }
    match resp.json::<Envelope<Vec<Report>>>().await {
        Ok(env) => match env.data {
            Some(reports) if !reports.is_empty() => {
                report_to_snapshot(info, true, reports.last().unwrap())
            }
            _ => offline_snapshot(info),
        },
        Err(_) => offline_snapshot(info),
    }
}

pub async fn fetch_nodes(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
) -> Result<HashMap<String, ClientInfo>, String> {
    let url = format!("{base}/api/nodes");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("请求 /api/nodes 失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("后端返回 401：站点为私有模式，请在设置中填写 API Key".into());
    }
    if !status.is_success() {
        return Err(format!("请求 /api/nodes 失败: HTTP {status}"));
    }
    parse_nodes(&text)
}

/// Shared by the engine and the settings window's picker, which reads the same
/// list through unsaved form values.
pub fn parse_nodes(text: &str) -> Result<HashMap<String, ClientInfo>, String> {
    let env: Envelope<Vec<ClientInfo>> =
        serde_json::from_str(text).map_err(|e| format!("解析 /api/nodes 失败: {e}"))?;
    if env.status != "success" {
        return Err(format!(
            "后端返回错误: {}{}",
            env.status,
            env.message.map(|m| format!("（{m}）")).unwrap_or_default()
        ));
    }
    let data = env.data.ok_or("响应缺少 data 字段")?;
    Ok(data
        .into_iter()
        .filter(|c| !c.uuid.is_empty())
        .map(|c| (c.uuid.clone(), c))
        .collect())
}

// ---------------------------------------------------------------------------
// Ping records
// ---------------------------------------------------------------------------

/// Fetch one node's ping records. `None` on any failure — a node the backend
/// has no ping task for simply has no data, which is not an error.
pub async fn fetch_ping(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    uuid: &str,
    hours: u64,
) -> Option<(Vec<PingPoint>, PingSummary)> {
    let mut req = client.get(format!("{base}/api/records/ping?uuid={uuid}&hours={hours}"));
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let points = parse_ping_records(&body);
    let summary = crate::models::summarize_ping(&points);
    Some((points, summary))
}

pub fn parse_ping_records(body: &serde_json::Value) -> Vec<PingPoint> {
    let mut out = Vec::new();
    if let Some(records) = body.pointer("/data/records").and_then(|v| v.as_array()) {
        for r in records {
            let Some(v) = r.get("value").and_then(|x| x.as_f64()) else {
                continue;
            };
            let task_id = r.get("task_id").and_then(|x| x.as_u64()).unwrap_or(0);
            let time = r.get("time").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(t) = crate::models::parse_rfc3339_ms(time) {
                // One record is one probe here, so it either answered or it did
                // not; `summarize_ping` reads the same thing off the sign.
                out.push(PingPoint {
                    t,
                    v,
                    task_id,
                    loss: Some(if v < 0.0 { 1.0 } else { 0.0 }),
                });
            }
        }
    }
    out.sort_by_key(|p| p.t);
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ws_json() -> String {
        r#"{
          "status": "success",
          "data": {
            "online": ["uuid-1"],
            "data": {
              "uuid-1": {
                "cpu": {"usage": 42.5},
                "ram": {"total": 8589934592, "used": 4294967296},
                "swap": {"total": 1073741824, "used": 0},
                "disk": {"total": 107374182400, "used": 53687091200},
                "network": {"up": 128.0, "down": 3567155.2, "totalUp": 1024, "totalDown": 2048},
                "connections": {"tcp": 120, "udp": 8},
                "uptime": 98765
              },
              "uuid-2": {
                "cpu": {"usage": 91.0},
                "ram": {"total": 4294967296, "used": 2147483648}
              }
            }
          }
        }"#
        .to_string()
    }
    #[test]
    fn parse_ws_payload() {
        let env: Envelope<WsPayload> = serde_json::from_str(&sample_ws_json()).unwrap();
        let payload = env.data.unwrap();
        assert_eq!(payload.online, vec!["uuid-1".to_string()]);
        assert_eq!(payload.data.len(), 2);
        let rep = payload.data.get("uuid-1").unwrap();
        let info = ClientInfo {
            uuid: "uuid-1".into(),
            name: "node-1".into(),
            traffic_limit: 1_000_000,
            traffic_limit_type: "sum".into(),
            expired_at: Some("2027-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        let snap = report_to_snapshot(&info, true, rep);
        assert_eq!(snap.name, "node-1");
        assert!((snap.cpu_usage - 42.5).abs() < 1e-9);
        assert_eq!(snap.ram_used, 4294967296);
        assert!((snap.net_down - 3567155.2).abs() < 1e-9);
        assert_eq!(snap.tcp, 120);
        assert_eq!(snap.traffic_limit, 1_000_000);
        assert_eq!(snap.traffic_limit_type, "sum");
        assert_eq!(snap.expired_at.as_deref(), Some("2027-01-01T00:00:00Z"));
        // unknown node gets a fallback name
        let rep2 = payload.data.get("uuid-2").unwrap();
        let info2 = ClientInfo {
            uuid: "uuid-2".into(),
            ..Default::default()
        };
        let snap2 = report_to_snapshot(&info2, false, rep2);
        assert_eq!(snap2.name, "节点 uuid-2");
        assert_eq!(snap2.uptime_secs, 0);
    }
    #[test]
    fn client_info_parses_billing_metadata() {
        let info: ClientInfo = serde_json::from_str(
            r#"{
          "uuid":"node-1",
          "name":"Tokyo",
          "traffic_limit":1099511627776,
          "traffic_limit_type":"sum",
          "expired_at":"2027-06-30T00:00:00Z"
        }"#,
        )
        .unwrap();
        assert_eq!(info.traffic_limit, 1_099_511_627_776);
        assert_eq!(info.traffic_limit_type, "sum");
        assert_eq!(info.expired_at.as_deref(), Some("2027-06-30T00:00:00Z"));
    }
    #[test]
    fn group_reaches_the_snapshot_and_is_optional() {
        let info: ClientInfo =
            serde_json::from_str(r#"{"uuid":"u","group":"\u751f\u4ea7"}"#).unwrap();
        assert_eq!(info.group, "生产");
        let snap = report_to_snapshot(&info, true, &Report::default());
        assert_eq!(snap.group, "生产");

        // Komari versions without groups simply leave the node ungrouped.
        let plain: ClientInfo = serde_json::from_str(r#"{"uuid":"u"}"#).unwrap();
        assert!(report_to_snapshot(&plain, true, &Report::default())
            .group
            .is_empty());
    }
    #[test]
    fn tags_split_on_semicolons() {
        assert_eq!(split_tags("hk;bgp"), vec!["hk", "bgp"]);
        // Komari lets the field be blank, ragged or padded.
        assert_eq!(split_tags(" hk ; ; bgp;"), vec!["hk", "bgp"]);
        assert!(split_tags("").is_empty());
        assert!(split_tags(";;").is_empty());
        // commas are not separators, they stay inside one tag
        assert_eq!(split_tags("hk,bgp"), vec!["hk,bgp"]);
    }
    #[test]
    fn api_key_uses_authenticated_http_transport() {
        let mut settings = Settings::default();
        assert!(!requires_authenticated_http(&settings));

        settings.api_key = "secret".into();
        assert!(requires_authenticated_http(&settings));
    }

    #[test]
    fn ws_parse_keeps_nodes_without_live_data() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "uuid-1".to_string(),
            ClientInfo {
                uuid: "uuid-1".into(),
                name: "A".into(),
                ..Default::default()
            },
        );
        nodes.insert(
            "uuid-2".to_string(),
            ClientInfo {
                uuid: "uuid-2".into(),
                name: "B".into(),
                ..Default::default()
            },
        );
        nodes.insert(
            "uuid-3".to_string(),
            ClientInfo {
                uuid: "uuid-3".into(),
                name: "C".into(),
                ..Default::default()
            },
        );
        let txt = r#"{"status":"success","data":{"data":{"uuid-1":{"cpu":{"usage":10}}},"online":["uuid-1"]}}"#;
        let snap = parse_ws_text(txt, &nodes).expect("parse ok");
        assert_eq!(snap.nodes.len(), 3);
        let a = snap.nodes.iter().find(|n| n.uuid == "uuid-1").unwrap();
        assert!(a.online);
        assert!((a.cpu_usage - 10.0).abs() < 1e-9);
        let c = snap.nodes.iter().find(|n| n.uuid == "uuid-3").unwrap();
        assert!(!c.online);
        assert_eq!(c.cpu_usage, 0.0);
    }

    #[test]
    fn ping_records_preserve_task_identity_and_skip_invalid_values() {
        let body = serde_json::json!({
            "data": { "records": [
                { "task_id": 7, "time": "2026-08-29T16:50:00Z", "value": 42.5 },
                { "task_id": 9, "time": "2026-08-29T16:51:00Z", "value": 1 },
                { "task_id": 7, "time": "2026-08-29T16:52:00Z" }
            ] }
        });
        let points = parse_ping_records(&body);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].task_id, 7);
        assert_eq!(points[0].v, 42.5);
        assert_eq!(points[1].task_id, 9);
        assert_eq!(points[1].v, 1.0);
        // A probe that answered is not a loss, and one that did not is a whole one.
        assert_eq!(points[0].loss, Some(0.0));
        assert_eq!(
            parse_ping_records(&serde_json::json!({
                "data": { "records": [
                    { "task_id": 7, "time": "2026-08-29T16:50:00Z", "value": -1 }
                ] }
            }))[0]
                .loss,
            Some(1.0)
        );
    }

    /// The quota rule the popover used to apply in JavaScript, byte for byte.
    #[test]
    fn traffic_used_follows_the_billing_mode() {
        assert_eq!(traffic_used("sum", 30, 70), 100);
        assert_eq!(traffic_used("max", 30, 70), 70);
        assert_eq!(traffic_used("min", 30, 70), 30);
        assert_eq!(traffic_used("up", 30, 70), 30);
        assert_eq!(traffic_used("down", 30, 70), 70);
        // Unset or unrecognised bills the larger direction, as before.
        assert_eq!(traffic_used("", 30, 70), 70);
        assert_eq!(traffic_used("SUM", 30, 70), 100);
        // A node whose counters would overflow the sum must not wrap.
        assert_eq!(traffic_used("sum", u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn snapshot_reports_lifetime_traffic_against_the_quota() {
        let info = ClientInfo {
            uuid: "u".into(),
            traffic_limit: 1_000,
            traffic_limit_type: "sum".into(),
            ..Default::default()
        };
        let report = Report {
            network: Some(NetStat {
                total_up: Some(40),
                total_down: Some(60),
                ..Default::default()
            }),
            ..Default::default()
        };
        let snap = report_to_snapshot(&info, true, &report);
        assert_eq!(snap.traffic_used, 100);
        assert_eq!(snap.traffic_period, "累计");
        assert!(snap.has_metrics);
    }
}
