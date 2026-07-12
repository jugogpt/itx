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
    #[error("you have already joined this task")]
    AlreadyJoined,
    #[error("the window to join this task has passed")]
    JoinWindowExpired,
    #[error("the window to submit an answer for this task has passed")]
    SubmissionWindowExpired,
    #[error("this operation doesn't apply to this task's verification kind")]
    WrongTaskKind,
    #[error("you have already submitted an answer for this task")]
    AlreadySubmitted,
    #[error("this task requires at least {required} completed tasks, you have {have}")]
    InsufficientReputation { required: u64, have: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Open,
    Claimed,
    /// A winner (or, for a `Consensus` task, at least one) has been determined; payout to them is in flight but not yet confirmed submitted to the chain.
    Verified,
    Paid,
    /// `Consensus`-only terminal status covering two distinct causes --
    /// no one is owed a payout and no reputation was docked either way.
    /// See `Task::close_reason` for which cause it was.
    Closed,
}

/// Why a `Consensus` task ended in `TaskStatus::Closed`. Exposed on the
/// wire (see `handlers::TaskDto`) so a client can tell "an honest vote
/// split" apart from "never found enough participants" without having to
/// infer it themselves by comparing `assignees_joined` to `num_assignees`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Every answer was tied for the majority (or every assignee
    /// disagreed with every other) -- an honest split, nobody dinged.
    NoMajority,
    /// Never reached `num_assignees` joiners before its join deadline.
    Understaffed,
}

/// One assignee's participation in a `Consensus` task: their answer (once
/// submitted), their fixed bounty share (set once at resolution, present
/// only for winners), and whether that share has been paid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsensusAssignment {
    pub answer: Option<String>,
    pub paid: bool,
    /// This assignee's fixed share of the bounty, assigned once by
    /// `resolve_consensus` for winners only (`None` for losers and for
    /// anyone before resolution). Fixed at resolution time rather than
    /// recomputed from the shrinking pool of still-unpaid winners, so a
    /// partial payout failure and retry never inflates whoever is left
    /// unpaid's share.
    pub share: Option<u64>,
}


/// `Consensus` is for open-ended tasks with no single checkable answer:
/// `num_assignees` independent agents are each assigned the same task and
/// submit without seeing anyone else's answer; whichever answer the
/// majority converges on is treated as correct. There is no on-chain
/// currency stake here -- reputation is the stake. Agreeing with the
/// majority earns the usual payout and reputation credit; disagreeing
/// (or never submitting before the deadline) costs a reputation hit, the
/// same as a wrong `HashMatch` answer, without needing a separate escrow
/// mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskKind {
    HashMatch {
        expected_output_hash: Hash,
    },
    Consensus {
        num_assignees: u32,
        /// How long the task may sit `Open` waiting for `num_assignees`
        /// joiners before it's cancelled (freeing its escrow) if it never
        /// fills -- separate from `submission_deadline`, which only
        /// starts to matter once the task is already full.
        join_deadline: DateTime<Utc>,
        /// Minutes assignees get to submit once the task actually fills
        /// up. Stored as a window rather than a fixed point in time,
        /// because the fill moment (Open -> Claimed) isn't known at
        /// creation -- `submission_deadline` is computed from this the
        /// instant that happens (see `join_consensus_task`), not from
        /// creation time. Anchoring it at creation instead was a real bug
        /// this project shipped once already: a join phase that takes a
        /// while (exactly what `join_deadline` exists to allow) could
        /// leave a task's submission window already expired the moment
        /// it fills, force-resolving it before assignees who joined in
        /// good faith ever got a chance to answer.
        submission_window_minutes: i64,
        /// `None` for the entire time the task is `Open`; set once,
        /// atomically with the Open -> Claimed transition.
        submission_deadline: Option<DateTime<Utc>>,
        assignees: BTreeMap<PublicKey, ConsensusAssignment>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    pub kind: TaskKind,
    pub poster: PublicKey,
    pub status: TaskStatus,
    /// `HashMatch` only -- `Consensus` tasks track their assignees inside
    /// `TaskKind::Consensus` instead, since there can be more than one.
    pub claimant: Option<PublicKey>,
    /// `HashMatch` only, same reason.
    pub claim_deadline: Option<DateTime<Utc>>,
    pub failed_attempts: u32,
    pub created_at: DateTime<Utc>,
    /// Minimum `Reputation::completed` count required to claim/join this
    /// task. `0` (the default for every task `create_task`/
    /// `create_consensus_task` produce) means ungated -- set via
    /// `set_min_reputation` right after creation to require a track
    /// record before agents may attempt higher-value or higher-trust work.
    pub min_reputation: u64,
    /// `Consensus`-only: why the task ended `Closed`, if it did. See
    /// `CloseReason`.
    pub close_reason: Option<CloseReason>,
}

impl Task {
    /// Every (recipient, amount) pair still owed for this task right now.
    /// Empty unless the task is `Verified`: for `HashMatch` that's always
    /// exactly the one claimant; for `Consensus` it's every not-yet-paid
    /// assignee with a `share` (only winners get one, fixed once by
    /// `resolve_consensus` -- see `ConsensusAssignment::share`'s own docs
    /// for why this must be a stored, not recomputed, value). The caller
    /// retries whatever this returns until it comes back empty.
    pub fn pending_payouts(&self) -> Vec<(PublicKey, u64)> {
        if self.status != TaskStatus::Verified {
            return vec![];
        }
        match &self.kind {
            TaskKind::HashMatch { .. } => match &self.claimant {
                Some(claimant) => vec![(claimant.clone(), self.bounty)],
                None => vec![],
            },
            TaskKind::Consensus { assignees, .. } => assignees
                .iter()
                .filter(|(_, a)| !a.paid)
                .filter_map(|(pk, a)| a.share.map(|share| (pk.clone(), share)))
                .collect(),
        }
    }

    /// Whether `recipient` has already been paid their share of this
    /// task. Meant for a caller about to actually send money to
    /// re-confirm against live state immediately beforehand: the
    /// (recipient, amount) pair it's holding may come from an earlier,
    /// now-stale `pending_payouts()` snapshot, and another concurrent
    /// settlement attempt (the sweep vs. an immediate post-submit call,
    /// or two overlapping sweeps) may have already paid this exact
    /// recipient in the meantime.
    pub fn is_recipient_paid(&self, recipient: &PublicKey) -> bool {
        match &self.kind {
            TaskKind::HashMatch { .. } => {
                self.status == TaskStatus::Paid && self.claimant.as_ref() == Some(recipient)
            }
            TaskKind::Consensus { assignees, .. } => {
                assignees.get(recipient).is_some_and(|a| a.paid)
            }
        }
    }

    /// Every assignee of a `Consensus` task (empty for `HashMatch`).
    /// `resolve_consensus` can change several assignees' reputation at
    /// once (every loser, not just whoever's request happened to trigger
    /// resolution) -- callers persisting reputation after a resolution
    /// should save every pubkey this returns, not just the one they
    /// already had in hand.
    pub fn consensus_assignees(&self) -> Vec<PublicKey> {
        match &self.kind {
            TaskKind::HashMatch { .. } => vec![],
            TaskKind::Consensus { assignees, .. } => assignees.keys().cloned().collect(),
        }
    }
}

/// The answer with strictly the most submissions, or `None` if there are
/// no answers yet or the top spot is tied between two or more answers --
/// a tie is deliberately treated as "no consensus" rather than picking
/// arbitrarily among them.
fn majority_answer(assignees: &BTreeMap<PublicKey, ConsensusAssignment>) -> Option<String> {
    let mut counts: BTreeMap<&String, usize> = BTreeMap::new();
    for assignment in assignees.values() {
        if let Some(answer) = &assignment.answer {
            *counts.entry(answer).or_insert(0) += 1;
        }
    }
    let max_count = *counts.values().max()?;
    let mut top: Vec<&&String> = counts.iter().filter(|(_, &c)| c == max_count).map(|(k, _)| k).collect();
    match top.pop() {
        Some(answer) if top.is_empty() => Some((*answer).clone()),
        _ => None, // either no answers at all, or a tie for first place
    }
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
            .filter(|t| !matches!(t.status, TaskStatus::Paid | TaskStatus::Closed))
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
            kind: TaskKind::HashMatch { expected_output_hash },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Creates an open-ended task verified by majority agreement across
    /// `num_assignees` independent agents instead of a single checkable
    /// hash -- see `TaskKind::Consensus`. Callers (the HTTP layer) should
    /// validate `num_assignees >= 2` before calling this; a value of 0 or
    /// 1 is accepted here without complaint but makes "majority" a
    /// degenerate, always-trivially-true concept. `submission_window_minutes`
    /// is a duration, not a deadline -- the actual `submission_deadline` is
    /// computed once the task fills up (see `join_consensus_task`), not here.
    pub fn create_consensus_task(
        &mut self,
        poster: PublicKey,
        description: String,
        bounty: u64,
        num_assignees: u32,
        join_deadline: DateTime<Utc>,
        submission_window_minutes: i64,
    ) -> Task {
        let task = Task {
            id: Uuid::new_v4(),
            description,
            bounty,
            kind: TaskKind::Consensus {
                num_assignees,
                join_deadline,
                submission_window_minutes,
                submission_deadline: None,
                assignees: BTreeMap::new(),
            },
            poster,
            status: TaskStatus::Open,
            claimant: None,
            claim_deadline: None,
            failed_attempts: 0,
            created_at: Utc::now(),
            min_reputation: 0,
            close_reason: None,
        };
        self.tasks.insert(task.id, task.clone());
        task
    }

    /// Sets a minimum `Reputation::completed` count required to claim or
    /// join `id` going forward. Meant to be called right after creation
    /// (while the task is still `Open`, so no one has claimed it under
    /// the old, looser bar), but not restricted to that -- tightening or
    /// loosening the bar on an already-`Claimed` task simply has no
    /// effect on whoever already claimed it.
    pub fn set_min_reputation(&mut self, id: Uuid, min_reputation: u64) -> Result<(), BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        task.min_reputation = min_reputation;
        Ok(())
    }

    /// Checks `agent`'s `completed` count against `task`'s
    /// `min_reputation` bar. Shared by `claim_task` and
    /// `join_consensus_task` so both enforce the same rule the same way.
    fn check_min_reputation(&self, task: &Task, agent: &PublicKey) -> Result<(), BoardError> {
        let have = self.reputation(agent).completed;
        if have < task.min_reputation {
            return Err(BoardError::InsufficientReputation { required: task.min_reputation, have });
        }
        Ok(())
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

    /// Tasks that passed verification but whose payout hasn't been
    /// confirmed sent yet -- either because a payout attempt is still in
    /// flight, or a previous one failed. Polled periodically by the hub's
    /// sweep loop to retry payouts without needing a human to notice and
    /// resubmit them by hand.
    pub fn verified_unpaid_tasks(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| t.status == TaskStatus::Verified)
            .collect()
    }

    /// `HashMatch` only -- a `Consensus` task takes on multiple
    /// simultaneous assignees, so it uses `join_consensus_task` instead.
    pub fn claim_task(
        &mut self,
        id: Uuid,
        claimant: PublicKey,
        deadline: DateTime<Utc>,
    ) -> Result<(), BoardError> {
        let task = self.tasks.get(&id).ok_or(BoardError::NotFound)?;
        if !matches!(task.kind, TaskKind::HashMatch { .. }) {
            return Err(BoardError::WrongTaskKind);
        }
        if task.status != TaskStatus::Open {
            return Err(BoardError::NotOpen);
        }
        self.check_min_reputation(task, &claimant)?;

        let task = self.tasks.get_mut(&id).expect("existence already checked above");
        task.status = TaskStatus::Claimed;
        task.claimant = Some(claimant);
        task.claim_deadline = Some(deadline);
        Ok(())
    }

    /// Joins `agent` to an open `Consensus` task as one of its
    /// independent assignees. Once `num_assignees` have joined, the task
    /// closes to new joiners, moves to `Claimed`, and its
    /// `submission_deadline` is set from this exact moment (not from
    /// creation -- see `TaskKind::Consensus::submission_window_minutes`).
    pub fn join_consensus_task(&mut self, id: Uuid, agent: PublicKey) -> Result<(), BoardError> {
        let task = self.tasks.get(&id).ok_or(BoardError::NotFound)?;
        let TaskKind::Consensus { join_deadline, .. } = &task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        // Status checked before the deadline (not after): once a task is
        // no longer Open, its join_deadline is stale/irrelevant, and
        // NotOpen is the more accurate error than JoinWindowExpired for
        // e.g. a late retry against a task that filled up minutes ago.
        if task.status != TaskStatus::Open {
            return Err(BoardError::NotOpen);
        }
        // Defensive, not just an optimization: without this, a join
        // landing in the gap between the deadline passing and the next
        // sweep's `cancel_understaffed_consensus_tasks` call could bring
        // a task that should already be dead back to `Claimed`.
        if Utc::now() > *join_deadline {
            return Err(BoardError::JoinWindowExpired);
        }
        self.check_min_reputation(task, &agent)?;

        let task = self.tasks.get_mut(&id).expect("existence and kind already checked above");
        let TaskKind::Consensus {
            num_assignees,
            submission_window_minutes,
            submission_deadline,
            assignees,
            ..
        } = &mut task.kind
        else {
            unreachable!("kind already checked above");
        };
        if assignees.contains_key(&agent) {
            return Err(BoardError::AlreadyJoined);
        }
        assignees.insert(agent, ConsensusAssignment::default());
        let just_filled = assignees.len() as u32 >= *num_assignees;
        if just_filled {
            *submission_deadline = Some(Utc::now() + chrono::Duration::minutes(*submission_window_minutes));
        }
        if just_filled {
            task.status = TaskStatus::Claimed;
        }
        Ok(())
    }

    /// `HashMatch` only. Checks `output_hash` against the task's
    /// verification spec. Returns whether it matched. On a mismatch, the
    /// task reopens for another attempt (by anyone, including the same
    /// agent) and the submitter takes a reputation hit; on a match, the
    /// task moves to `Verified` (the caller is expected to then actually
    /// pay out via `pending_payouts`/`mark_recipient_paid`).
    pub fn submit(
        &mut self,
        id: Uuid,
        submitter: PublicKey,
        output_hash: Hash,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        let TaskKind::HashMatch { expected_output_hash } = &task.kind else {
            return Err(BoardError::WrongTaskKind);
        };
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        if task.claimant.as_ref() != Some(&submitter) {
            return Err(BoardError::NotClaimant);
        }

        if output_hash == *expected_output_hash {
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

    /// `Consensus` only. Records `agent`'s answer. Once every assignee
    /// has submitted, immediately resolves the task (see
    /// `resolve_consensus`) and returns `true`; otherwise returns `false`
    /// to indicate the task is still waiting on other assignees.
    pub fn submit_consensus_answer(
        &mut self,
        id: Uuid,
        agent: PublicKey,
        answer: String,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if !matches!(task.kind, TaskKind::Consensus { .. }) {
            return Err(BoardError::WrongTaskKind);
        }
        if task.status != TaskStatus::Claimed {
            return Err(BoardError::NotClaimed);
        }
        let TaskKind::Consensus { submission_deadline, assignees, .. } = &mut task.kind else {
            unreachable!("kind already checked above");
        };
        // Defensive, mirroring `join_consensus_task`'s check against
        // `join_deadline`: without this, a submission landing in the gap
        // between the deadline passing and the next sweep's
        // `resolve_expired_consensus_tasks` call would be silently
        // accepted as a real vote instead of being treated as a no-show.
        let deadline = submission_deadline
            .expect("a Claimed consensus task always has a submission_deadline, set atomically when it filled");
        if Utc::now() > deadline {
            return Err(BoardError::SubmissionWindowExpired);
        }
        let assignment = assignees.get_mut(&agent).ok_or(BoardError::NotClaimant)?;
        if assignment.answer.is_some() {
            return Err(BoardError::AlreadySubmitted);
        }
        assignment.answer = Some(answer);

        if assignees.values().all(|a| a.answer.is_some()) {
            self.resolve_consensus(id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Forces resolution of any `Consensus` task still `Claimed` whose
    /// submission deadline has passed, treating any assignee who never
    /// submitted as having disagreed with the majority (same as an
    /// ordinary wrong answer) rather than leaving the task stuck waiting
    /// on a no-show forever. Returns the ids resolved this way. Call
    /// periodically from a background sweep, same as `expire_claims`.
    pub fn resolve_expired_consensus_tasks(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let due: Vec<Uuid> = self
            .tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Claimed
                    && matches!(&t.kind, TaskKind::Consensus { submission_deadline, .. } if submission_deadline.is_some_and(|d| now > d))
            })
            .map(|t| t.id)
            .collect();
        for id in &due {
            self.resolve_consensus(*id);
        }
        due
    }

    /// Cancels any `Consensus` task still `Open` (never reached
    /// `num_assignees` joiners) past its join deadline, transitioning it
    /// straight to `Closed` -- the same terminal, no-payout-no-dings
    /// status a tied vote reaches, since an under-subscribed task isn't
    /// anyone's fault either. Without this, a task nobody finishes
    /// joining would sit `Open` forever, permanently tying up its bounty
    /// in the operator's `allocated_bounty()`. Returns the ids cancelled
    /// this way. Call periodically from a background sweep, same as
    /// `resolve_expired_consensus_tasks`.
    ///
    /// Single-pass, unlike `resolve_expired_consensus_tasks`: that one
    /// needs a separate collect-then-act split because `resolve_consensus`
    /// takes `&mut self` (it also touches `self.reputation`), which can't
    /// be called from inside a `self.tasks.values_mut()` loop -- this
    /// function only ever touches the `Task` already in hand, so no such
    /// split is needed here.
    pub fn cancel_understaffed_consensus_tasks(&mut self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut cancelled = Vec::new();
        for task in self.tasks.values_mut() {
            if task.status == TaskStatus::Open
                && matches!(&task.kind, TaskKind::Consensus { join_deadline, .. } if now > *join_deadline)
            {
                task.status = TaskStatus::Closed;
                task.close_reason = Some(CloseReason::Understaffed);
                cancelled.push(task.id);
            }
        }
        cancelled
    }

    /// Computes the majority answer across a `Consensus` task's
    /// assignees (whoever has submitted so far -- callers only invoke
    /// this once every assignee has answered, or the deadline forced an
    /// early call), dings the reputation of everyone who didn't match it
    /// (a non-submission never matches), and transitions the task to
    /// `Verified` if there's a majority to pay out, or `Closed` if the
    /// vote was a tie (nobody paid, nobody dinged for an honest split).
    fn resolve_consensus(&mut self, id: Uuid) {
        let Some(task) = self.tasks.get(&id) else { return };
        let TaskKind::Consensus { assignees, .. } = &task.kind else {
            return;
        };
        let winning_answer = majority_answer(assignees);
        // Computed once, right now, from the full winner count -- and
        // never recomputed later. `pending_payouts`/`mark_recipient_paid`
        // only ever read this stored value back, so a payout that fails
        // partway through and gets retried can't inflate whoever is left
        // unpaid's share by dividing the bounty over a shrinking pool.
        let share = winning_answer.as_ref().map(|answer| {
            let winner_count = assignees
                .values()
                .filter(|a| a.answer.as_ref() == Some(answer))
                .count() as u64;
            task.bounty / winner_count.max(1)
        });

        if let Some(winning_answer) = &winning_answer {
            let losers: Vec<PublicKey> = assignees
                .iter()
                .filter(|(_, a)| a.answer.as_ref() != Some(winning_answer))
                .map(|(pk, _)| pk.clone())
                .collect();
            for loser in losers {
                self.reputation.entry(loser).or_default().failed += 1;
            }
        }

        let task = self.tasks.get_mut(&id).expect("checked above");
        if let (Some(winning_answer), Some(share), TaskKind::Consensus { assignees, .. }) =
            (&winning_answer, share, &mut task.kind)
        {
            for assignment in assignees.values_mut() {
                if assignment.answer.as_ref() == Some(winning_answer) {
                    assignment.share = Some(share);
                }
            }
        }
        if winning_answer.is_some() {
            task.status = TaskStatus::Verified;
        } else {
            task.status = TaskStatus::Closed;
            task.close_reason = Some(CloseReason::NoMajority);
        }
    }

    /// Records that `recipient`'s `amount`-sized share of a `Verified`
    /// task's bounty was successfully paid out on-chain, crediting their
    /// reputation. Split from resolution so a transient payout failure
    /// never silently credits reputation for a payment that didn't
    /// actually happen -- the caller only calls this once the payment is
    /// confirmed sent. Works for both task kinds: a `HashMatch` task has
    /// exactly one possible recipient (its claimant) and always
    /// completes the task in one call; a `Consensus` task may have
    /// several winners, and the task only reaches `Paid` once every
    /// winner named by `pending_payouts` has been recorded here. Returns
    /// whether the whole task is now fully paid.
    pub fn mark_recipient_paid(
        &mut self,
        id: Uuid,
        recipient: &PublicKey,
        amount: u64,
    ) -> Result<bool, BoardError> {
        let task = self.tasks.get_mut(&id).ok_or(BoardError::NotFound)?;
        if task.status != TaskStatus::Verified {
            return Err(BoardError::NotVerified);
        }

        let now_fully_paid = match &mut task.kind {
            TaskKind::HashMatch { .. } => {
                if task.claimant.as_ref() != Some(recipient) {
                    return Err(BoardError::NotClaimant);
                }
                true
            }
            TaskKind::Consensus { assignees, .. } => {
                let assignment = assignees.get_mut(recipient).ok_or(BoardError::NotClaimant)?;
                if assignment.share.is_none() {
                    return Err(BoardError::NotClaimant);
                }
                if assignment.paid {
                    return Ok(false);
                }
                assignment.paid = true;
                // Every winner's `share` was fixed by `resolve_consensus`;
                // "fully paid" just means every one of them is now marked
                // paid too -- no need to recompute the majority again.
                assignees.values().all(|a| a.share.is_none() || a.paid)
            }
        };

        if now_fully_paid {
            task.status = TaskStatus::Paid;
        }
        let rep = self.reputation.entry(recipient.clone()).or_default();
        rep.completed += 1;
        rep.total_earned += amount;
        Ok(now_fully_paid)
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

        // not paid/credited until mark_recipient_paid is called
        assert_eq!(board.reputation(&worker).completed, 0);
        assert!(board.mark_recipient_paid(task.id, &worker, 100).unwrap());
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
    fn verified_unpaid_tasks_lists_only_verified_tasks() {
        let mut board = TaskBoard::new();
        let expected = Hash::hash_bytes(b"x");
        let worker = pubkey();

        // Open: not listed
        board.create_task(pubkey(), "open".to_string(), 10, expected);

        // Verified: listed
        let verified_task = board.create_task(pubkey(), "verified".to_string(), 10, expected);
        board
            .claim_task(verified_task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(verified_task.id, worker.clone(), expected).unwrap();

        // Paid: no longer listed
        let paid_task = board.create_task(pubkey(), "paid".to_string(), 10, expected);
        board
            .claim_task(paid_task.id, worker.clone(), Utc::now() + chrono::Duration::minutes(5))
            .unwrap();
        board.submit(paid_task.id, worker.clone(), expected).unwrap();
        board.mark_recipient_paid(paid_task.id, &worker, 10).unwrap();

        let unpaid = board.verified_unpaid_tasks();
        assert_eq!(unpaid.len(), 1);
        assert_eq!(unpaid[0].id, verified_task.id);
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
            board.mark_recipient_paid(task.id, agent, bounty).unwrap();
        }

        let board_order = board.leaderboard(10);
        assert_eq!(board_order[0].0, high);
        assert_eq!(board_order[1].0, low);
    }

    /// `deadline` is the desired *submission* deadline, converted to a
    /// `submission_window_minutes` internally (callers still pass a
    /// `DateTime` the way every existing test already computes one, e.g.
    /// `Utc::now() + Duration::minutes(30)` -- only this helper needed to
    /// know about the window-vs-deadline distinction). Every assignee
    /// joins synchronously right here, well within any reasonable join
    /// window, so the join deadline itself is just a generous, fixed hour
    /// out and not parameterized (tests that specifically care about the
    /// join window construct their own task directly instead of using
    /// this helper).
    fn create_and_fill_consensus_task(
        board: &mut TaskBoard,
        num_assignees: u32,
        deadline: DateTime<Utc>,
    ) -> (Uuid, Vec<PublicKey>) {
        let join_deadline = Utc::now() + chrono::Duration::hours(1);
        let submission_window_minutes = (deadline - Utc::now()).num_minutes().max(1);
        let task = board.create_consensus_task(
            pubkey(),
            "open-ended".to_string(),
            900,
            num_assignees,
            join_deadline,
            submission_window_minutes,
        );
        let assignees: Vec<PublicKey> = (0..num_assignees).map(|_| pubkey()).collect();
        for agent in &assignees {
            board.join_consensus_task(task.id, agent.clone()).unwrap();
        }
        (task.id, assignees)
    }

    #[test]
    fn consensus_task_resolves_and_pays_the_majority_once_everyone_submits() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);

        // two agree, one doesn't
        assert!(!board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap());
        assert!(!board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap());
        assert!(board.submit_consensus_answer(task_id, assignees[2].clone(), "wrong".to_string()).unwrap());

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Verified);

        let payouts = task.pending_payouts();
        assert_eq!(payouts.len(), 2, "the two agreeing assignees each owe a share");
        for (pk, amount) in &payouts {
            assert!(assignees[0..2].contains(pk));
            assert_eq!(*amount, 900 / 2);
        }

        // the loser was dinged immediately at resolution, without waiting on payout
        assert_eq!(board.reputation(&assignees[2]).failed, 1);
        assert_eq!(board.reputation(&assignees[0]).completed, 0, "not credited until actually paid");
    }

    #[test]
    fn consensus_task_closes_with_no_payout_on_a_tie() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        board.submit_consensus_answer(task_id, assignees[0].clone(), "a".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "b".to_string()).unwrap();

        let task = board.get_task(task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert!(task.pending_payouts().is_empty());
        assert_eq!(board.reputation(&assignees[0]).failed, 0, "an honest tie dings no one");
        assert_eq!(board.reputation(&assignees[1]).failed, 0);
        assert_eq!(board.allocated_bounty(), 0, "a closed task's bounty is no longer allocated");
    }

    #[test]
    fn join_consensus_task_closes_to_new_joiners_once_full() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, _assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        assert!(matches!(
            board.join_consensus_task(task_id, pubkey()),
            Err(BoardError::NotOpen)
        ));
    }

    #[test]
    fn cannot_join_a_consensus_task_twice() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let task = board.create_consensus_task(pubkey(), "t".to_string(), 10, 3, deadline, 30);
        let agent = pubkey();
        board.join_consensus_task(task.id, agent.clone()).unwrap();

        assert!(matches!(
            board.join_consensus_task(task.id, agent),
            Err(BoardError::AlreadyJoined)
        ));
    }

    #[test]
    fn resolve_expired_consensus_tasks_treats_no_shows_as_disagreeing() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let deadline = now + chrono::Duration::minutes(5);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);

        // only two of three ever submit, and they agree
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap();

        // not expired yet -- still waiting on assignees[2]
        assert!(board.resolve_expired_consensus_tasks(now).is_empty());
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);

        // now it is
        let resolved = board.resolve_expired_consensus_tasks(deadline + chrono::Duration::seconds(1));
        assert_eq!(resolved, vec![task_id]);
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);
        assert_eq!(board.reputation(&assignees[2]).failed, 1, "no-show counts as disagreeing");
        assert_eq!(board.get_task(task_id).unwrap().pending_payouts().len(), 2);
    }

    #[test]
    fn mark_recipient_paid_completes_a_consensus_task_only_once_every_winner_is_paid() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();
        board.submit_consensus_answer(task_id, assignees[1].clone(), "42".to_string()).unwrap();
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[0], 450).unwrap();
        assert!(!fully_paid, "one of two winners paid -- task not fully settled yet");
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Verified);
        assert_eq!(board.get_task(task_id).unwrap().pending_payouts().len(), 1);

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[1], 450).unwrap();
        assert!(fully_paid, "both winners paid -- task fully settled");
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Paid);
        assert_eq!(board.reputation(&assignees[0]).total_earned, 450);
        assert_eq!(board.reputation(&assignees[1]).total_earned, 450);
    }

    #[test]
    fn hash_match_operations_reject_a_consensus_task_and_vice_versa() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let consensus_task = board.create_consensus_task(pubkey(), "c".to_string(), 10, 2, deadline, 30);
        let hash_task = board.create_task(pubkey(), "h".to_string(), 10, Hash::hash_bytes(b"x"));

        assert!(matches!(
            board.claim_task(consensus_task.id, pubkey(), deadline),
            Err(BoardError::WrongTaskKind)
        ));
        assert!(matches!(
            board.join_consensus_task(hash_task.id, pubkey()),
            Err(BoardError::WrongTaskKind)
        ));

        board.claim_task(hash_task.id, pubkey(), deadline).unwrap();
        assert!(matches!(
            board.submit_consensus_answer(hash_task.id, pubkey(), "x".to_string()),
            Err(BoardError::WrongTaskKind)
        ));

        board.join_consensus_task(consensus_task.id, pubkey()).unwrap();
        board.join_consensus_task(consensus_task.id, pubkey()).unwrap();
        assert!(matches!(
            board.submit(consensus_task.id, pubkey(), Hash::hash_bytes(b"x")),
            Err(BoardError::WrongTaskKind)
        ));
    }

    #[test]
    fn cannot_submit_a_consensus_answer_twice() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        board.submit_consensus_answer(task_id, assignees[0].clone(), "42".to_string()).unwrap();

        assert!(matches!(
            board.submit_consensus_answer(task_id, assignees[0].clone(), "43".to_string()),
            Err(BoardError::AlreadySubmitted)
        ));
    }

    #[test]
    fn claim_task_enforces_min_reputation() {
        let mut board = TaskBoard::new();
        let task = board.create_task(pubkey(), "gated".to_string(), 10, Hash::hash_bytes(b"x"));
        board.set_min_reputation(task.id, 5).unwrap();
        let novice = pubkey();
        let deadline = Utc::now() + chrono::Duration::minutes(5);

        assert!(matches!(
            board.claim_task(task.id, novice.clone(), deadline),
            Err(BoardError::InsufficientReputation { required: 5, have: 0 })
        ));
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open, "a rejected claim must not consume the task");

        let veteran = pubkey();
        board.restore_reputation(veteran.clone(), Reputation { completed: 5, failed: 0, total_earned: 0 });
        assert!(board.claim_task(task.id, veteran, deadline).is_ok());
    }

    #[test]
    fn join_consensus_task_enforces_min_reputation() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let task = board.create_consensus_task(pubkey(), "gated".to_string(), 10, 2, deadline, 30);
        board.set_min_reputation(task.id, 3).unwrap();

        let novice = pubkey();
        assert!(matches!(
            board.join_consensus_task(task.id, novice),
            Err(BoardError::InsufficientReputation { required: 3, have: 0 })
        ));

        let veteran = pubkey();
        board.restore_reputation(veteran.clone(), Reputation { completed: 3, failed: 0, total_earned: 0 });
        assert!(board.join_consensus_task(task.id, veteran).is_ok());
    }

    #[test]
    fn set_min_reputation_on_a_missing_task_fails() {
        let mut board = TaskBoard::new();
        assert!(matches!(
            board.set_min_reputation(Uuid::new_v4(), 5),
            Err(BoardError::NotFound)
        ));
    }

    #[test]
    fn consensus_share_stays_fixed_across_a_partial_payout_retry() {
        // Regression test: `pending_payouts` used to divide the bounty by
        // the count of still-*unpaid* winners, so paying one of several
        // winners inflated everyone else's share on the next call. The
        // share must instead be fixed once, at resolution, and never
        // recomputed from a shrinking pool.
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        for agent in &assignees {
            board.submit_consensus_answer(task_id, agent.clone(), "42".to_string()).unwrap();
        }

        let payouts = board.get_task(task_id).unwrap().pending_payouts();
        assert_eq!(payouts.len(), 3);
        for (_, amount) in &payouts {
            assert_eq!(*amount, 300, "900 bounty split 3 ways");
        }

        let fully_paid = board.mark_recipient_paid(task_id, &assignees[0], 300).unwrap();
        assert!(!fully_paid);

        let remaining = board.get_task(task_id).unwrap().pending_payouts();
        assert_eq!(remaining.len(), 2);
        for (_, amount) in &remaining {
            assert_eq!(
                *amount, 300,
                "share must stay fixed at 300 -- must NOT inflate to 900/2=450 just because one winner is already paid"
            );
        }
    }

    #[test]
    fn is_recipient_paid_reflects_live_state_for_both_task_kinds() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);

        // HashMatch
        let hash_task = board.create_task(pubkey(), "h".to_string(), 100, Hash::hash_bytes(b"x"));
        let worker = pubkey();
        board.claim_task(hash_task.id, worker.clone(), deadline).unwrap();
        board.submit(hash_task.id, worker.clone(), Hash::hash_bytes(b"x")).unwrap();
        assert!(!board.get_task(hash_task.id).unwrap().is_recipient_paid(&worker));
        board.mark_recipient_paid(hash_task.id, &worker, 100).unwrap();
        assert!(board.get_task(hash_task.id).unwrap().is_recipient_paid(&worker));

        // Consensus
        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);
        for agent in &assignees {
            board.submit_consensus_answer(task_id, agent.clone(), "42".to_string()).unwrap();
        }
        assert!(!board.get_task(task_id).unwrap().is_recipient_paid(&assignees[0]));
        board.mark_recipient_paid(task_id, &assignees[0], 450).unwrap();
        assert!(board.get_task(task_id).unwrap().is_recipient_paid(&assignees[0]));
        assert!(!board.get_task(task_id).unwrap().is_recipient_paid(&assignees[1]), "the other winner is still unpaid");
    }

    #[test]
    fn consensus_assignees_lists_everyone_for_consensus_and_nobody_for_hash_match() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(30);
        let hash_task = board.create_task(pubkey(), "h".to_string(), 10, Hash::hash_bytes(b"x"));
        assert!(board.get_task(hash_task.id).unwrap().consensus_assignees().is_empty());

        let (task_id, assignees) = create_and_fill_consensus_task(&mut board, 3, deadline);
        let mut listed = board.get_task(task_id).unwrap().consensus_assignees();
        listed.sort();
        let mut expected = assignees.clone();
        expected.sort();
        assert_eq!(listed, expected);
    }

    #[test]
    fn cancel_understaffed_consensus_tasks_closes_a_task_that_never_filled_up() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let join_deadline = now + chrono::Duration::minutes(5);
        let task = board.create_consensus_task(
            pubkey(),
            "needs 3, only 1 shows up".to_string(),
            900,
            3,
            join_deadline,
            60,
        );
        board.join_consensus_task(task.id, pubkey()).unwrap();
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Open);
        assert_eq!(board.allocated_bounty(), 900);

        // not expired yet
        assert!(board.cancel_understaffed_consensus_tasks(now).is_empty());

        // now it is
        let cancelled = board.cancel_understaffed_consensus_tasks(join_deadline + chrono::Duration::seconds(1));
        assert_eq!(cancelled, vec![task.id]);
        assert_eq!(board.get_task(task.id).unwrap().status, TaskStatus::Closed);
        assert_eq!(board.allocated_bounty(), 0, "a cancelled task's bounty is freed back up");
    }

    #[test]
    fn cancel_understaffed_consensus_tasks_leaves_a_fully_joined_task_alone() {
        let mut board = TaskBoard::new();
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        let (task_id, _assignees) = create_and_fill_consensus_task(&mut board, 2, deadline);

        // even well past what would have been an unmet join deadline, a
        // task that already filled up (and is now Claimed) must never be
        // touched by the understaffed-task sweep
        let cancelled = board.cancel_understaffed_consensus_tasks(Utc::now() + chrono::Duration::hours(2));
        assert!(cancelled.is_empty());
        assert_eq!(board.get_task(task_id).unwrap().status, TaskStatus::Claimed);
    }

    #[test]
    fn join_consensus_task_rejects_a_join_past_its_deadline() {
        let mut board = TaskBoard::new();
        let now = Utc::now();
        let join_deadline = now - chrono::Duration::seconds(1);
        let task = board.create_consensus_task(
            pubkey(),
            "already expired".to_string(),
            10,
            2,
            join_deadline,
            60,
        );

        assert!(matches!(
            board.join_consensus_task(task.id, pubkey()),
            Err(BoardError::JoinWindowExpired)
        ));
    }
}
