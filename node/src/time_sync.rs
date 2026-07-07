use tracing::*;

use chrono::Duration;
use dashmap::DashMap;
use static_init::dynamic;
use std::net::IpAddr;

/// How many distinct peers' time samples we keep at once. Bounded so a
/// long-lived, well-connected node doesn't grow this forever.
const MAX_SAMPLES: usize = 50;
/// Need at least this many independent samples before trusting a
/// network-adjusted time at all. Each peer contributes at most one
/// sample (keyed by IP, overwritten on reconnect) no matter how many
/// times it reconnects, so this also means an attacker needs multiple
/// distinct IPs to influence the median at all, not just one chatty
/// connection.
const MIN_SAMPLES_TO_ADJUST: usize = 5;
/// If the median peer-reported offset from our own clock is larger than
/// this, refuse to apply it and fall back to trusting our own clock
/// outright. This is what stops a Sybil of malicious peers from dragging
/// a victim's adjusted time arbitrarily far -- mirrors Bitcoin Core's
/// DEFAULT_MAX_TIME_ADJUSTMENT (70 minutes).
const MAX_TIME_ADJUSTMENT_MINUTES: i64 = 70;

#[dynamic]
static SAMPLES: DashMap<IpAddr, Duration> = DashMap::new();

/// Records a new peer time-offset sample (their reported clock minus
/// ours, taken once per successful handshake) and recomputes the
/// network-adjusted offset applied to block timestamp validation.
///
/// A single sample is never trusted alone -- this is only ever one input
/// to a median across every peer we've sampled, and that median itself is
/// ignored if it's implausibly large (see `MAX_TIME_ADJUSTMENT_MINUTES`).
pub async fn record_sample(ip: IpAddr, offset: Duration) {
    if SAMPLES.len() >= MAX_SAMPLES && !SAMPLES.contains_key(&ip) {
        // at capacity with a brand new peer -- keep the samples we
        // already have rather than growing this without bound
        return;
    }
    SAMPLES.insert(ip, offset);

    let applied = match median_offset() {
        None => Duration::zero(),
        Some(median) if median.num_minutes().abs() > MAX_TIME_ADJUSTMENT_MINUTES => {
            println!(
                "peer time offset {median} exceeds the {MAX_TIME_ADJUSTMENT_MINUTES}-minute \
                 safety bound; ignoring network time and trusting this node's own clock. \
                 Check your system clock if this persists."
            );
            Duration::zero()
        }
        Some(median) => median,
    };

    let mut blockchain = crate::BLOCKCHAIN.write().await;
    blockchain.set_time_offset(applied);
}

fn median_offset() -> Option<Duration> {
    let secs: Vec<i64> = SAMPLES.iter().map(|e| e.value().num_seconds()).collect();
    median_of_seconds(&secs)
}

/// The actual median/minimum-sample-count algorithm, pulled out of
/// `median_offset` so it's testable against synthetic data without
/// touching the shared global sample table.
fn median_of_seconds(secs: &[i64]) -> Option<Duration> {
    if secs.len() < MIN_SAMPLES_TO_ADJUST {
        return None;
    }
    let mut sorted = secs.to_vec();
    sorted.sort_unstable();
    Some(Duration::seconds(sorted[sorted.len() / 2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_seconds_requires_a_minimum_sample_count() {
        assert_eq!(median_of_seconds(&[1, 2, 3]), None);
    }

    #[test]
    fn median_of_seconds_computes_the_middle_value() {
        // sorted: -5, 3, 7, 10, 100 -> median is 7
        assert_eq!(
            median_of_seconds(&[10, -5, 3, 100, 7]),
            Some(Duration::seconds(7))
        );
    }
}
