use tracing::*;

use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use static_init::dynamic;
use std::net::IpAddr;

/// How long a single ban lasts before the peer is allowed to reconnect.
const BAN_DURATION_HOURS: i64 = 1;
/// How many ordinary strikes within the window below escalate to a ban.
/// Keeps an occasional honest mistake (e.g. a miner submitting a template
/// that went stale mid-mine) from being punished, while still catching a
/// peer that keeps sending bad data.
const MAX_STRIKES: u32 = 3;
const STRIKE_WINDOW_MINUTES: i64 = 10;

#[dynamic]
static BANNED: DashMap<IpAddr, DateTime<Utc>> = DashMap::new();

#[dynamic]
static STRIKES: DashMap<IpAddr, (u32, DateTime<Utc>)> = DashMap::new();

/// Loads bans persisted from a previous run into the in-memory ban list.
/// Call once at startup, after `BLOCK_STORE` is initialized -- without
/// this, every ban would reset for free the moment a node restarts for
/// any reason (crash, upgrade, routine ops), handing a banned peer back
/// its access at zero cost.
pub fn load_persisted(store: &btclib::store::BlockStore) {
    let bans = match store.load_bans() {
        Ok(bans) => bans,
        Err(e) => {
            println!("failed to load persisted bans: {e}");
            return;
        }
    };
    let now = Utc::now();
    let mut restored = 0;
    for (ip_str, until_unix) in bans {
        let (Ok(ip), Some(until)) = (ip_str.parse::<IpAddr>(), DateTime::from_timestamp(until_unix, 0)) else {
            continue;
        };
        if until > now {
            BANNED.insert(ip, until);
            restored += 1;
        }
    }
    if restored > 0 {
        println!("restored {restored} ban(s) from a previous run");
    }
}

/// Whether `ip` is currently banned. Expired bans are pruned as a side
/// effect of checking them.
pub fn is_banned(ip: IpAddr) -> bool {
    if let Some(entry) = BANNED.get(&ip) {
        if *entry.value() > Utc::now() {
            return true;
        }
        drop(entry);
        BANNED.remove(&ip);
    }
    false
}

/// Records a protocol violation from `ip`.
///
/// `severe` violations -- garbage/oversized data, a failed handshake, or
/// a message sent completely out of protocol -- ban immediately, since no
/// legitimate client ever does these by accident.
///
/// Non-severe violations -- a block or transaction that failed content
/// validation -- only accumulate a strike. These *can* occasionally
/// happen to an honest peer (e.g. a race with a difficulty retarget), so
/// only a sustained pattern (several within a short window) escalates to
/// a ban.
pub fn strike(ip: IpAddr, severe: bool) {
    if severe {
        ban_now(ip);
        return;
    }

    let now = Utc::now();
    let mut entry = STRIKES.entry(ip).or_insert((0, now));
    if now - entry.1 > Duration::minutes(STRIKE_WINDOW_MINUTES) {
        *entry = (0, now);
    }
    entry.0 += 1;
    let count = entry.0;
    drop(entry);

    if count >= MAX_STRIKES {
        STRIKES.remove(&ip);
        ban_now(ip);
    }
}

fn ban_now(ip: IpAddr) {
    let until = Utc::now() + Duration::hours(BAN_DURATION_HOURS);
    println!("banning peer {ip} until {until}");
    BANNED.insert(ip, until);
    // Best-effort: if this fails (or the store isn't up yet), the ban
    // still applies for the rest of this process's life, it just won't
    // survive a restart.
    if let Some(store) = crate::BLOCK_STORE.get() {
        if let Err(e) = store.save_ban(&ip.to_string(), until.timestamp()) {
            println!("failed to persist ban for {ip}: {e}");
        }
    }
}
