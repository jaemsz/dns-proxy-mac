# dns-proxy

A lightweight DNS proxy for macOS that accepts standard DNS queries on UDP port 53 and forwards them encrypted over DNS-over-TLS (DoT, port 853) to a remote [dns](https://github.com/jaemsz/dns/) server. All DNS traffic leaving your Mac is encrypted and filtered for malicious domains by the upstream server.

```
macOS apps --> UDP :53 (localhost) --> dns-proxy --> DoT :853 --> dns (AWS EC2)
```

## Security

The proxy is hardened against supply-chain attacks in its dependency tree:

- **Privilege dropping** — Starts as root to bind port 53, then immediately drops to the `nobody` user (uid 65534). Compromised code runs without privileges.
- **launchd service** — Auto-restarts on crash, logs to `/var/log/dns-proxy.log`

## Prerequisites

- macOS
- Rust toolchain (`rustup`, `cargo`)
- A running [dns](https://github.com/jaemsz/dns/) server with DoT enabled on port 853

## Build

```bash
cargo build --release
```

## Configuration

Edit `config.toml`:

```toml
[server]
listen_udp    = "127.0.0.1:53"
debug         = true
drop_user_id  = 65534    # 'nobody' user
drop_group_id = 65534

[upstream]
addr       = "<EC2_PUBLIC_IP>:853"   # your dns server IP
tls_name   = "dns-filter"           # must match CN in server's TLS cert
timeout_ms = 3000
# ca_cert  = "cert.pem"            # uncomment for self-signed server certs
```

Replace `<EC2_PUBLIC_IP>` with your dns server's public IP address. The `tls_name` must match the Common Name (CN) in the server's TLS certificate.

If the dns server uses a self-signed certificate, copy its `cert.pem` to the proxy directory and uncomment the `ca_cert` line.

## Install as a system service

```bash
# Build release binary
cargo build --release

# Create install directory
sudo mkdir -p /opt/dns-proxy

# Copy files
sudo cp target/release/dns-proxy /opt/dns-proxy/
sudo cp config.toml /opt/dns-proxy/

# If using a custom CA cert for self-signed server certs:
# sudo cp cert.pem /opt/dns-proxy/

# Install and start the launchd service
sudo cp com.dns-proxy.plist /Library/LaunchDaemons/
sudo launchctl load /Library/LaunchDaemons/com.dns-proxy.plist
```

## Set macOS to use the proxy

Option A — System Settings:

> System Settings > Network > Wi-Fi > Details > DNS > add `127.0.0.1`

Option B — Command line:

```bash
networksetup -setdnsservers Wi-Fi 127.0.0.1
```

To revert:

```bash
networksetup -setdnsservers Wi-Fi empty
```

## Verify

```bash
# Should resolve normally
dig @127.0.0.1 google.com

# Should return NXDOMAIN (blocked by upstream dns)
dig @127.0.0.1 ads.facebook.com
```

## Run manually (without launchd)

```bash
sudo cargo run -- config.toml
```

## Uninstall

```bash
sudo launchctl unload /Library/LaunchDaemons/com.dns-proxy.plist
sudo rm /Library/LaunchDaemons/com.dns-proxy.plist
sudo rm -rf /opt/dns-proxy
networksetup -setdnsservers Wi-Fi empty
```

## Logs

```bash
tail -f /var/log/dns-proxy.log
```

Or with debug logging:

```bash
sudo RUST_LOG=dns_proxy=debug ./target/release/dns-proxy config.toml
```
