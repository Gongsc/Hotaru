use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Settings {
    pub backend_url: String,
    pub api_key: String,
    pub poll_interval_secs: u64,
    pub tray_mode: TrayMode,
    pub pinned_uuid: String,
    pub show_menu_bar_text: bool,
    pub cpu_warn_pct: f64,
    pub mem_warn_pct: f64,
    pub accept_invalid_certs: bool,
    pub chart_range_secs: u64,
    pub theme: ThemeMode,
    /// Node uuids kept out of the tray popover's list. The hidden set is stored
    /// rather than the visible one so a newly added node shows up by default and
    /// an empty list means "show everything". Display only — the tray icon,
    /// tooltip and aggregate chart still cover every node.
    pub hidden_nodes: Vec<String>,
    /// What the popover's collapsed node row shows after the name.
    pub node_badge: NodeBadge,
    /// Start into the tray only: the panel webview is still created at launch
    /// but stays hidden until asked for.
    pub silent_start: bool,
    pub chart_keep_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend_url: String::new(),
            api_key: String::new(),
            poll_interval_secs: 3,
            tray_mode: TrayMode::Aggregate,
            pinned_uuid: String::new(),
            show_menu_bar_text: true,
            cpu_warn_pct: 80.0,
            mem_warn_pct: 85.0,
            accept_invalid_certs: false,
            chart_range_secs: CHART_RANGE_DEFAULT,
            theme: ThemeMode::System,
            hidden_nodes: Vec::new(),
            node_badge: NodeBadge::Tags,
            silent_start: false,
            chart_keep_open: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// What the tray popover puts after a node's name in the collapsed row. The
/// node tags are only one of the choices; the others are live figures the
/// popover already has, so switching costs no extra request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NodeBadge {
    /// Komari 的节点标签
    #[default]
    Tags,
    /// 当前延迟与丢包率
    Ping,
    /// 当前网络速率
    Net,
    /// 距到期还剩多久
    Expiry,
}

/// Selectable display ranges (seconds) for the tray network chart.
pub const CHART_RANGES: &[u64] = &[300, 900, 3600, 21_600];
pub const CHART_RANGE_DEFAULT: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayMode {
    Aggregate,
    Node,
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.backend_url = self.backend_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.poll_interval_secs = self.poll_interval_secs.clamp(1, 60);
        self.cpu_warn_pct = self.cpu_warn_pct.clamp(1.0, 100.0);
        self.mem_warn_pct = self.mem_warn_pct.clamp(1.0, 100.0);
        if self.pinned_uuid.trim().is_empty() {
            self.tray_mode = TrayMode::Aggregate;
        }
        if !CHART_RANGES.contains(&self.chart_range_secs) {
            self.chart_range_secs = CHART_RANGE_DEFAULT;
        }
        let mut seen = std::collections::HashSet::new();
        self.hidden_nodes = std::mem::take(&mut self.hidden_nodes)
            .into_iter()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty() && seen.insert(u.clone()))
            .collect();
        self
    }
}

/// Komari keeps a node's tags in one `;`-separated string.
pub fn split_tags(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// One ping-monitor probe.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct PingPoint {
    /// Epoch milliseconds.
    pub t: u64,
    /// Latency in ms; negative means the probe was lost.
    pub v: f64,
    /// Ping task identity, used to average the latest result per target.
    pub task_id: u64,
}

/// Latest ping figures for one node, as a node card shows them.
#[derive(Serialize, Default, PartialEq, Debug)]
pub struct PingSummary {
    /// Mean of each task's most recent successful probe, in ms. `None` when no
    /// task has a successful probe in the window.
    pub latency: Option<f64>,
    /// Share of lost probes over the window, 0.0–1.0. `None` without records.
    pub loss: Option<f64>,
}

/// Latency is the mean of each task's newest successful probe — one slow target
/// should not be hidden by a fast one, and a single task's jitter should not
/// swing the figure. Loss is counted over the whole window instead, since a
/// per-task "latest" says nothing about how often probes go missing.
pub fn summarize_ping(points: &[PingPoint]) -> PingSummary {
    let mut latest: std::collections::BTreeMap<u64, &PingPoint> = std::collections::BTreeMap::new();
    for p in points {
        let slot = latest.entry(p.task_id).or_insert(p);
        if p.t >= slot.t {
            *slot = p;
        }
    }
    let ok: Vec<f64> = latest
        .values()
        .filter(|p| p.v.is_finite() && p.v >= 0.0)
        .map(|p| p.v)
        .collect();
    PingSummary {
        latency: if ok.is_empty() {
            None
        } else {
            Some(ok.iter().sum::<f64>() / ok.len() as f64)
        },
        loss: if points.is_empty() {
            None
        } else {
            Some(points.iter().filter(|p| p.v < 0.0).count() as f64 / points.len() as f64)
        },
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub uuid: String,
    pub name: String,
    pub online: bool,
    #[serde(default)]
    pub region: String,
    /// e.g. "windows" / "linux" / "darwin".
    #[serde(default)]
    pub os: String,
    /// Komari node group. A node belongs to at most one; empty when unset.
    #[serde(default)]
    pub group: String,
    /// Komari node tags, already split out of its `;`-separated string.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Latest ping figures, merged in from the monitor's ping cache so a node
    /// card can show them without waiting for a request of its own.
    #[serde(default)]
    pub latency: Option<f64>,
    #[serde(default)]
    pub loss: Option<f64>,
    pub cpu_usage: f64,
    pub ram_used: u64,
    pub ram_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    /// B/s
    pub net_up: f64,
    /// B/s
    pub net_down: f64,
    pub total_up: u64,
    pub total_down: u64,
    #[serde(default)]
    pub traffic_limit: u64,
    #[serde(default)]
    pub traffic_limit_type: String,
    #[serde(default)]
    pub expired_at: Option<String>,
    pub tcp: u64,
    pub udp: u64,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub backend_ok: bool,
    pub error: Option<String>,
    pub nodes: Vec<NodeSnapshot>,
    pub last_update_ms: u64,
}

impl MonitorSnapshot {
    pub fn offline(error: &str) -> Self {
        Self {
            backend_ok: false,
            error: Some(error.to_string()),
            nodes: Vec::new(),
            last_update_ms: now_ms(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Aggregate {
    pub online: usize,
    pub total: usize,
    pub cpu: f64,
    pub mem_pct: f64,
    pub ram_used: u64,
    pub ram_total: u64,
    pub net_up: f64,
    pub net_down: f64,
    pub total_up: u64,
    pub total_down: u64,
}

/// One sampled point of the aggregate network history (in-memory only).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct NetPoint {
    /// Epoch milliseconds.
    pub t: u64,
    /// B/s summed over all nodes.
    pub up: f64,
    /// B/s summed over all nodes.
    pub down: f64,
    /// Aggregate points are always `true`; per-node points carry the node's
    /// online state at sample time.
    pub online: bool,
}

/// One history frame: a timestamp plus every node's rates at that moment.
/// `(uuid, up B/s, down B/s, online)` per entry.
#[derive(Debug, Clone, PartialEq)]
pub struct NetFrame {
    /// Epoch milliseconds.
    pub t: u64,
    pub nodes: Vec<(String, f64, f64, bool)>,
}

/// Nodes covered by the current tray mode: all nodes, or the pinned one
/// (falling back to all when the pinned node vanished from the backend).
pub fn scoped_nodes<'a>(settings: &Settings, nodes: &'a [NodeSnapshot]) -> Vec<&'a NodeSnapshot> {
    match settings.tray_mode {
        TrayMode::Node => {
            let pinned: Vec<&NodeSnapshot> = nodes
                .iter()
                .filter(|n| n.uuid == settings.pinned_uuid)
                .collect();
            if pinned.is_empty() {
                nodes.iter().collect()
            } else {
                pinned
            }
        }
        TrayMode::Aggregate => nodes.iter().collect(),
    }
}

pub fn aggregate(nodes: &[&NodeSnapshot]) -> Aggregate {
    let total = nodes.len();
    let online: Vec<&&NodeSnapshot> = nodes.iter().filter(|n| n.online).collect();
    let count = online.len().max(1) as f64;
    let ram_used: u64 = online.iter().map(|n| n.ram_used).sum();
    let ram_total: u64 = online.iter().map(|n| n.ram_total).sum();
    Aggregate {
        online: online.len(),
        total,
        cpu: online.iter().map(|n| n.cpu_usage).sum::<f64>() / count,
        mem_pct: pct(ram_used, ram_total).unwrap_or(0.0),
        ram_used,
        ram_total,
        net_up: online.iter().map(|n| n.net_up).sum(),
        net_down: online.iter().map(|n| n.net_down).sum(),
        total_up: online.iter().map(|n| n.total_up).sum(),
        total_down: online.iter().map(|n| n.total_down).sum(),
    }
}

pub fn pct(used: u64, total: u64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(used as f64 / total as f64 * 100.0)
    }
}

// ---------------------------------------------------------------------------
// Severity for the tray icon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Err,
    Down,
}

#[derive(Debug, Clone, Copy)]
pub struct IconState {
    pub severity: Severity,
    /// Worst metric (CPU or memory) in percent, when available.
    pub gauge: Option<f64>,
    pub badge: bool,
}

pub fn icon_state(settings: &Settings, snap: &MonitorSnapshot) -> IconState {
    if !snap.backend_ok {
        return IconState {
            severity: Severity::Down,
            gauge: None,
            badge: false,
        };
    }
    let scope = scoped_nodes(settings, &snap.nodes);
    if scope.is_empty() {
        return IconState {
            severity: Severity::Down,
            gauge: None,
            badge: false,
        };
    }
    let offline = scope.iter().filter(|n| !n.online).count();
    let online: Vec<&&NodeSnapshot> = scope.iter().filter(|n| n.online).collect();
    if online.is_empty() {
        return IconState {
            severity: Severity::Err,
            gauge: None,
            badge: false,
        };
    }
    let mut gauge: Option<f64> = None;
    let mut severity = Severity::Ok;
    let err_threshold = |warn: f64| (warn + 15.0).min(100.0);
    for n in &online {
        for (value, warn) in [
            (n.cpu_usage, settings.cpu_warn_pct),
            (
                pct(n.ram_used, n.ram_total).unwrap_or(0.0),
                settings.mem_warn_pct,
            ),
        ] {
            let s = if value >= err_threshold(warn) {
                Severity::Err
            } else if value >= warn {
                Severity::Warn
            } else {
                Severity::Ok
            };
            if rank(s) > rank(severity) {
                severity = s;
            }
            if gauge.map_or(true, |g| value > g) {
                gauge = Some(value.clamp(0.0, 100.0));
            }
        }
    }
    let badge = match settings.tray_mode {
        TrayMode::Aggregate => offline > 0,
        TrayMode::Node => false,
    };
    IconState {
        severity,
        gauge,
        badge,
    }
}

fn rank(s: Severity) -> u8 {
    match s {
        Severity::Ok => 0,
        Severity::Warn => 1,
        Severity::Err => 2,
        Severity::Down => 3,
    }
}

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

/// Normalize a backend base URL: trim, strip trailing slashes, default scheme.
pub fn normalize_base(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("后端地址为空".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed: url::Url =
        url::Url::parse(&with_scheme).map_err(|_| format!("无法解析后端地址: {trimmed}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(with_scheme),
        _ => Err(format!("不支持的协议: {}", parsed.scheme())),
    }
}

/// `https://a.b:8080/x` -> `https://a.b:8080` (used for the WS Origin header).
pub fn origin_of(base: &str) -> String {
    let parsed = url::Url::parse(base);
    if let Ok(u) = parsed {
        let port = match (u.scheme(), u.port()) {
            ("http", None) => Some(80),
            ("https", None) => Some(443),
            (_, p) => p,
        };
        let host = u.host_str().unwrap_or("");
        match port {
            Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
            None => format!("{}://{}", u.scheme(), host),
        }
    } else {
        base.to_string()
    }
}

pub fn ws_url_of(base: &str) -> Result<String, String> {
    let base = normalize_base(base)?;
    let ws = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    Ok(format!("{ws}/api/clients"))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Formatting helpers (shared by tray, tooltip and settings UI)
// ---------------------------------------------------------------------------

pub fn fmt_rate(bps: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    if bps < 0.0 {
        "0B/s".into()
    } else if bps < KB {
        format!("{bps:.0}B/s")
    } else if bps < MB {
        format!("{:.1}KB/s", bps / KB)
    } else if bps < GB {
        format!("{:.2}MB/s", bps / MB)
    } else {
        format!("{:.2}GB/s", bps / GB)
    }
}

#[allow(dead_code)]
pub fn fmt_uptime(secs: u64) -> String {
    #[allow(clippy::manual_div_ceil)]
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}天{h}小时")
    } else if h > 0 {
        format!("{h}小时{m}分")
    } else {
        format!("{m}分钟")
    }
}

// ---------------------------------------------------------------------------
// Komari wire format (parsed payloads)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Envelope<T> {
    pub status: String,
    #[serde(default)]
    pub data: Option<T>,
    #[serde(default)]
    pub message: Option<String>,
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
        expired_at: info.expired_at.clone(),
        tcp: r.connections.as_ref().and_then(|c| c.tcp).unwrap_or(0),
        udp: r.connections.as_ref().and_then(|c| c.udp).unwrap_or(0),
        uptime_secs: r.uptime.unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_defaults_for_existing_settings_and_serializes() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.theme, ThemeMode::System);

        let mut dark = Settings::default();
        dark.theme = ThemeMode::Dark;
        let json = serde_json::to_value(&dark).unwrap();
        assert_eq!(json["theme"], "dark");
    }

    #[test]
    fn node_badge_defaults_for_settings_saved_before_the_field_existed() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.node_badge, NodeBadge::Tags);

        let net = Settings {
            node_badge: NodeBadge::Net,
            ..Settings::default()
        };
        let json = serde_json::to_value(&net).unwrap();
        assert_eq!(json["node_badge"], "net");
        let back: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(back.sanitized().node_badge, NodeBadge::Net);
    }

    #[test]
    fn normalize_base_variants() {
        assert_eq!(normalize_base(" https://a.com/ ").unwrap(), "https://a.com");
        assert_eq!(normalize_base("a.com").unwrap(), "https://a.com");
        assert_eq!(
            normalize_base("http://a.com:8080//").unwrap(),
            "http://a.com:8080"
        );
        assert!(normalize_base("").is_err());
        assert!(normalize_base("ftp://a.com").is_err());
    }

    #[test]
    fn origin_variants() {
        assert_eq!(origin_of("https://a.com"), "https://a.com:443");
        assert_eq!(origin_of("https://a.com:8443/x"), "https://a.com:8443");
        assert_eq!(origin_of("http://a.com"), "http://a.com:80");
        assert_eq!(origin_of("http://a.com:9000"), "http://a.com:9000");
    }

    #[test]
    fn ws_url_variants() {
        assert_eq!(
            ws_url_of("https://a.com").unwrap(),
            "wss://a.com/api/clients"
        );
        assert_eq!(ws_url_of("http://a.com").unwrap(), "ws://a.com/api/clients");
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_rate(512.0), "512B/s");
        assert_eq!(fmt_rate(2048.0), "2.0KB/s");
        assert_eq!(fmt_rate(3.4 * 1024.0 * 1024.0), "3.40MB/s");
        assert_eq!(fmt_uptime(3600 * 27), "1天3小时");
        assert_eq!(fmt_uptime(300), "5分钟");
    }

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
    fn aggregate_math() {
        let mut names = std::collections::HashMap::new();
        names.insert("uuid-1".to_string(), "node-1".to_string());
        let env: Envelope<WsPayload> = serde_json::from_str(&sample_ws_json()).unwrap();
        let payload = env.data.unwrap();
        let nodes: Vec<NodeSnapshot> = payload
            .data
            .iter()
            .map(|(uuid, rep)| {
                let online = payload.online.iter().any(|o| o == uuid);
                let info = ClientInfo {
                    uuid: uuid.clone(),
                    name: names.get(uuid).cloned().unwrap_or_default(),
                    ..Default::default()
                };
                report_to_snapshot(&info, online, rep)
            })
            .collect();
        let agg = aggregate(&nodes.iter().collect::<Vec<_>>());
        assert_eq!(agg.total, 2);
        assert_eq!(agg.online, 1);
        assert!((agg.cpu - 42.5).abs() < 1e-9);
        assert!((agg.mem_pct - 50.0).abs() < 1e-9);
        assert!((agg.net_down - 3567155.2).abs() < 1e-9);
        assert_eq!(agg.ram_total, 8589934592);
    }

    #[test]
    fn icon_severity_rules() {
        let mut s = Settings::default();
        s.cpu_warn_pct = 80.0;
        s.mem_warn_pct = 85.0;
        let snap = MonitorSnapshot {
            backend_ok: false,
            error: Some("x".into()),
            nodes: vec![],
            last_update_ms: 0,
        };
        assert_eq!(icon_state(&s, &snap).severity, Severity::Down);

        let snap = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![NodeSnapshot {
                cpu_usage: 42.0,
                ram_used: 4294967296,
                ram_total: 8589934592,
                online: true,
                ..NodeSnapshot::test_default()
            }],
            last_update_ms: 0,
        };
        let st = icon_state(&s, &snap);
        assert_eq!(st.severity, Severity::Ok);
        assert!((st.gauge.unwrap() - 50.0).abs() < 1e-9);
        assert!(!st.badge);

        // offline sibling triggers badge in aggregate mode
        let mut offline = NodeSnapshot::test_default();
        offline.online = false;
        let snap2 = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![snap.nodes[0].clone(), offline],
            last_update_ms: 0,
        };
        assert!(icon_state(&s, &snap2).badge);

        // cpu above warn -> Warn
        let mut high = snap.nodes[0].clone();
        high.cpu_usage = 88.0;
        let snap3 = MonitorSnapshot {
            nodes: vec![high],
            ..snap
        };
        assert_eq!(icon_state(&s, &snap3).severity, Severity::Warn);

        // node mode without badge
        let mut s_node = s.clone();
        s_node.tray_mode = TrayMode::Node;
        s_node.pinned_uuid = "pinned".into();
        let mut pinned = NodeSnapshot::test_default();
        pinned.uuid = "pinned".into();
        pinned.online = true;
        let other = NodeSnapshot {
            uuid: "other".into(),
            online: false,
            ..NodeSnapshot::test_default()
        };
        let snap4 = MonitorSnapshot {
            backend_ok: true,
            error: None,
            nodes: vec![pinned, other],
            last_update_ms: 0,
        };
        assert!(!icon_state(&s_node, &snap4).badge);
        assert_eq!(icon_state(&s_node, &snap4).severity, Severity::Ok);
    }

    #[test]
    fn scoped_nodes_fallback() {
        let mut s = Settings::default();
        s.tray_mode = TrayMode::Node;
        s.pinned_uuid = "missing".into();
        let nodes = vec![NodeSnapshot {
            uuid: "a".into(),
            ..NodeSnapshot::test_default()
        }];
        // pinned node vanished -> falls back to all nodes
        assert_eq!(scoped_nodes(&s, &nodes).len(), 1);
    }

    #[test]
    fn chart_range_sanitized() {
        // valid presets pass through
        for &r in CHART_RANGES {
            let mut s = Settings::default();
            s.chart_range_secs = r;
            assert_eq!(s.sanitized().chart_range_secs, r);
        }
        // out-of-preset values fall back to the default
        for bad in [0, 1, 299, 3601, 100_000, u64::MAX] {
            let mut s = Settings::default();
            s.chart_range_secs = bad;
            assert_eq!(s.sanitized().chart_range_secs, CHART_RANGE_DEFAULT);
        }
    }

    #[test]
    fn ping_summary_averages_the_latest_probe_per_task() {
        let p = |t, v, task_id| PingPoint { t, v, task_id };
        // Task 7 last succeeded at 40ms, task 9 at 60ms -> mean 50ms. The older
        // 10ms sample must not count, and the lost probe (-1) is 1 of 4 records.
        let points = [p(1, 10.0, 7), p(2, 40.0, 7), p(3, -1.0, 9), p(4, 60.0, 9)];
        let s = summarize_ping(&points);
        assert_eq!(s.latency, Some(50.0));
        assert_eq!(s.loss, Some(0.25));

        // Every probe lost: no latency to report, but the loss ratio is known.
        let lost = [p(1, -1.0, 7), p(2, -1.0, 9)];
        assert_eq!(
            summarize_ping(&lost),
            PingSummary {
                latency: None,
                loss: Some(1.0)
            }
        );

        // No records at all -> nothing known, so the node sorts last either way.
        assert_eq!(summarize_ping(&[]), PingSummary::default());
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
    fn hidden_nodes_are_trimmed_and_deduped_in_order() {
        let mut s = Settings::default();
        assert!(s.hidden_nodes.is_empty(), "everything visible by default");
        s.hidden_nodes = vec![
            " b ".into(),
            "a".into(),
            "b".into(),
            String::new(),
            "   ".into(),
            "a".into(),
        ];
        assert_eq!(s.sanitized().hidden_nodes, vec!["b", "a"]);
    }

    #[test]
    fn hidden_nodes_default_for_settings_saved_before_the_field_existed() {
        let s: Settings = serde_json::from_str(r#"{"backend_url":"https://x.example"}"#).unwrap();
        assert!(s.hidden_nodes.is_empty());
        // Silent start is opt-in, so an older config still opens the panel.
        assert!(!s.silent_start);
    }

    impl NodeSnapshot {
        fn test_default() -> Self {
            Self {
                uuid: "u".into(),
                name: "n".into(),
                region: String::new(),
                os: String::new(),
                group: String::new(),
                tags: Vec::new(),
                latency: None,
                loss: None,
                online: true,
                cpu_usage: 0.0,
                ram_used: 0,
                ram_total: 0,
                swap_used: 0,
                swap_total: 0,
                disk_used: 0,
                disk_total: 0,
                net_up: 0.0,
                net_down: 0.0,
                total_up: 0,
                total_down: 0,
                traffic_limit: 0,
                traffic_limit_type: String::new(),
                expired_at: None,
                tcp: 0,
                udp: 0,
                uptime_secs: 0,
            }
        }
    }
}
