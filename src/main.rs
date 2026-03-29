mod config;
mod server;

use crate::config::Config;
use crate::server::{build_resolver, drop_privileges, DnsProxy};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install ring crypto provider before any rustls code runs
    tokio_rustls::rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install ring crypto provider"))?;

    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("dns_proxy=info,warn")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(Config::default_path);

    let config = Arc::new(Config::load(&config_path)?);
    info!("Configuration loaded from {}", config_path.display());

    let resolver = build_resolver(&config)?;
    info!(
        addr = %config.upstream.addr,
        tls_name = %config.upstream.tls_name,
        "Upstream DoT resolver configured"
    );

    let proxy = Arc::new(DnsProxy::new(Arc::clone(&config), resolver));

    // Bind the UDP socket while we still have root privileges
    let socket = proxy.bind_udp().await?;

    // Drop root privileges if running as root.
    // Switch to the `nobody` user (uid=65534, gid=65534 on macOS).
    let drop_uid = config.server.drop_user_id;
    if unsafe { libc::getuid() } == 0 {
        if let Some(uid) = drop_uid {
            let gid = config.server.drop_group_id.unwrap_or(uid);
            drop_privileges(uid, gid)?;
        } else {
            tracing::warn!(
                "Running as root without drop_user_id configured — \
                 consider setting [server].drop_user_id in config.toml"
            );
        }
    }

    let shutdown = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
        info!("Shutdown signal received, stopping...");
    };

    tokio::select! {
        result = proxy.run_udp(socket) => {
            if let Err(e) = result {
                tracing::error!("UDP server exited with error: {e}");
                return Err(e);
            }
        },
        _ = shutdown => {
            info!("Shutting down cleanly");
        }
    }

    Ok(())
}
