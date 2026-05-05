use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::net::UdpSocket;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:41000")]
    listen: SocketAddr,
    #[arg(long, default_value = "127.0.0.1:40000")]
    server: SocketAddr,
    #[arg(long, default_value_t = 0.0, value_parser = parse_drop_percent)]
    drop_percent: f64,
    #[arg(long, default_value_t = 0, value_parser = parse_delay_ms)]
    delay_ms: u64,
    #[arg(long, default_value_t = 0, value_parser = parse_jitter_ms)]
    jitter_ms: u64,
    #[arg(long, default_value_t = 0x1c0d_ecaf)]
    seed: u64,
    #[arg(long, default_value_t = 5_000, value_parser = parse_stats_interval_ms)]
    stats_interval_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("netem=info".parse()?))
        .init();

    let args = Args::parse();
    let listen_socket = Arc::new(
        UdpSocket::bind(args.listen)
            .await
            .with_context(|| format!("bind UDP netem listener at {}", args.listen))?,
    );
    let impairment = Arc::new(Impairment::new(
        args.drop_percent / 100.0,
        args.delay_ms,
        args.jitter_ms,
        args.seed,
    ));
    let stats = Arc::new(ProxyStats::default());

    tracing::info!(
        listen = %args.listen,
        server = %args.server,
        drop_percent = args.drop_percent,
        delay_ms = args.delay_ms,
        jitter_ms = args.jitter_ms,
        stats_interval_ms = args.stats_interval_ms,
        "UDP impairment proxy listening; use delay-ms plus jitter-ms for symmetric delay variation"
    );

    if args.stats_interval_ms > 0 {
        tokio::spawn(report_stats(
            Arc::clone(&stats),
            Duration::from_millis(args.stats_interval_ms),
        ));
    }

    run_proxy(listen_socket, args.server, impairment, stats).await
}

async fn run_proxy(
    listen_socket: Arc<UdpSocket>,
    server_addr: SocketAddr,
    impairment: Arc<Impairment>,
    stats: Arc<ProxyStats>,
) -> anyhow::Result<()> {
    let mut routes = HashMap::<SocketAddr, Arc<UdpSocket>>::new();
    let mut buf = vec![0_u8; 2_048];

    loop {
        let (len, client_addr) = listen_socket.recv_from(&mut buf).await?;
        stats
            .client_to_server_received
            .fetch_add(1, Ordering::Relaxed);
        let server_socket = match routes.get(&client_addr) {
            Some(socket) => Arc::clone(socket),
            None => {
                let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
                socket.connect(server_addr).await?;
                tracing::info!(
                    client = %client_addr,
                    server_socket = %socket.local_addr()?,
                    "created UDP route"
                );
                tokio::spawn(forward_server_to_client(
                    Arc::clone(&socket),
                    Arc::clone(&listen_socket),
                    client_addr,
                    Arc::clone(&impairment),
                    Arc::clone(&stats),
                ));
                routes.insert(client_addr, Arc::clone(&socket));
                socket
            }
        };

        forward_client_to_server(
            Arc::clone(&server_socket),
            buf[..len].to_vec(),
            Arc::clone(&impairment),
            Arc::clone(&stats),
        );
    }
}

fn forward_client_to_server(
    server_socket: Arc<UdpSocket>,
    packet: Vec<u8>,
    impairment: Arc<Impairment>,
    stats: Arc<ProxyStats>,
) {
    if impairment.should_drop() {
        stats
            .client_to_server_dropped
            .fetch_add(1, Ordering::Relaxed);
        tracing::debug!(direction = "client_to_server", "dropped UDP packet");
        return;
    }

    let delay = impairment.jitter_delay();
    tokio::spawn(async move {
        if delay > Duration::ZERO {
            tokio::time::sleep(delay).await;
        }
        if let Err(err) = server_socket.send(&packet).await {
            tracing::warn!(%err, "failed to forward UDP packet to server");
        } else {
            stats
                .client_to_server_forwarded
                .fetch_add(1, Ordering::Relaxed);
        }
    });
}

async fn forward_server_to_client(
    server_socket: Arc<UdpSocket>,
    listen_socket: Arc<UdpSocket>,
    client_addr: SocketAddr,
    impairment: Arc<Impairment>,
    stats: Arc<ProxyStats>,
) {
    let mut buf = vec![0_u8; 2_048];

    loop {
        let len = match server_socket.recv(&mut buf).await {
            Ok(len) => len,
            Err(err) => {
                tracing::warn!(client = %client_addr, %err, "server route closed");
                return;
            }
        };
        stats
            .server_to_client_received
            .fetch_add(1, Ordering::Relaxed);

        if impairment.should_drop() {
            stats
                .server_to_client_dropped
                .fetch_add(1, Ordering::Relaxed);
            tracing::debug!(direction = "server_to_client", "dropped UDP packet");
            continue;
        }

        let packet = buf[..len].to_vec();
        let socket = Arc::clone(&listen_socket);
        let delay = impairment.jitter_delay();
        let stats = Arc::clone(&stats);
        tokio::spawn(async move {
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
            if let Err(err) = socket.send_to(&packet, client_addr).await {
                tracing::warn!(%client_addr, %err, "failed to forward UDP packet to client");
            } else {
                stats
                    .server_to_client_forwarded
                    .fetch_add(1, Ordering::Relaxed);
            }
        });
    }
}

#[derive(Debug, Default)]
struct ProxyStats {
    client_to_server_received: AtomicU64,
    client_to_server_forwarded: AtomicU64,
    client_to_server_dropped: AtomicU64,
    server_to_client_received: AtomicU64,
    server_to_client_forwarded: AtomicU64,
    server_to_client_dropped: AtomicU64,
}

async fn report_stats(stats: Arc<ProxyStats>, interval: Duration) {
    let mut timer = tokio::time::interval(interval);
    loop {
        timer.tick().await;
        tracing::info!(
            c2s_rx = stats.client_to_server_received.load(Ordering::Relaxed),
            c2s_tx = stats.client_to_server_forwarded.load(Ordering::Relaxed),
            c2s_drop = stats.client_to_server_dropped.load(Ordering::Relaxed),
            s2c_rx = stats.server_to_client_received.load(Ordering::Relaxed),
            s2c_tx = stats.server_to_client_forwarded.load(Ordering::Relaxed),
            s2c_drop = stats.server_to_client_dropped.load(Ordering::Relaxed),
            "netem packet counters"
        );
    }
}

#[derive(Debug)]
struct Impairment {
    drop_probability: f64,
    delay_ms: u64,
    jitter_ms: u64,
    rng: Mutex<Lcg>,
}

impl Impairment {
    fn new(drop_probability: f64, delay_ms: u64, jitter_ms: u64, seed: u64) -> Self {
        Self {
            drop_probability: drop_probability.clamp(0.0, 1.0),
            delay_ms,
            jitter_ms,
            rng: Mutex::new(Lcg::new(seed)),
        }
    }

    fn should_drop(&self) -> bool {
        self.drop_probability > 0.0 && self.next_f64() < self.drop_probability
    }

    fn jitter_delay(&self) -> Duration {
        let delay = self.delay_ms as i64;
        let jitter = self.jitter_ms as i64;
        if jitter == 0 {
            return Duration::from_millis(self.delay_ms);
        }

        let delta = ((self.next_f64() * 2.0 - 1.0) * jitter as f64).round() as i64;
        Duration::from_millis(delay.saturating_add(delta).max(0) as u64)
    }

    fn next_f64(&self) -> f64 {
        self.rng.lock().unwrap().next_f64()
    }
}

#[derive(Debug)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_f64(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((self.state >> 11) as f64) / ((1_u64 << 53) as f64)
    }
}

fn parse_drop_percent(value: &str) -> Result<f64, String> {
    let percent = value
        .parse::<f64>()
        .map_err(|err| format!("invalid drop percent `{value}`: {err}"))?;
    if !(0.0..=100.0).contains(&percent) {
        return Err("drop percent must be between 0 and 100".to_string());
    }
    Ok(percent)
}

fn parse_delay_ms(value: &str) -> Result<u64, String> {
    let delay_ms = value
        .parse::<u64>()
        .map_err(|err| format!("invalid delay milliseconds `{value}`: {err}"))?;
    if delay_ms > 1_000 {
        return Err("delay milliseconds must be between 0 and 1000".to_string());
    }
    Ok(delay_ms)
}

fn parse_jitter_ms(value: &str) -> Result<u64, String> {
    let jitter_ms = value
        .parse::<u64>()
        .map_err(|err| format!("invalid jitter milliseconds `{value}`: {err}"))?;
    if jitter_ms > 1_000 {
        return Err("jitter milliseconds must be between 0 and 1000".to_string());
    }
    Ok(jitter_ms)
}

fn parse_stats_interval_ms(value: &str) -> Result<u64, String> {
    let interval = value
        .parse::<u64>()
        .map_err(|err| format!("invalid stats interval milliseconds `{value}`: {err}"))?;
    if interval > 60_000 {
        return Err("stats interval milliseconds must be between 0 and 60000".to_string());
    }
    Ok(interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impairment_drop_probability_is_clamped() {
        assert_eq!(Impairment::new(-1.0, 0, 0, 1).drop_probability, 0.0);
        assert_eq!(Impairment::new(2.0, 0, 0, 1).drop_probability, 1.0);
    }

    #[test]
    fn zero_jitter_has_no_delay() {
        let impairment = Impairment::new(0.0, 0, 0, 1);

        assert_eq!(impairment.jitter_delay(), Duration::ZERO);
    }

    #[test]
    fn fixed_delay_is_used_without_jitter() {
        let impairment = Impairment::new(0.0, 25, 0, 1);

        assert_eq!(impairment.jitter_delay(), Duration::from_millis(25));
    }

    #[test]
    fn jitter_is_symmetric_around_delay_and_clamped() {
        let impairment = Impairment::new(0.0, 20, 20, 1);

        for _ in 0..64 {
            assert!(impairment.jitter_delay() <= Duration::from_millis(40));
        }

        let impairment = Impairment::new(0.0, 0, 20, 1);
        for _ in 0..64 {
            assert!(impairment.jitter_delay() <= Duration::from_millis(20));
        }
    }

    #[test]
    fn validates_cli_impairment_ranges() {
        assert_eq!(parse_drop_percent("2.5").unwrap(), 2.5);
        assert!(parse_drop_percent("100.1").is_err());
        assert_eq!(parse_delay_ms("250").unwrap(), 250);
        assert!(parse_delay_ms("1001").is_err());
        assert_eq!(parse_jitter_ms("250").unwrap(), 250);
        assert!(parse_jitter_ms("1001").is_err());
        assert_eq!(parse_stats_interval_ms("0").unwrap(), 0);
        assert!(parse_stats_interval_ms("60001").is_err());
    }

    #[tokio::test]
    async fn proxy_forwards_packets_both_directions_and_counts_them() {
        let listen_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let proxy_addr = listen_socket.local_addr().unwrap();
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let stats = Arc::new(ProxyStats::default());

        let proxy_task = tokio::spawn(run_proxy(
            Arc::clone(&listen_socket),
            server_addr,
            Arc::new(Impairment::new(0.0, 0, 0, 1)),
            Arc::clone(&stats),
        ));

        client.send_to(b"hello", proxy_addr).await.unwrap();
        let mut buf = [0_u8; 64];
        let (len, route_addr) =
            tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buf[..len], b"hello");

        server.send_to(b"world", route_addr).await.unwrap();
        let (len, from_addr) =
            tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(from_addr, proxy_addr);
        assert_eq!(&buf[..len], b"world");

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(stats.client_to_server_received.load(Ordering::Relaxed), 1);
        assert_eq!(stats.client_to_server_forwarded.load(Ordering::Relaxed), 1);
        assert_eq!(stats.server_to_client_received.load(Ordering::Relaxed), 1);
        assert_eq!(stats.server_to_client_forwarded.load(Ordering::Relaxed), 1);

        proxy_task.abort();
    }

    #[tokio::test]
    async fn proxy_drop_rule_prevents_forwarding_and_counts_drop() {
        let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let stats = Arc::new(ProxyStats::default());

        forward_client_to_server(
            server_socket,
            b"dropped".to_vec(),
            Arc::new(Impairment::new(1.0, 0, 0, 1)),
            Arc::clone(&stats),
        );

        let mut buf = [0_u8; 64];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), server.recv_from(&mut buf))
                .await
                .is_err()
        );
        assert_eq!(stats.client_to_server_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(stats.client_to_server_forwarded.load(Ordering::Relaxed), 0);
    }
}
