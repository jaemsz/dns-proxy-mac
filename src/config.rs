use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub listen_udp: SocketAddr,
    #[serde(default)]
    pub debug: bool,
    /// Unix UID to drop privileges to after binding the socket.
    /// If unset and running as root, a warning is logged.
    pub drop_user_id: Option<u32>,
    /// Unix GID to drop privileges to. Defaults to drop_user_id if unset.
    pub drop_group_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamConfig {
    /// IP:port of the remote dns-filter server, e.g. "1.2.3.4:853"
    pub addr: SocketAddr,
    /// TLS SNI name for certificate validation, e.g. "dns-filter"
    pub tls_name: String,
    pub timeout_ms: u64,
    /// Optional path to a PEM CA certificate for self-signed server certs.
    /// If absent, uses webpki root CAs (for Let's Encrypt / public CAs).
    #[allow(dead_code)]
    pub ca_cert: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {}: {e}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Failed to parse config: {e}"))?;
        Ok(config)
    }

    pub fn default_path() -> PathBuf {
        PathBuf::from("config.toml")
    }
}
