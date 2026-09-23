//! Backend providers.
//!
//! Both supported backends expose a node list, live metrics and ping records,
//! but the shape of the wire differs enough that one function cannot serve
//! both: Komari's live socket is a pull (`send "get"`, read one reply) and its
//! HTTP path needs a request per node, while 极简探针 pushes on its own timer
//! and answers the whole fleet in one request.
//!
//! So the *session* lives in the provider module and everything around it —
//! the settings watch, the backoff, error reporting, publishing and the tray's
//! rate limit — stays in [`crate::engine`].

pub mod komari;
pub mod monitor;

use tauri::AppHandle;

use crate::engine::Publisher;
use crate::models::{BackendKind, PingPoint, PingSummary, Settings};

/// A `/api/nodes` fetch that failed.
///
/// A 401 is kept apart from everything else because what to tell the user
/// about it depends on which backend answered — a Komari site wants an API
/// Key, a 极简探针 site wants its public page switched on — and the fetch
/// itself runs before that is known.
#[derive(Debug)]
pub enum FetchError {
    Unauthorized,
    Other(String),
}

impl FetchError {
    /// Resolve into a message, given what the caller knows about the backend.
    pub fn explain(self, unauthorized: &str) -> String {
        match self {
            FetchError::Unauthorized => unauthorized.to_string(),
            FetchError::Other(e) => e,
        }
    }
}

/// A backend, once decided. [`BackendKind`] is what settings persist and may
/// still be `Auto`; this is what the engine actually runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Komari,
    /// 极简探针 (`monitor-probe/monitor`).
    Monitor,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Komari => "Komari",
            Provider::Monitor => "极简探针",
        }
    }

    /// How many ping requests this backend tolerates in flight.
    ///
    /// Komari serves one uuid per request and has no documented gate, so the
    /// fan-out is what keeps a refresh inside its interval. 极简探针 answers
    /// history behind a semaphore of four **shared with every other visitor**,
    /// refusing rather than queueing beyond it, and each request holds the one
    /// connection its agents report through. One at a time costs about a
    /// second a minute for a small fleet and never takes a slot from the web
    /// UI.
    pub fn ping_concurrency(self) -> usize {
        match self {
            Provider::Komari => 8,
            Provider::Monitor => 1,
        }
    }

    /// Resolve the configured kind, probing the site when it is `Auto`.
    pub async fn resolve(
        client: &reqwest::Client,
        settings: &Settings,
        base: &str,
    ) -> Result<Self, String> {
        match settings.backend_kind {
            BackendKind::Komari => Ok(Provider::Komari),
            BackendKind::Monitor => Ok(Provider::Monitor),
            BackendKind::Auto => {
                // Both backends answer `/api/nodes`, and their envelopes are
                // distinct enough to tell apart, so detection costs no request
                // the session would not have made anyway.
                //
                // The key rides along even though only Komari has one: a
                // private Komari site answers 401 without it, which would make
                // it indistinguishable from a 极简探针 site that has closed its
                // public page. A 极简探针 hub authenticates by cookie and
                // ignores the header.
                let text = monitor::fetch_nodes_text(client, base, &settings.api_key)
                    .await
                    .map_err(|e| e.explain(UNAUTHORIZED_UNKNOWN_KIND))?;
                sniff(&text).ok_or_else(|| {
                    "无法识别后端类型：/api/nodes 的响应既不是 Komari 也不是极简探针".to_string()
                })
            }
        }
    }

    pub async fn run(
        self,
        app: &AppHandle,
        client: &reqwest::Client,
        settings: &Settings,
        rx: &mut tokio::sync::watch::Receiver<u64>,
        publisher: &mut Publisher,
    ) -> Result<(), String> {
        match self {
            Provider::Komari => komari::run(app, client, settings, rx, publisher).await,
            Provider::Monitor => monitor::run(app, client, settings, rx, publisher).await,
        }
    }

    pub async fn fetch_ping(
        self,
        client: &reqwest::Client,
        base: &str,
        api_key: &str,
        uuid: &str,
        hours: u64,
    ) -> Option<(Vec<PingPoint>, PingSummary)> {
        match self {
            Provider::Komari => komari::fetch_ping(client, base, api_key, uuid, hours).await,
            Provider::Monitor => monitor::fetch_ping(client, base, uuid, hours).await,
        }
    }
}

/// A 401 from a site whose backend is still unknown: either reading needs a
/// credential this app has not been given, or it is a 极简探针 site with its
/// public page closed. Both are named, since nothing in the response says
/// which.
pub const UNAUTHORIZED_UNKNOWN_KIND: &str =
    "后端返回 401：Komari 私有站点需要在设置中填写 API Key；极简探针站点则需要开启公开页";

/// Which backend produced this `/api/nodes` body.
///
/// Komari wraps everything in `{"status":…,"data":[…]}`; 极简探针 answers
/// `{"admin":…,"nodes":[…]}`. Checking for the list rather than only the
/// marker key keeps an unrelated service that happens to return `{"status":…}`
/// from being mistaken for a backend.
pub fn sniff(body: &str) -> Option<Provider> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if value.get("nodes").is_some_and(|v| v.is_array()) {
        return Some(Provider::Monitor);
    }
    if value.get("status").is_some_and(|v| v.is_string()) && value.get("data").is_some() {
        return Some(Provider::Komari);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_tells_the_two_envelopes_apart() {
        assert_eq!(
            sniff(r#"{"status":"success","data":[{"uuid":"a"}]}"#),
            Some(Provider::Komari)
        );
        assert_eq!(
            sniff(r#"{"admin":false,"nodes":[{"id":1}]}"#),
            Some(Provider::Monitor)
        );
        // An empty fleet still identifies its backend.
        assert_eq!(
            sniff(r#"{"admin":false,"nodes":[]}"#),
            Some(Provider::Monitor)
        );
        assert_eq!(
            sniff(r#"{"status":"success","data":[]}"#),
            Some(Provider::Komari)
        );
    }

    #[test]
    fn sniff_refuses_anything_else() {
        // A reverse proxy's error page, or some other service entirely.
        assert_eq!(sniff("<html>404</html>"), None);
        assert_eq!(sniff(r#"{"status":"ok"}"#), None);
        assert_eq!(sniff(r#"{"nodes":"not-a-list"}"#), None);
        assert_eq!(sniff("[]"), None);
    }

    /// A one-request HTTP server that answers `/api/nodes` with a Komari
    /// envelope only when the Bearer header is present, the way a private
    /// Komari site behaves. Returns its base URL.
    fn private_komari() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let (status, body) = if req.contains("authorization: bearer") {
                    (
                        "200 OK",
                        r#"{"status":"success","data":[{"uuid":"u","name":"n"}]}"#,
                    )
                } else {
                    ("401 Unauthorized", "")
                };
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        base
    }

    /// Detection has to send the API Key. A private Komari site answers 401
    /// without it, which is exactly what a 极简探针 site with its public page
    /// closed answers — so probing anonymously would identify every private
    /// Komari site as an unsupported probe site.
    #[tokio::test]
    async fn auto_detection_sends_the_api_key_so_private_komari_is_recognised() {
        let base = private_komari();
        let settings = Settings {
            backend_kind: BackendKind::Auto,
            api_key: "secret".into(),
            ..Settings::default()
        };
        let found = Provider::resolve(&reqwest::Client::new(), &settings, &base).await;
        assert_eq!(found, Ok(Provider::Komari));
    }

    /// The same site without a key cannot be told apart from a closed probe
    /// site, so the message has to name both.
    #[tokio::test]
    async fn unauthenticated_detection_names_both_possible_causes() {
        let base = private_komari();
        let settings = Settings {
            backend_kind: BackendKind::Auto,
            ..Settings::default()
        };
        let err = Provider::resolve(&reqwest::Client::new(), &settings, &base)
            .await
            .unwrap_err();
        assert_eq!(err, UNAUTHORIZED_UNKNOWN_KIND);
        assert!(err.contains("API Key") && err.contains("公开页"));
    }

    #[test]
    fn explicit_kinds_skip_detection() {
        // `resolve` is async and would reach the network for `Auto`; the two
        // explicit variants must map without one, which is what makes the
        // settings picker a way to avoid the probe.
        for (kind, want) in [
            (BackendKind::Komari, Provider::Komari),
            (BackendKind::Monitor, Provider::Monitor),
        ] {
            let settings = Settings {
                backend_kind: kind,
                ..Settings::default()
            };
            let got = match settings.backend_kind {
                BackendKind::Komari => Provider::Komari,
                BackendKind::Monitor => Provider::Monitor,
                BackendKind::Auto => unreachable!(),
            };
            assert_eq!(got, want);
        }
    }

    #[test]
    fn ping_concurrency_respects_the_probe_hubs_history_gate() {
        assert_eq!(Provider::Monitor.ping_concurrency(), 1);
        assert!(Provider::Komari.ping_concurrency() > 1);
    }
}
