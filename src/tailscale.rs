// src/tailscale.rs — Tailscale integration for remote ADB access
//
// Resolves phone on tailnet, connects ADB over WireGuard tunnel,
// monitors health, auto-reconnects. Integrates with existing
// perception.adb_device and action executor device targets.

use std::net::TcpStream;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

// ── Config (deserialized from config.toml [tailscale]) ──────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TailscaleConfig {
    #[serde(default)]
    pub enabled: bool,

    /// Phone's Tailscale hostname (e.g. "pixel-7") or IP (e.g. "100.64.1.2")
    #[serde(default = "default_phone_hostname")]
    pub phone_hostname: String,

    /// ADB TCP port on the phone (usually 5555)
    #[serde(default = "default_adb_port")]
    pub adb_port: u16,

    /// Auto-connect ADB via Tailscale on startup
    #[serde(default = "default_true")]
    pub auto_connect: bool,

    /// Health-check interval in seconds (0 = disabled)
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,

    /// Consecutive failures before attempting reconnect
    #[serde(default = "default_max_failures")]
    pub max_failures_before_reconnect: u32,
}

fn default_phone_hostname() -> String { "my-android-phone".into() }
fn default_adb_port() -> u16 { 5555 }
fn default_true() -> bool { true }
fn default_health_interval() -> u64 { 60 }
fn default_max_failures() -> u32 { 3 }

impl Default for TailscaleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            phone_hostname: default_phone_hostname(),
            adb_port: default_adb_port(),
            auto_connect: true,
            health_check_interval_secs: default_health_interval(),
            max_failures_before_reconnect: default_max_failures(),
        }
    }
}

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected { ip: String, latency_ms: Option<u64> },
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct TailscalePeer {
    pub hostname: String,
    pub ip: String,
    pub online: bool,
    pub os: String,
}

/// API response for /tailscale/status
#[derive(Debug, Serialize)]
pub struct TailscaleStatusResponse {
    pub enabled: bool,
    pub connection_state: String,
    pub phone_hostname: String,
    pub phone_ip: Option<String>,
    pub adb_address: Option<String>,
    pub latency_ms: Option<u64>,
    pub self_ip: Option<String>,
    pub android_peers: Vec<TailscalePeer>,
}

// ── Manager ─────────────────────────────────────────────────────────────────

pub struct TailscaleManager {
    config: TailscaleConfig,
    state: ConnectionState,
    resolved_ip: Option<String>,
    consecutive_failures: u32,
    last_health_check: Option<Instant>,
}

impl TailscaleManager {
    pub fn new(config: TailscaleConfig) -> Self {
        Self {
            config,
            state: ConnectionState::Disconnected,
            resolved_ip: None,
            consecutive_failures: 0,
            last_health_check: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// The ADB address to use (e.g. "100.64.1.2:5555") — drop-in replacement
    /// for config.perception.adb_device
    pub fn adb_address(&self) -> Option<String> {
        self.resolved_ip.as_ref().map(|ip| format!("{}:{}", ip, self.config.adb_port))
    }

    pub fn connection_state(&self) -> &ConnectionState {
        &self.state
    }

    // ── CLI checks ──────────────────────────────────────────────────────

    pub fn is_tailscale_installed() -> bool {
        Command::new("tailscale").arg("version").output()
            .map(|o| o.status.success()).unwrap_or(false)
    }

    pub fn get_self_ip() -> Option<String> {
        Command::new("tailscale").args(["ip", "-4"]).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// List all peers, optionally filtering to Android only
    pub fn list_peers(android_only: bool) -> Vec<TailscalePeer> {
        let output = match Command::new("tailscale").args(["status", "--json"]).output() {
            Ok(o) if o.status.success() => o,
            _ => return vec![],
        };

        let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let mut peers = Vec::new();
        if let Some(peer_map) = json.get("Peer").and_then(|p| p.as_object()) {
            for (_key, peer) in peer_map {
                let os = peer.get("OS").and_then(|o| o.as_str()).unwrap_or("").to_string();
                if android_only && os.to_lowercase() != "android" {
                    continue;
                }
                peers.push(TailscalePeer {
                    hostname: peer.get("HostName").and_then(|h| h.as_str()).unwrap_or("").to_string(),
                    ip: peer.get("TailscaleIPs")
                        .and_then(|ips| ips.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|ip| ip.as_str())
                        .unwrap_or("").to_string(),
                    online: peer.get("Online").and_then(|o| o.as_bool()).unwrap_or(false),
                    os,
                });
            }
        }
        peers
    }

    // ── Resolution ──────────────────────────────────────────────────────

    /// Resolve phone_hostname → Tailscale IP
    pub fn resolve_phone_ip(&mut self) -> Result<String, String> {
        let hostname = &self.config.phone_hostname;

        // Already an IP?
        if hostname.starts_with("100.") || hostname.parse::<std::net::Ipv4Addr>().is_ok() {
            self.resolved_ip = Some(hostname.clone());
            return Ok(hostname.clone());
        }

        // `tailscale ip -4 <hostname>`
        if let Ok(output) = Command::new("tailscale").args(["ip", "-4", hostname]).output() {
            if output.status.success() {
                let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !ip.is_empty() {
                    info!("Resolved {} → {}", hostname, ip);
                    self.resolved_ip = Some(ip.clone());
                    return Ok(ip);
                }
            }
        }

        // Search peers by hostname
        for peer in Self::list_peers(false) {
            if peer.hostname.to_lowercase() == hostname.to_lowercase() {
                info!("Found phone in peer list: {} → {}", peer.hostname, peer.ip);
                self.resolved_ip = Some(peer.ip.clone());
                return Ok(peer.ip);
            }
        }

        Err(format!(
            "Could not resolve '{}'. Is Tailscale running on the phone and signed into the same tailnet?",
            hostname
        ))
    }

    // ── Connect / Disconnect ────────────────────────────────────────────

    /// Full connect: ensure tailscale up → resolve → TCP test → adb connect
    /// Returns the ADB address string (e.g. "100.64.1.2:5555")
    pub fn connect(&mut self) -> Result<String, String> {
        self.state = ConnectionState::Connecting;

        // Ensure tailscale is running
        let status_out = Command::new("tailscale").arg("status").output()
            .map_err(|e| format!("tailscale not found: {e}"))?;
        if !status_out.status.success() {
            info!("Tailscale not connected, trying `tailscale up`...");
            let _ = Command::new("sudo").args(["tailscale", "up"]).status();
            // Re-check
            let recheck = Command::new("tailscale").arg("status").output()
                .map_err(|e| format!("tailscale still failing: {e}"))?;
            if !recheck.status.success() {
                let msg = "Tailscale is not connected. Run `sudo tailscale up` first.".to_string();
                self.state = ConnectionState::Failed { reason: msg.clone() };
                return Err(msg);
            }
        }

        // Resolve IP
        let ip = self.resolve_phone_ip()?;
        let addr = format!("{}:{}", ip, self.config.adb_port);

        // TCP connectivity test
        debug!("Testing TCP to {addr}...");
        TcpStream::connect_timeout(
            &addr.parse().map_err(|e| format!("bad address: {e}"))?,
            Duration::from_secs(10),
        ).map_err(|e| {
            let msg = format!("TCP to {addr} failed: {e}. Is ADB TCP enabled? (adb tcpip 5555)");
            self.state = ConnectionState::Failed { reason: msg.clone() };
            msg
        })?;

        // adb connect
        let output = Command::new("adb").args(["connect", &addr]).output()
            .map_err(|e| { let m = format!("adb failed: {e}"); self.state = ConnectionState::Failed { reason: m.clone() }; m })?;
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.contains("connected") || stdout.contains("already connected") {
            let latency = self.ping_phone();
            self.state = ConnectionState::Connected { ip: ip.clone(), latency_ms: latency };
            self.consecutive_failures = 0;
            info!("✅ ADB connected via Tailscale: {addr}");
            Ok(addr)
        } else {
            let msg = format!("adb connect to {addr}: {}", stdout.trim());
            self.state = ConnectionState::Failed { reason: msg.clone() };
            Err(msg)
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(addr) = self.adb_address() {
            let _ = Command::new("adb").args(["disconnect", &addr]).output();
            info!("Disconnected ADB from {addr}");
        }
        self.state = ConnectionState::Disconnected;
    }

    /// Ping via tailscale, return latency in ms
    pub fn ping_phone(&self) -> Option<u64> {
        let ip = self.resolved_ip.as_ref()?;
        let start = Instant::now();
        let output = Command::new("tailscale")
            .args(["ping", "--c", "1", "--timeout", "5s", ip])
            .output().ok()?;
        if output.status.success() {
            // Try parsing "pong from ... in 42ms"
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(ms) = text.lines()
                .find(|l| l.contains("pong"))
                .and_then(|l| l.rsplit("in ").next())
                .and_then(|s| s.trim().strip_suffix("ms"))
                .and_then(|s| s.parse::<u64>().ok())
            {
                return Some(ms);
            }
            Some(start.elapsed().as_millis() as u64)
        } else {
            None
        }
    }

    // ── Health check ────────────────────────────────────────────────────

    /// Returns true if healthy. Triggers auto-reconnect after max failures.
    pub fn health_check(&mut self) -> bool {
        if self.config.health_check_interval_secs == 0 {
            return true;
        }
        if let Some(last) = self.last_health_check {
            if last.elapsed() < Duration::from_secs(self.config.health_check_interval_secs) {
                return matches!(self.state, ConnectionState::Connected { .. });
            }
        }
        self.last_health_check = Some(Instant::now());

        let addr = match self.adb_address() {
            Some(a) => a,
            None => return false,
        };

        let ok = TcpStream::connect_timeout(
            &addr.parse().unwrap_or_else(|_| "100.0.0.1:5555".parse().unwrap()),
            Duration::from_secs(5),
        ).is_ok();

        if ok {
            self.consecutive_failures = 0;
            if let ConnectionState::Connected { ref ip, .. } = self.state {
                let latency = self.ping_phone();
                self.state = ConnectionState::Connected { ip: ip.clone(), latency_ms: latency };
            }
            true
        } else {
            self.consecutive_failures += 1;
            warn!("Tailscale health check failed ({}/{})",
                self.consecutive_failures, self.config.max_failures_before_reconnect);

            if self.consecutive_failures >= self.config.max_failures_before_reconnect {
                warn!("Max failures reached — reconnecting...");
                self.connect().is_ok()
            } else {
                false
            }
        }
    }

    // ── API response ────────────────────────────────────────────────────

    pub fn api_status(&self) -> TailscaleStatusResponse {
        let (conn_str, latency) = match &self.state {
            ConnectionState::Disconnected => ("disconnected".into(), None),
            ConnectionState::Connecting => ("connecting".into(), None),
            ConnectionState::Connected { latency_ms, .. } => ("connected".into(), *latency_ms),
            ConnectionState::Failed { reason } => (format!("failed: {reason}"), None),
        };

        TailscaleStatusResponse {
            enabled: self.config.enabled,
            connection_state: conn_str,
            phone_hostname: self.config.phone_hostname.clone(),
            phone_ip: self.resolved_ip.clone(),
            adb_address: self.adb_address(),
            latency_ms: latency,
            self_ip: Self::get_self_ip(),
            android_peers: Self::list_peers(true),
        }
    }
}

// ── Background health loop (run in tokio::spawn) ────────────────────────────

pub async fn tailscale_health_loop(
    manager: Arc<Mutex<TailscaleManager>>,
    interval_secs: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if interval_secs == 0 { return; }

    let interval = Duration::from_secs(interval_secs);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                let mut mgr = manager.lock().await;
                mgr.health_check();
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    info!("Tailscale health loop shutting down");
                    break;
                }
            }
        }
    }
}