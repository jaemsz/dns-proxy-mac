use crate::config::Config;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::RecordType;
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use hickory_proto::xfer::Protocol;
use hickory_proto::ProtoErrorKind;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::{ResolveErrorKind, TokioResolver};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{error, info, warn};

const MAX_UDP_PAYLOAD: usize = 4096;

pub struct DnsProxy {
    config: Arc<Config>,
    resolver: TokioResolver,
}

impl DnsProxy {
    pub fn new(config: Arc<Config>, resolver: TokioResolver) -> Self {
        Self { config, resolver }
    }

    /// Bind the UDP socket (requires root for port 53) and return it.
    /// Call this before dropping privileges.
    pub async fn bind_udp(&self) -> anyhow::Result<UdpSocket> {
        let socket = UdpSocket::bind(self.config.server.listen_udp).await?;
        info!(addr = %self.config.server.listen_udp, "UDP socket bound");
        Ok(socket)
    }

    /// Run the UDP recv loop on an already-bound socket.
    /// Can run as unprivileged user since the socket is already bound.
    pub async fn run_udp(self: Arc<Self>, socket: UdpSocket) -> anyhow::Result<()> {
        info!("UDP listener started");
        let socket = Arc::new(socket);
        let mut buf = vec![0u8; MAX_UDP_PAYLOAD];

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, peer)) => {
                    let query_bytes = buf[..len].to_vec();
                    let server = Arc::clone(&self);
                    let sock = Arc::clone(&socket);
                    tokio::spawn(async move {
                        server.handle_udp_query(sock, query_bytes, peer).await;
                    });
                }
                Err(e) => error!("UDP recv error: {e}"),
            }
        }
    }

    async fn handle_udp_query(
        &self,
        socket: Arc<UdpSocket>,
        query_bytes: Vec<u8>,
        peer: SocketAddr,
    ) {
        let response_bytes = match self.process_query(&query_bytes, peer).await {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(peer = %peer, "Query processing error: {e}");
                return;
            }
        };

        if let Err(e) = socket.send_to(&response_bytes, peer).await {
            error!("UDP send error to {peer}: {e}");
        }
    }

    async fn process_query(
        &self,
        query_bytes: &[u8],
        peer: SocketAddr,
    ) -> anyhow::Result<Vec<u8>> {
        let query = Message::from_bytes(query_bytes)
            .map_err(|e| anyhow::anyhow!("DNS parse error: {e}"))?;

        if query.message_type() != MessageType::Query {
            anyhow::bail!("Not a query message");
        }

        if query.op_code() != OpCode::Query {
            return build_not_impl(&query);
        }

        let question = query.queries().first();
        let domain = match question {
            Some(q) => q.name().to_string(),
            None => anyhow::bail!("Empty question section"),
        };
        let qtype = question.map(|q| q.query_type()).unwrap_or(RecordType::A);

        if self.config.server.debug {
            info!(domain = %domain, qtype = ?qtype, peer = %peer, "Query");
        }

        // Forward to upstream dns-filter over DoT
        match self.forward(&query).await {
            Ok(response) => response
                .to_bytes()
                .map_err(|e| anyhow::anyhow!("Encode response: {e}")),
            Err(e) => {
                warn!(domain = %domain, error = %e, "Upstream DoT forwarding failed");
                build_servfail(&query)
            }
        }
    }

    async fn forward(&self, query: &Message) -> anyhow::Result<Message> {
        let question = query
            .queries()
            .first()
            .ok_or_else(|| anyhow::anyhow!("Query has no questions"))?;

        let name = question.name().clone();
        let record_type = question.query_type();

        match self.resolver.lookup(name.clone(), record_type).await {
            Ok(response) => {
                let mut msg = Message::new();
                msg.set_id(query.id());
                msg.set_message_type(MessageType::Response);
                msg.set_op_code(OpCode::Query);
                msg.set_response_code(ResponseCode::NoError);
                msg.set_recursion_desired(query.recursion_desired());
                msg.set_recursion_available(true);
                msg.add_queries(query.queries().to_vec());
                for record in response.records() {
                    msg.add_answer(record.clone());
                }
                Ok(msg)
            }
            Err(e) => {
                // Propagate authoritative NXDOMAIN / NODATA from upstream
                if let ResolveErrorKind::Proto(proto) = e.kind() {
                    if let ProtoErrorKind::NoRecordsFound { response_code, .. } = proto.kind() {
                        let mut msg = Message::new();
                        msg.set_id(query.id());
                        msg.set_message_type(MessageType::Response);
                        msg.set_op_code(OpCode::Query);
                        msg.set_response_code(*response_code);
                        msg.set_recursion_desired(query.recursion_desired());
                        msg.set_recursion_available(true);
                        msg.add_queries(query.queries().to_vec());
                        return Ok(msg);
                    }
                }
                warn!(name = %name, record_type = ?record_type, error = %e, "Upstream DoT resolution failed");
                Err(anyhow::anyhow!("Upstream error: {e}"))
            }
        }
    }
}

/// Build a DoT resolver targeting the remote dns-filter server.
pub fn build_resolver(config: &Config) -> anyhow::Result<TokioResolver> {
    let mut resolver_config = ResolverConfig::new();
    resolver_config.add_name_server(NameServerConfig {
        socket_addr: config.upstream.addr,
        protocol: Protocol::Tls,
        tls_dns_name: Some(config.upstream.tls_name.clone()),
        http_endpoint: None,
        trust_negative_responses: true,
        bind_addr: None,
    });

    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_millis(config.upstream.timeout_ms);
    opts.attempts = 2;
    opts.edns0 = true;
    opts.cache_size = 1024;

    let resolver =
        TokioResolver::builder_with_config(resolver_config, TokioConnectionProvider::default())
            .with_options(opts)
            .build();

    Ok(resolver)
}

/// Drop root privileges by switching to the given uid/gid.
/// Call after binding privileged ports.
pub fn drop_privileges(uid: u32, gid: u32) -> anyhow::Result<()> {
    // setgid must come before setuid (can't change group after dropping root)
    if unsafe { libc::setgid(gid) } != 0 {
        anyhow::bail!("Failed to setgid({}): {}", gid, std::io::Error::last_os_error());
    }
    if unsafe { libc::setuid(uid) } != 0 {
        anyhow::bail!("Failed to setuid({}): {}", uid, std::io::Error::last_os_error());
    }
    info!(uid, gid, "Dropped privileges");
    Ok(())
}

fn base_response(query: &Message) -> Message {
    let mut resp = Message::new();
    resp.set_id(query.id());
    resp.set_message_type(MessageType::Response);
    resp.set_op_code(OpCode::Query);
    resp.set_recursion_desired(query.recursion_desired());
    resp.set_recursion_available(true);
    resp.add_queries(query.queries().to_vec());
    resp
}

fn build_servfail(query: &Message) -> anyhow::Result<Vec<u8>> {
    let mut resp = base_response(query);
    resp.set_response_code(ResponseCode::ServFail);
    resp.to_bytes()
        .map_err(|e| anyhow::anyhow!("Encode SERVFAIL: {e}"))
}

fn build_not_impl(query: &Message) -> anyhow::Result<Vec<u8>> {
    let mut resp = base_response(query);
    resp.set_response_code(ResponseCode::NotImp);
    resp.to_bytes()
        .map_err(|e| anyhow::anyhow!("Encode NOTIMP: {e}"))
}
