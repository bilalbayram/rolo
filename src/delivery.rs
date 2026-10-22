//! Webhook delivery worker. Fans events out from the `events` outbox to
//! subscribed webhooks, then POSTs them signed per the Standard Webhooks spec
//! (<https://www.standardwebhooks.com/>), retrying failures with backoff.
//!
//! Run one server per database: the worker assumes it is the only one sending.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use hmac::Hmac;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::CONTENT_TYPE;
use sha2::Sha256;
use sqlx::types::Json;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::error::Result;
use crate::service::events::Event;
use crate::service::webhooks::DeliveryStatus;
use crate::util;

/// Attempts per delivery before it is marked failed.
const MAX_ATTEMPTS: i64 = 8;

/// Wait after failed attempt N (1-based) before the next one; about 21 hours in total.
const BACKOFF: [Duration; 7] = [
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(2 * 60 * 60),
    Duration::from_secs(6 * 60 * 60),
    Duration::from_secs(12 * 60 * 60),
];

/// Deliveries attempted concurrently per tick.
const DELIVERY_BATCH: i64 = 32;

/// Events scanned per webhook per tick when fanning out.
const FAN_OUT_BATCH: i64 = 1000;

pub struct WorkerConfig {
    /// Allow destinations on loopback, private and other non-public networks.
    /// Off by default so a webhook cannot be used to reach internal services.
    pub allow_private_destinations: bool,
    pub poll_interval: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            allow_private_destinations: false,
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Start the worker. It stops after `shutdown` is cancelled, once the current
/// batch finishes.
pub fn spawn(
    db: Db,
    config: WorkerConfig,
    shutdown: CancellationToken,
) -> anyhow::Result<JoinHandle<()>> {
    let sender = Sender::new(config.allow_private_destinations)?;
    Ok(tokio::spawn(run(
        db,
        sender,
        config.poll_interval,
        shutdown,
    )))
}

async fn run(db: Db, sender: Sender, poll_interval: Duration, shutdown: CancellationToken) {
    loop {
        if let Err(err) = fan_out(&db).await {
            tracing::error!(error = ?err, "webhook fan-out failed");
        }
        if let Err(err) = deliver_due(&db, &sender).await {
            tracing::error!(error = ?err, "webhook delivery failed");
        }
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = tokio::time::sleep(poll_interval) => {}
        }
    }
}

/// Create pending deliveries for events past each enabled webhook's cursor.
async fn fan_out(db: &Db) -> Result<()> {
    let behind = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM webhooks
               WHERE enabled = 1 AND cursor_seq < (SELECT coalesce(max(seq), 0) FROM events)
           ) AS "behind!: bool""#,
    )
    .fetch_one(&db.reader)
    .await?;
    if !behind {
        return Ok(());
    }

    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    let webhooks = sqlx::query!(
        r#"SELECT id, event_types AS "event_types: Json<Vec<String>>", cursor_seq
           FROM webhooks WHERE enabled = 1"#,
    )
    .fetch_all(&mut *tx)
    .await?;
    for webhook in webhooks {
        let events = sqlx::query!(
            "SELECT seq, type AS event_type FROM events WHERE seq > ? ORDER BY seq LIMIT ?",
            webhook.cursor_seq,
            FAN_OUT_BATCH,
        )
        .fetch_all(&mut *tx)
        .await?;
        let Some(last_seq) = events.last().map(|event| event.seq) else {
            continue;
        };
        let matching = events
            .iter()
            .filter(|event| is_subscribed(&webhook.event_types, &event.event_type));
        for event in matching {
            let id = util::new_id("dlv");
            sqlx::query!(
                "INSERT OR IGNORE INTO webhook_deliveries
                     (id, webhook_id, event_seq, status, next_attempt_at, created_at, updated_at)
                 VALUES (?, ?, ?, 'pending', ?, ?, ?)",
                id,
                webhook.id,
                event.seq,
                now,
                now,
                now,
            )
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query!(
            "UPDATE webhooks SET cursor_seq = ? WHERE id = ?",
            last_seq,
            webhook.id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Empty `prefixes` subscribes to everything.
fn is_subscribed(prefixes: &[String], event_type: &str) -> bool {
    prefixes.is_empty()
        || prefixes
            .iter()
            .any(|prefix| event_type.starts_with(prefix.as_str()))
}

struct DueDelivery {
    id: String,
    attempts: i64,
    url: String,
    secret: String,
    event: Event,
}

struct AttemptResult {
    delivery_id: String,
    previous_attempts: i64,
    outcome: Outcome,
}

enum Outcome {
    Delivered { status: u16 },
    Failed { status: Option<u16>, error: String },
}

async fn deliver_due(db: &Db, sender: &Sender) -> Result<()> {
    let now = Utc::now();
    let rows = sqlx::query!(
        r#"SELECT d.id, d.attempts, w.url, w.secret,
                  e.seq, e.id AS event_id, e.type AS event_type, e.entity_id,
                  e.data AS "data: Json<serde_json::Value>",
                  e.occurred_at AS "occurred_at: DateTime<Utc>"
           FROM webhook_deliveries d
           JOIN webhooks w ON w.id = d.webhook_id
           JOIN events e ON e.seq = d.event_seq
           WHERE d.status = 'pending' AND d.next_attempt_at <= ? AND w.enabled = 1
           ORDER BY d.next_attempt_at, d.event_seq
           LIMIT ?"#,
        now,
        DELIVERY_BATCH,
    )
    .fetch_all(&db.reader)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }

    let mut attempts = JoinSet::new();
    for row in rows {
        let due = DueDelivery {
            id: row.id,
            attempts: row.attempts,
            url: row.url,
            secret: row.secret,
            event: Event {
                seq: row.seq,
                id: row.event_id,
                event_type: row.event_type,
                entity_id: row.entity_id,
                data: row.data,
                occurred_at: row.occurred_at,
            },
        };
        let sender = sender.clone();
        attempts.spawn(async move {
            let outcome = sender.send(&due.url, &due.secret, &due.event).await;
            AttemptResult {
                delivery_id: due.id,
                previous_attempts: due.attempts,
                outcome,
            }
        });
    }
    let results = attempts.join_all().await;
    record_results(db, results).await
}

async fn record_results(db: &Db, results: Vec<AttemptResult>) -> Result<()> {
    let now = Utc::now();
    let mut tx = db.writer.begin().await?;
    for result in results {
        let attempts = result.previous_attempts + 1;
        match result.outcome {
            Outcome::Delivered { status } => {
                let status = i64::from(status);
                sqlx::query!(
                    "UPDATE webhook_deliveries
                     SET status = 'succeeded', attempts = ?, next_attempt_at = NULL,
                         last_status_code = ?, last_error = NULL, delivered_at = ?, updated_at = ?
                     WHERE id = ?",
                    attempts,
                    status,
                    now,
                    now,
                    result.delivery_id,
                )
                .execute(&mut *tx)
                .await?;
            }
            Outcome::Failed { status, error } => {
                let next_attempt_at = next_attempt_at(attempts, now);
                let delivery_status = match next_attempt_at {
                    Some(_) => DeliveryStatus::Pending,
                    None => DeliveryStatus::Failed,
                };
                let status = status.map(i64::from);
                tracing::warn!(delivery = %result.delivery_id, attempts, %error, "webhook delivery failed");
                sqlx::query!(
                    "UPDATE webhook_deliveries
                     SET status = ?, attempts = ?, next_attempt_at = ?,
                         last_status_code = ?, last_error = ?, updated_at = ?
                     WHERE id = ?",
                    delivery_status,
                    attempts,
                    next_attempt_at,
                    status,
                    error,
                    now,
                    result.delivery_id,
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// `None` once `attempts` reaches the limit.
fn next_attempt_at(attempts: i64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if attempts >= MAX_ATTEMPTS {
        return None;
    }
    let index = usize::try_from(attempts - 1)
        .unwrap_or(0)
        .min(BACKOFF.len() - 1);
    let delay = chrono::Duration::from_std(BACKOFF[index]).expect("backoff delays fit in chrono");
    Some(now + delay)
}

/// Standard Webhooks signature header value: `v1,<base64 HMAC-SHA256>` over
/// `{message_id}.{timestamp}.{body}`, keyed by the base64 part of the `whsec_` secret.
pub fn sign(secret: &str, message_id: &str, timestamp: i64, body: &[u8]) -> anyhow::Result<String> {
    use base64::Engine as _;
    use hmac::{KeyInit as _, Mac as _};

    let key = BASE64
        .decode(secret.strip_prefix("whsec_").unwrap_or(secret))
        .context("webhook secret is not valid base64")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).context("invalid webhook secret length")?;
    mac.update(format!("{message_id}.{timestamp}.").as_bytes());
    mac.update(body);
    Ok(format!("v1,{}", BASE64.encode(mac.finalize().into_bytes())))
}

#[derive(Clone)]
struct Sender {
    client: reqwest::Client,
    allow_private_destinations: bool,
}

impl Sender {
    fn new(allow_private_destinations: bool) -> anyhow::Result<Self> {
        // reqwest is built without a bundled crypto provider (the default,
        // aws-lc, needs cmake to build), so install ring. This fails harmlessly
        // if a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("rolo/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            // Redirects and proxies would bypass the destination check.
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if !allow_private_destinations {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build().context("building webhook HTTP client")?;
        Ok(Self {
            client,
            allow_private_destinations,
        })
    }

    async fn send(&self, url: &str, secret: &str, event: &Event) -> Outcome {
        match self.try_send(url, secret, event).await {
            Ok(outcome) => outcome,
            Err(err) => Outcome::Failed {
                status: None,
                error: format!("{err:#}"),
            },
        }
    }

    async fn try_send(&self, url: &str, secret: &str, event: &Event) -> anyhow::Result<Outcome> {
        if !self.allow_private_destinations
            && let Some(ip) = literal_ip(url)
            && !is_public(ip)
        {
            anyhow::bail!(blocked_message(&ip.to_string()));
        }
        let body = serde_json::to_vec(event)?;
        let timestamp = Utc::now().timestamp();
        let signature = sign(secret, &event.id, timestamp, &body)?;
        let response = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header("webhook-id", &event.id)
            .header("webhook-timestamp", timestamp.to_string())
            .header("webhook-signature", signature)
            .body(body)
            .send()
            .await
            .map_err(|err| anyhow::anyhow!(error_chain(&err)))?;
        let status = response.status();
        if status.is_success() {
            Ok(Outcome::Delivered {
                status: status.as_u16(),
            })
        } else {
            Ok(Outcome::Failed {
                status: Some(status.as_u16()),
                error: format!("endpoint responded {status}"),
            })
        }
    }
}

/// reqwest's top-level error omits the cause (DNS, TLS, connection refused).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    message
}

fn blocked_message(host: &str) -> String {
    format!(
        "blocked: {host} is a private or reserved address; \
         start rolo with --allow-private-webhooks to deliver to internal hosts"
    )
}

/// The URL's host if it is an IP literal (which skips DNS resolution).
fn literal_ip(url: &str) -> Option<IpAddr> {
    let url = reqwest::Url::parse(url).ok()?;
    let host = url.host_str()?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

/// Resolves hostnames but only returns public addresses. Checking inside the
/// resolver (rather than before the request) means a hostname cannot pass the
/// check and then resolve to an internal address at connect time.
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public: Vec<SocketAddr> =
                resolved.filter(|address| is_public(address.ip())).collect();
            if public.is_empty() {
                return Err(blocked_message(&host).into());
            }
            let addresses: Addrs = Box::new(public.into_iter());
            Ok(addresses)
        })
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => match embedded_v4(ip) {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(ip),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let reserved = a == 0
        // 100.64.0.0/10: carrier-grade NAT
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24: IETF protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15: benchmarking
        || (a == 198 && (18..20).contains(&b))
        // 240.0.0.0/4: reserved
        || a >= 240;
    !(reserved
        || ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast())
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || unique_local
        || link_local
        || documentation)
}

/// IPv4 address carried in an IPv4-mapped (`::ffff:a.b.c.d`) or NAT64
/// (`64:ff9b::a.b.c.d`) IPv6 address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let segments = ip.segments();
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., high, low] = segments;
        return Some(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_addresses() {
        let public = ["8.8.8.8", "1.1.1.1", "2606:4700::1111", "64:ff9b::808:808"];
        let private = [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
        ];
        for address in public {
            assert!(
                is_public(address.parse().expect("valid IP")),
                "{address} should be public"
            );
        }
        for address in private {
            assert!(
                !is_public(address.parse().expect("valid IP")),
                "{address} should be private"
            );
        }
    }

    #[test]
    fn signs_per_standard_webhooks() {
        // Test vector from the Standard Webhooks reference implementation.
        let signature = sign(
            "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw",
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            1614265330,
            br#"{"test": 2432232314}"#,
        )
        .expect("valid secret");
        assert_eq!(signature, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
    }

    #[test]
    fn backs_off_then_gives_up() {
        let now = Utc::now();
        assert_eq!(
            next_attempt_at(1, now),
            Some(now + chrono::Duration::seconds(10))
        );
        assert_eq!(
            next_attempt_at(7, now),
            Some(now + chrono::Duration::hours(12))
        );
        assert_eq!(next_attempt_at(MAX_ATTEMPTS, now), None);
    }
}
