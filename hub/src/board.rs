use tracing::*;

use btclib::crypto::PublicKey;
use btclib::sha256::Hash;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum BoardError {
    #[error("task not found")]
    NotFound,
    #[error("task is not open")]
    NotOpen,
    #[error("task is not claimed")]
    NotClaimed,
    #[error("you are not the claimant of this task")]
    NotClaimant,
    #[error("task has not been verified yet")]
    NotVerified,
    #[error("this pubkey has already claimed a faucet grant")]
    AlreadyClaimed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Open,
    Claimed,
    /// Submitted output matched the expected hash; payout is in flight
    /// but not yet confirmed submitted to the chain.
    Verified,
    Paid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    /// The deterministic verification spec for this task: the submitted
    /// output's SHA256 must equal this. This is Phase B's one supported
    /// verification tier -- objectively checkable compute/data jobs.
    /// Redundant-assignment-with-staking for open-ended tasks is a later
    /// phase.
    pub expected_output_hash: Hash,
    pub poster: PublicKey,
    pub status: TaskStatus,
    pub claimant: Option<PublicKey>,
    pub claim_deadline: Option<DateTime<Utc>>,
    pub failed_attempts: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reputation {
    pub completed: u64,
    pub failed: u64,
    pub total_earned: u64,
}

/// Pure in-memory task-marketplace state: no I/O, no knowledge of the
/// blockchain or HTTP -- mirrors how `Blockchain` itself is a pure data
/// structure the node crate drives. `HubStore` is this module's
/// equivalent of `BlockStore`, and the HTTP handlers are this module's
/// equivalent of `node`'s message handlers: they own actually paying
/// people (an on-chain operation `TaskBoard` has no concept of) and call
/// back in here only to record the outcome.
#[derive(Debug, Clone, Default)]
pub struct TaskBoard {
    tasks: BTreeMap<Uuid, Task>,
    reputation: BTreeMap<PublicKey, Reputation>,
    faucet_grants: BTreeSet<PublicKey>,
}

impl TaskBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total bounty already promised to tasks that haven't been paid out
    /// yet. Callers use this against the operator's actual on-chain
    /// balance to decide whether a new task can be safely funded.
    pub fn allocated_bounty(&self) -> u64 {
        self.tasks
            .values()
            .filter(|t| t.status != TaskStatus::Paid)
            .map(|t| t.bounty)
            .sum()
    }

    pub fn create_task(
        &mut self,
        poster: PublicKey,
        description: String,
        bounty: u64,
        expected_output_hash: Hash,
    ) -> Task {
        let task = Task {
            id: Uuid::new_v4(),
            description,
            bounty,
            expected_output_hash,
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Restores a task exactly as previously persisted -- used only by
    /// `HubStore` when loading from disk, since `create_task` always
    /// mints a fresh id/timestamp.
    pub fn restore_task(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    /// Restores a reputation record previously persisted by `HubStore`.
    pub fn restore_reputation(&mut self, pubkey: PublicKey, reputation: Reputation) {
        self.reputation.insert(pubkey, reputation);
    }

    /// Restores a faucet grant previously persisted by `HubStore`.
    pub fn restore_faucet_grant(&mut self, pubkey: PublicKey) {
        self.faucet_grants.insert(pubkey);
    }

    pub fn get_task(&self, id: Uuid) -> Option<&Task> {
        self.tasks.get(&id)
    }

    pub fn list_open_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Open)
            .collect()
    }

    pub fn claim_task(
        &mut self,
        id: Uuid,
        claimant: PublicKey,
        deadline: DateTime<Utc>,
    ) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if task.status != TaskStatus::Open {
            return Err(BoardError::NotOpen);
        }
        task.status = TaskStatus::Claimed;
        task.claimant = Some(claimant);
        task.claim_deadline = Some(deadline);
        Ok(())
    }

    /// Checks `output_hash` against the task's verification spec. Returns
    /// whether it matched. On a mismatch, the task reopens for another
    /// attempt (by anyone, including the same agent) and the submitter
    /// takes a reputation hit; on a match, the task moves to `Verified`
    /// (the caller is expected to then actually pay out and call
    /// `mark_paid`).
    pub fn submit(
        &mut self,
        id: Uuid,
        submitter: PublicKey,
        output_hash: Hash,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        if task.claimant.as_ref() != Some(&submitter) {
            return Err(BoardError::NotClaimant);
        }

        if output_hash == task.expected_output_hash {
            task.status = TaskStatus::Verified;
            Ok(true)
        } else {
            task.status = TaskStatus::Open;
            task.claimant = None;
            task.claim_deadline = None;
            task.failed_attempts += 1;
            self.reputation.entry(submitter).or_default().failed += 1;
            Ok(false)
        }
    }

    /// Records that a verified task's bounty was successfully paid out
    /// on-chain. Split from `submit` so a transient payout failure never
    /// silently credits reputation for a payment that didn't actually
    /// happen -- the caller only calls this once the payment is
    /// confirmed sent.
    pub fn mark_paid(&mut self, id: Uuid) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if task.status != TaskStatus::Verified {
            return Err(BoardError::NotVerified);
        }
        task.status = TaskStatus::Paid;
        if let Some(claimant) = task.claimant.clone() {
            let rep = self.reputation.entry(claimant).or_default();
            rep.completed += 1;
            rep.total_earned += task.bounty;
        }
        Ok(())
    }

    /// Reopens any `Claimed` task whose deadline has passed, so an
    /// abandoned claim doesn't sit locked forever. Returns the ids that
    /// were reopened. Call periodically from a background sweep.
    pub fn expire_claims(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut reopened = Vec::new();
        for task in self.tasks.values_mut() {
            if task.status == TaskStatus::Claimed {
                if let Some(deadline) = task.claim_deadline {
                    if now > deadline {
                        task.status = TaskStatus::Open;
                        task.claimant = None;
                        task.claim_deadline = None;
                        reopened.push(task.id);
                    }
                }
            }
        }
        reopened
    }

    pub fn reputation(&self, pubkey: &PublicKey) -> Reputation {
        self.reputation.get(pubkey).cloned().unwrap_or_default()
    }

    pub fn leaderboard(&self, top_n: usize) -> Vec<(PublicKey, Reputation)> {
        let mut entries: Vec<_> = self
            .reputation
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.sort_by(|a, b| b.1.total_earned.cmp(&a.1.total_earned));
        entries.truncate(top_n);
        entries
    }

    /// Whether `pubkey` is still eligible for a faucet grant. Read-only
    /// on purpose -- see `record_faucet_grant`.
    pub fn can_claim_faucet(&self, pubkey: &PublicKey) -> bool {
        !self.faucet_grants.contains(pubkey)
    }

    /// Atomically reserves the one grant `pubkey` is entitled to (fails if
    /// it's already been reserved or granted). Callers should reserve
    /// BEFORE attempting the on-chain payout -- that's what makes two
    /// concurrent claims from the same pubkey safe -- and call
    /// `revoke_faucet_grant` to release the reservation if the payout
    /// then fails, so a transient failure doesn't permanently lock the
    /// agent out of a grant it never actually received.
    pub fn record_faucet_grant(&mut self, pubkey: PublicKey) -> Result<(), BoardError> {
        if !self.faucet_grants.insert(pubkey) {
            return Err(BoardError::AlreadyClaimed);
        }
        Ok(())
    }

    /// Releases a faucet-grant reservation made by `record_faucet_grant`.
    /// Only call this when the payout that was supposed to follow the
    /// reservation actually failed.
    pub fn revoke_faucet_grant(&mut self, pubkey: &PublicKey) {
        self.faucet_grants.remove(pubkey);
    }

    pub fn all_tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    pub fn all_reputation(&self) -> impl Iterator<Item = (&PublicKey, &Reputation)> {
        self.reputation.iter()
    }

    pub fn all_faucet_grants(&self) -> impl Iterator<Item = &PublicKey> {
        self.faucet_grants.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use btclib::crypto::PrivateKey;

    fn pubkey() -> PublicKey {
        PrivateKey::new_key().public_key()
    }

    #[test]
    fn full_task_lifecycle_pays_out_and_updates_reputation() {
        let mut board = TaskBoard::new();
        let poster = pubkey();
        let worker = pubkey();
        let expected = Hash::hash_bytes(b"the correct answer");

        let task = board.create_task(poster, "add 2+2".to_string(), 100, expected);
        assert_eq!(board.list_open_tasks().len(), 1);
        assert_eq!(board.allocated_bounty(), 100);

        board
            .claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10))
            .unwrap();
        assert!(board.list_open_tasks().is_empty());

        // wrong answer: reopens, dings reputation, does NOT pay
        let wrong = Hash::hash_bytes(b"a wrong answer");
        assert!(!board.submit(task.id, worker.clone(), wrong).unwrap());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
        assert_eq!(board.reputation(&worker).failed, 1);

        // claim again and submit correctly
        board
            .claim_task(task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(10))
            .unwrap();
        assert!(board.submit(task.id, worker.clone(), expected).unwrap());
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Verified);

        // not paid/credited until mark_paid is called
        assert_eq!(board.reputation(&worker).completed, 0);
        board.mark_paid(task.id).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.reputation(&worker).completed, 1);
        assert_eq!(board.reputation(&worker).total_earned, 100);
        assert_eq!(board.allocated_bounty(), 0);
    }

    #[test]
    fn only_the_claimant_can_submit() {
        let mut board = TaskBoard::new();
        let expected = Hash::hash_bytes(b"answer");
        let task = board.create_task(pubkey(), "task".to_string(), 10, expected);
        let claimant = pubkey();
        let impostor = pubkey();
        board
            .claim_task(task.id, claimant, Utc::now() + chrono::Duration::minutes(5))
            .unwrap();

        assert!(matches!(
            board.submit(task.id, impostor, expected),
            Err(BoardError::NotClaimant)
        ));
    }

    #[test]
    fn cannot_claim_an_already_claimed_task() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "task".to_string(), 10, Hash::hash_bytes(b"x"));
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        board.claim_task(task.id, pubkey(), deadline).unwrap();

        assert!(matches!(
            board.claim_task(task.id, pubkey(), deadline),
            Err(BoardError::NotOpen)
        ));
    }

    #[test]
    fn abandoned_claims_expire_back_to_open() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "task".to_string(), 10, Hash::hash_bytes(b"x"));
        let now = Utc::now();
        board.claim_task(task.id, pubkey(), now + chrono::Duration::seconds(1)).unwrap();

        // not expired yet
        assert!(board.expire_claims(now).is_empty());

        // now it is
        let later = now + chrono::Duration::seconds(2);
        let reopened = board.expire_claims(later);
        assert_eq!(reopened, vec![task.id]);
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
    }

    #[test]
    fn faucet_grants_are_one_per_pubkey() {
        let mut board = TaskBoard::new();
        let agent = pubkey();
        assert!(board.can_claim_faucet(&agent));
        board.record_faucet_grant(agent.clone()).unwrap();
        assert!(!board.can_claim_faucet(&agent));
        assert!(matches!(
            board.record_faucet_grant(agent),
            Err(BoardError::AlreadyClaimed)
        ));
    }

    #[test]
    fn leaderboard_sorts_by_total_earned_descending() {
        let mut board = TaskBoard::new();
        let low = pubkey();
        let high = pubkey();

        for (agent, bounty) in [(&low, 10u64), (&high, 500u64)] {
            let expected = Hash::hash_bytes(b"x");
            let task = board.create_task(pubkey(), "t".to_string(), bounty, expected);
            board
                .claim_task(task.id, agent.clone(), Utc::now() + chrono::Duration::minutes(5))
                .unwrap();
            board.submit(task.id, agent.clone(), expected).unwrap();
            board.mark_paid(task.id).unwrap();
        }

        let board_order = board.leaderboard(10);
        assert_eq!(board_order[0].0, high);
        assert_eq!(board_order[1].0, low);
    }
}
