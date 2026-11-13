use crate::modules::manifest::BackendKind;
use crate::peers;
use crate::state::{AppState, restart_link};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const LINK_RESUME_POLL: Duration = Duration::from_secs(5);
/// `Instant` stops while the machine is suspended; `SystemTime` does not. A
/// gap this large between the two clocks means the process just resumed.
const LINK_RESUME_GAP: Duration = Duration::from_secs(20);
/// Sleep/wake is rare, but a flapping clock or a burst of lid toggles must not
/// turn into a burst of account requests. One automatic rebuild per minute.
const LINK_RESUME_COOLDOWN: Duration = Duration::from_secs(60);
/// Only retry when the network is not back yet. Each retry rebuilds Link, so
/// keep the list short.
const LINK_RESUME_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(120)];

pub async fn sidecar_supervisor(state: Arc<AppState>) {
    loop {
        state.sidecars.trim_logs(4 * 1024 * 1024);
        let dead = state.sidecars.reap();
        for id in dead {
            let status = state.sidecars.record_failure(&id, "sidecar exited");
            tracing::warn!(
                module = %id,
                attempts = status.attempts,
                retry_in_secs = status.next_retry_in_secs,
                "sidecar exited; restart scheduled"
            );
        }
        for module in state.modules.installed() {
            if module.manifest.backend.kind != BackendKind::Sidecar
                || !module.state.enabled
                || !module.grants("backend")
            {
                continue;
            }
            if state.sidecars.is_running(&module.manifest.id)
                || !state.sidecars.should_attempt(&module.manifest.id)
            {
                continue;
            }
            if !module.manifest.backend.supported_here() {
                continue;
            }
            if let Err(error) = state.sidecars.start(&module).await {
                let status = state
                    .sidecars
                    .record_failure(&module.manifest.id, error.to_string());
                tracing::warn!(
                    module = %module.manifest.id,
                    attempts = status.attempts,
                    retry_in_secs = status.next_retry_in_secs,
                    %error,
                    "sidecar restart failed"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    }
}

pub async fn refresh_all_peers(state: Arc<AppState>) {
    let peers = state.config().peers.clone();
    let checks = peers.into_iter().map(|peer| {
        let state = Arc::clone(&state);
        async move { peers::probe(&state.client, &peer, peer.url.as_deref()).await }
    });
    let health = futures_util::future::join_all(checks)
        .await
        .into_iter()
        .map(|health| (health.id.clone(), health))
        .collect();
    *state.peer_health.write().expect("peer health lock") = health;
}

/// Cache the LAN interfaces reported by online peers. The cache lets the hub
/// send a wake-on-LAN packet after the peer has gone to sleep.
pub async fn refresh_wake_cache(state: Arc<AppState>) {
    let peers = state.config().peers.clone();
    for peer in peers {
        if state.wake_cache_fresh(&peer.id) {
            continue;
        }
        let online = state
            .peer_health
            .read()
            .expect("peer health lock")
            .get(&peer.id)
            .map(|health| health.online)
            .unwrap_or(false);
        if !online {
            continue;
        }
        let Some(url) = state.peer_url(&peer) else {
            continue;
        };
        let Ok(value) =
            peers::fetch_json(&state.client, &peer, Some(&url), "/api/v1/peer/wake").await
        else {
            continue;
        };
        let Some(interfaces) = value.get("interfaces").cloned() else {
            continue;
        };
        let Ok(interfaces) = serde_json::from_value::<Vec<crate::wake::WakeInterface>>(interfaces)
        else {
            continue;
        };
        if let Err(error) = state.record_wake_interfaces(&peer.id, &peer.name, interfaces) {
            tracing::warn!(%error, peer = %peer.name, "could not cache wake-on-LAN interfaces");
        }
    }
}

#[derive(Clone, Copy)]
struct ClockSample {
    wall: SystemTime,
    mono: Instant,
}

impl ClockSample {
    fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
}

struct ResumeClock {
    last: ClockSample,
}

impl ResumeClock {
    fn new() -> Self {
        Self {
            last: ClockSample::now(),
        }
    }

    /// Return the wall-clock time that elapsed while the monotonic clock was
    /// not running. Normal ticks are a few milliseconds apart; a suspended
    /// machine produces a gap roughly equal to the sleep duration.
    fn observe(&mut self, now: ClockSample) -> Option<Duration> {
        let wall = now.wall.duration_since(self.last.wall).unwrap_or_default();
        let mono = now.mono.saturating_duration_since(self.last.mono);
        self.last = now;
        let gap = wall.saturating_sub(mono);
        (gap >= LINK_RESUME_GAP).then_some(gap)
    }
}

/// A stale iroh endpoint survives a system suspend even though the Link
/// heartbeat keeps working: loopback peer forwards then accept connections but
/// hang. Rebinding the whole Link manager is the repair. Detect resume with the
/// wall/monotonic clock gap and retry while the network comes back.
pub async fn link_resume_watchdog(state: Arc<AppState>) {
    let mut clock = ResumeClock::new();
    let mut retry: Option<(usize, Instant)> = None;
    let mut last_attempt: Option<Instant> = None;
    loop {
        tokio::time::sleep(LINK_RESUME_POLL).await;

        if let Some(paused) = clock.observe(ClockSample::now()) {
            let config = state.config().link.clone();
            let has_token = config
                .token
                .as_deref()
                .is_some_and(|token| !token.is_empty());
            let link_running = state.link.read().expect("link lock").is_some();
            let cooled_down = last_attempt.is_none_or(|at| at.elapsed() >= LINK_RESUME_COOLDOWN);
            if config.enabled && has_token && link_running && cooled_down {
                tracing::info!(
                    paused_secs = paused.as_secs(),
                    "system resume detected; re-establishing Link"
                );
                last_attempt = Some(Instant::now());
                retry = Some((0, Instant::now()));
            }
        }

        let Some((attempt, due)) = retry else {
            continue;
        };
        if Instant::now() < due {
            continue;
        }

        let config = state.config().link.clone();
        let has_token = config
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty());
        if !config.enabled || !has_token {
            retry = None;
            continue;
        }

        match restart_link(Arc::clone(&state)).await {
            Ok(()) => {
                tracing::info!("Link re-established after resume");
                retry = None;
            }
            Err(error) => {
                *state.link_error.write().expect("link error lock") = Some(format!("{error:#}"));
                if let Some(delay) = LINK_RESUME_RETRY_DELAYS.get(attempt) {
                    tracing::warn!(
                        %error,
                        retry_in_secs = delay.as_secs(),
                        "could not re-establish Link after resume"
                    );
                    retry = Some((attempt + 1, Instant::now() + *delay));
                } else {
                    tracing::warn!(%error, "could not re-establish Link after resume; giving up");
                    retry = None;
                }
            }
        }
    }
}

pub async fn peer_refresh_loop(state: Arc<AppState>) {
    tokio::spawn(link_resume_watchdog(Arc::clone(&state)));
    loop {
        refresh_all_peers(Arc::clone(&state)).await;
        refresh_wake_cache(Arc::clone(&state)).await;
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}

#[cfg(test)]
mod resume_clock_tests {
    use super::*;

    fn clock_at(wall: SystemTime, mono: Instant) -> ResumeClock {
        ResumeClock {
            last: ClockSample { wall, mono },
        }
    }

    #[test]
    fn ignores_a_normal_tick() {
        let mono = Instant::now();
        let mut clock = clock_at(SystemTime::UNIX_EPOCH, mono);
        assert_eq!(
            clock.observe(ClockSample {
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(5),
                mono: mono + Duration::from_secs(5),
            }),
            None
        );
    }

    #[test]
    fn detects_a_suspend_gap() {
        let mono = Instant::now();
        let mut clock = clock_at(SystemTime::UNIX_EPOCH, mono);
        let gap = clock
            .observe(ClockSample {
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1225),
                mono: mono + Duration::from_secs(5),
            })
            .expect("suspend gap");
        assert_eq!(gap, Duration::from_secs(1220));
    }

    #[test]
    fn ignores_a_short_clock_step() {
        let mono = Instant::now();
        let mut clock = clock_at(SystemTime::UNIX_EPOCH, mono);
        assert_eq!(
            clock.observe(ClockSample {
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(25),
                mono: mono + Duration::from_secs(10),
            }),
            None
        );
    }

    #[test]
    fn ignores_a_backward_wall_clock_step() {
        let mono = Instant::now();
        let mut clock = clock_at(SystemTime::UNIX_EPOCH + Duration::from_secs(100), mono);
        assert_eq!(
            clock.observe(ClockSample {
                wall: SystemTime::UNIX_EPOCH + Duration::from_secs(50),
                mono: mono + Duration::from_secs(5),
            }),
            None
        );
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::config::{Config, Paths, PeerConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn paths(root: &std::path::Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            runtime_dir: root.join("run"),
            config_file: root.join("config/config.toml"),
            state_file: root.join("data/state.json"),
            modules_dir: root.join("data/modules"),
            workshop_dir: root.join("data/workshop"),
            logs_dir: root.join("data/logs"),
        }
    }

    async fn peer_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 4096];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let body = if request.contains("/api/v1/peer/wake") {
                    r#"{"interfaces":[{"name":"enp6s0","mac":"aa:bb:cc:dd:ee:ff","ip":"192.168.68.10","netmask":"255.255.255.0","broadcast":"192.168.68.255"}]}"#
                } else {
                    r#"{"ok":true}"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn refreshes_peer_health_and_caches_lan_interfaces() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let url = peer_server().await;
        let config = Config {
            peer_token: Some("local-token".into()),
            peers: vec![PeerConfig {
                id: "link-test".into(),
                name: "Test peer".into(),
                url: Some(url),
                token: "peer-token".into(),
            }],
            ..Config::default()
        };
        let state = AppState::new(paths.clone(), config).await.unwrap();

        refresh_all_peers(Arc::clone(&state)).await;
        let health = state
            .peer_health
            .read()
            .expect("peer health lock")
            .get("link-test")
            .cloned()
            .expect("peer health");
        assert!(health.online, "{health:?}");
        assert_eq!(health.name, "Test peer");

        refresh_wake_cache(Arc::clone(&state)).await;
        assert!(state.wake_cache_fresh("link-test"));
        let cache = std::fs::read_to_string(paths.data_dir.join("wake.json")).unwrap();
        assert!(cache.contains("enp6s0"));
        assert!(cache.contains("aa:bb:cc:dd:ee:ff"));
    }
}
