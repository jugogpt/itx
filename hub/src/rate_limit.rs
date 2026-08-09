use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use std::net::{IpAddr, SocketAddr};

/// How often a client's request count resets.
const WINDOW_SECONDS: i64 = 60;
/// Requests allowed per client per window -- generous for a legitimate
/// agent (a market-making bot polling every few seconds, a dashboard
/// refreshing) while still bounding how much load one client can throw
/// at the process. A first, deliberately simple pass: one limit across
/// every route, not tiered by endpoint cost. `pub(crate)` so the
/// end-to-end HTTP test in `main.rs` can drive exactly this many requests
/// rather than hardcoding a second copy of the number.
pub(crate) const MAX_REQUESTS_PER_WINDOW: u32 = 120;

pub struct Window {
    started_at: DateTime<Utc>,
    count: u32,
}

/// Per-client request tracking. Deliberately a field on `AppState`
/// (constructed fresh per hub instance), *not* a process-wide
/// `#[dynamic] static` like `auth::SEEN_SIGNATURES` -- unlike a
/// signature (unique per request by construction), a client IP is not,
/// and every hub test instance in this workspace's test suite runs on
/// 127.0.0.1. A global static here would mean every test hub sharing
/// one rate-limit bucket and spuriously tripping each other's limits;
/// scoping it to `AppState` gives each hub instance (real or test) its
/// own independent table, the same reasoning `payout_lock` and `board`
/// are already instance-scoped rather than global.
pub type RateLimitTable = DashMap<IpAddr, Window>;

pub fn new_table() -> RateLimitTable {
    DashMap::new()
}

/// Records one request from `ip` and reports whether it's still within
/// the limit. Fixed-window, not sliding -- simple, and good enough for
/// "stop a naive flood," not meant to be a precise leaky-bucket.
fn check_and_record(table: &RateLimitTable, ip: IpAddr, now: DateTime<Utc>) -> bool {
    let mut entry = table.entry(ip).or_insert_with(|| Window { started_at: now, count: 0 });
    if now - entry.started_at > Duration::seconds(WINDOW_SECONDS) {
        entry.started_at = now;
        entry.count = 0;
    }
    entry.count += 1;
    entry.count <= MAX_REQUESTS_PER_WINDOW
}

/// Evicts entries whose window has already lapsed -- keeps this from
/// growing forever on a long-running process. Call periodically from
/// the sweep loop, mirroring `auth::cleanup_replay_guard`.
pub fn cleanup(table: &RateLimitTable) {
    let cutoff = Utc::now() - Duration::seconds(WINDOW_SECONDS);
    table.retain(|_, w| w.started_at > cutoff);
}

/// Prefers `X-Forwarded-For` (set by a reverse proxy) so rate limiting
/// still targets real clients when deployed behind one, rather than
/// uniformly limiting the proxy's own IP for every request it forwards.
/// Falls back to the direct TCP peer address for a bare, un-proxied
/// deployment (or a client that lied about its own header, in which
/// case it's only limiting itself).
fn client_ip(req: &Request<Body>, connect_addr: SocketAddr) -> IpAddr {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .unwrap_or_else(|| connect_addr.ip())
}

/// Registered via `middleware::from_fn_with_state` with the *same*
/// `Arc<AppState>` the router itself uses -- no separate state to keep
/// in sync, `state.rate_limits` is the single source of truth either
/// way a handler or this middleware reaches it.
pub async fn middleware(
    State(state): State<std::sync::Arc<crate::AppState>>,
    ConnectInfo(connect_addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&req, connect_addr);
    if !check_and_record(&state.rate_limits, ip, Utc::now()) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded, slow down").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_within_the_limit_and_rejects_beyond_it() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, ip, now));
        }
        assert!(!check_and_record(&table, ip, now), "one past the limit must be rejected");
    }

    #[test]
    fn resets_after_the_window_elapses() {
        let table = new_table();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, ip, now));
        }
        assert!(!check_and_record(&table, ip, now));

        let later = now + Duration::seconds(WINDOW_SECONDS + 1);
        assert!(check_and_record(&table, ip, later), "a new window must start fresh");
    }

    #[test]
    fn tracks_different_ips_independently() {
        let table = new_table();
        let a: IpAddr = "127.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Utc::now();
        for _ in 0..MAX_REQUESTS_PER_WINDOW {
            assert!(check_and_record(&table, a, now));
        }
        assert!(!check_and_record(&table, a, now));
        assert!(check_and_record(&table, b, now), "a different IP must not be affected by another's limit");
    }

    #[test]
    fn cleanup_evicts_only_lapsed_windows() {
        let table = new_table();
        let stale: IpAddr = "127.0.0.1".parse().unwrap();
        let fresh: IpAddr = "10.0.0.1".parse().unwrap();
        let now = Utc::now();
        check_and_record(&table, stale, now - Duration::seconds(WINDOW_SECONDS + 5));
        check_and_record(&table, fresh, now);

        cleanup(&table);

        assert!(table.get(&stale).is_none());
        assert!(table.get(&fresh).is_some());
    }
}
