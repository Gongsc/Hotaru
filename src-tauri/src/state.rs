use std::collections::VecDeque;

use parking_lot::{Mutex, RwLock};
use tauri::AppHandle;
use tokio::sync::watch;

use crate::models::{MonitorSnapshot, NetFrame, PingData, Settings};
use crate::providers::Provider;

/// How long network history is kept in memory. Independent of the
/// configured display range so switching ranges never has to re-accumulate.
pub const HISTORY_RETENTION_MS: u64 = 6 * 3600 * 1000;

/// In-memory network history; lives only for the current process run.
/// Retention is independent of the configured display range so switching
/// ranges never has to re-accumulate.
#[derive(Default)]
pub struct NetHistory(Mutex<VecDeque<NetFrame>>);

/// Absolute safety cap on stored points.
const HISTORY_MAX_POINTS: usize = 50_000;

impl NetHistory {
    pub fn push(&self, frame: NetFrame) {
        let mut hist = self.0.lock();
        if hist.back().is_some_and(|last| frame.t < last.t) {
            return; // stale sample (clock went backwards); keep timeline monotonic
        }
        hist.push_back(frame);
        let cutoff = hist.back().unwrap().t.saturating_sub(HISTORY_RETENTION_MS);
        while hist.front().is_some_and(|p| p.t < cutoff) {
            hist.pop_front();
        }
        while hist.len() > HISTORY_MAX_POINTS {
            hist.pop_front();
        }
    }

    pub fn since(&self, since_ms: u64) -> Vec<NetFrame> {
        self.0
            .lock()
            .iter()
            .filter(|p| p.t >= since_ms)
            .cloned()
            .collect()
    }

    /// Drop everything. Used when the configured backend changes: the stored
    /// frames belong to the old site's nodes and would otherwise keep showing
    /// up in the popover's charts for hours.
    pub fn clear(&self) {
        self.0.lock().clear();
    }
}

/// Shared application state: settings, latest monitor snapshot, in-memory
/// network history and a config epoch used to wake the monitor engine after
/// settings changes.
pub struct AppState {
    pub settings: RwLock<Settings>,
    pub snapshot: RwLock<MonitorSnapshot>,
    pub config_epoch_tx: watch::Sender<u64>,
    /// Backend URL the "main" webview window is currently showing.
    pub loaded_panel_url: Mutex<Option<String>>,
    /// When the chart popover last auto-hid on focus loss (toggle guard).
    pub chart_hidden_at: Mutex<Option<std::time::Instant>>,
    /// Chart popover "pinned" (keep open on blur, keep dragged position).
    pub chart_pinned: std::sync::atomic::AtomicBool,
    /// Whether the popover was last placed below the tray icon (top edge
    /// anchored) rather than above it (bottom edge anchored). Resizes grow
    /// away from that edge so an expanded node card cannot push the popover
    /// off-screen.
    pub chart_below: std::sync::atomic::AtomicBool,
    /// Epoch ms of the panel webview's last finished page load (watchdog).
    pub panel_load_ms: std::sync::atomic::AtomicU64,
    pub panel_load_started_ms: std::sync::atomic::AtomicU64,
    pub panel_reload_ms: std::sync::atomic::AtomicU64,
    pub panel_recreate_streak: std::sync::atomic::AtomicU64,
    pub panel_epoch: std::sync::atomic::AtomicU64,
    /// Aggregate network samples; lives only for the current process run.
    pub net_history: NetHistory,
    /// Ping records per node uuid, refreshed by the engine's ping loop so the
    /// popover gets latency and loss from memory like every other figure.
    pub ping_records: RwLock<std::collections::BTreeMap<String, PingData>>,
    /// Which backend the engine last identified at `backend_url`. `None` until
    /// it has connected once, which is also what the ping loop waits on: it
    /// cannot know which endpoint to ask before then.
    pub provider: RwLock<Option<Provider>>,
}

impl AppState {
    pub fn bump_config_epoch(&self) {
        let next = *self.config_epoch_tx.borrow() + 1;
        let _ = self.config_epoch_tx.send(next);
    }
}

pub fn init(app: &AppHandle) -> AppState {
    let settings = crate::settings::load(app);
    let (tx, _rx) = watch::channel(0u64);
    AppState {
        settings: RwLock::new(settings),
        snapshot: RwLock::new(MonitorSnapshot::offline("正在连接后端…")),
        config_epoch_tx: tx,
        loaded_panel_url: Mutex::new(None),
        chart_hidden_at: Mutex::new(None),
        chart_pinned: std::sync::atomic::AtomicBool::new(false),
        // macOS hangs the popover under the menu bar, Windows above the
        // taskbar. Overwritten the first time the popover is positioned.
        chart_below: std::sync::atomic::AtomicBool::new(cfg!(target_os = "macos")),
        panel_load_ms: std::sync::atomic::AtomicU64::new(0),
        panel_load_started_ms: std::sync::atomic::AtomicU64::new(0),
        panel_reload_ms: std::sync::atomic::AtomicU64::new(0),
        panel_recreate_streak: std::sync::atomic::AtomicU64::new(0),
        panel_epoch: std::sync::atomic::AtomicU64::new(1),
        net_history: NetHistory::default(),
        ping_records: RwLock::new(std::collections::BTreeMap::new()),
        provider: RwLock::new(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: u64, up: f64) -> NetFrame {
        NetFrame {
            t,
            nodes: vec![("a".into(), up, up * 2.0, true)],
        }
    }

    #[test]
    fn history_trims_by_retention() {
        let hist = NetHistory::default();
        for i in 0..10u64 {
            hist.push(frame(i * 1000, 1.0));
        }
        assert_eq!(hist.since(0).len(), 10);
        assert_eq!(hist.since(4000).len(), 6);
        // a much newer sample evicts everything strictly older than the
        // retention window (cutoff is exclusive: t == cutoff survives)
        hist.push(frame(HISTORY_RETENTION_MS + 9000, 2.0));
        assert_eq!(hist.since(0).len(), 2);
        hist.push(frame(2 * HISTORY_RETENTION_MS + 10_000, 2.0));
        assert_eq!(hist.since(0).len(), 1);
    }

    #[test]
    fn history_rejects_backwards_time() {
        let hist = NetHistory::default();
        hist.push(frame(2000, 1.0));
        hist.push(frame(1000, 5.0));
        let pts = hist.since(0);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].t, 2000);
    }

    #[test]
    fn history_keeps_per_node_entries() {
        let hist = NetHistory::default();
        hist.push(NetFrame {
            t: 1000,
            nodes: vec![("a".into(), 1.0, 2.0, true), ("b".into(), 3.0, 4.0, false)],
        });
        hist.push(NetFrame {
            t: 2000,
            nodes: vec![("a".into(), 5.0, 6.0, true)],
        });
        let frames = hist.since(0);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].nodes.len(), 2);
        assert_eq!(frames[1].nodes.len(), 1);
    }
}
