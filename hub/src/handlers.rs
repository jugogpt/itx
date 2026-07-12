use tracing::*;

use crate::auth::{AuthError, SignedEnvelope};
use crate::board::{BoardError, CloseReason, Reputation, Task, TaskBoard, TaskKind, TaskStatus};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use btclib::crypto::PublicKey;
use btclib::sha256::Hash;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use static_init::dynamic;
use std::sync::Arc;
use uuid::Uuid;

/// How long a claim holds a task before it's automatically reopened for
/// someone else, if the claimant never submits.
const CLAIM_TTL_MINUTES: i64 = 30;
/// Flat fee attached to every hub-issued payment (faucet grants, task
/// payouts). Small and nonzero, matching how a real fee market works,
/// even though a private testnet has no real fee competition yet.
const HUB_TRANSACTION_FEE: u64 = 1_000;
/// Size of a faucet grant, in the same base units as block rewards
/// (INITIAL_REWARD is denominated in whole coins * 10^8).
const FAUCET_GRANT_AMOUNT: u64 = 50_000_000;
/// Upper bound on `join_window_minutes`/`submission_window_minutes`: not
/// just a sanity limit, but the difference between a clean 400 and an
/// actual panic -- `chrono::Duration::minutes` panics on overflow, and an
/// unbounded `i64` from a request body can get arbitrarily close to that.
/// A year is already far more generous than any real testnet task needs.
const MAX_CONSENSUS_WINDOW_MINUTES: i64 = 60 * 24 * 365;
/// Upper bound on `num_assignees`. Each resolution persists one redb
/// write transaction per assignee (see `persist_other_assignees_reputation`
/// and the sweep's equivalent), so this also caps how much synchronous
/// disk I/O one task's resolution can trigger.
const MAX_CONSENSUS_ASSIGNEES: u32 = 100;

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::ClockDrift | AuthError::Replayed | AuthError::BadSignature => {
                ApiError::Unauthorized(e.to_string())
            }
            AuthError::BadPublicKey(_) | AuthError::BadSignatureEncoding(_) => {
                ApiError::BadRequest(e.to_string())
            }
        }
    }
}

impl From<BoardError> for ApiError {
    fn from(e: BoardError) -> Self {
        match e {
            BoardError::NotFound => ApiError::NotFound(e.to_string()),
            BoardError::NotOpen
            | BoardError::NotClaimed
            | BoardError::NotVerified
            | BoardError::AlreadyClaimed
            | BoardError::AlreadyJoined
            | BoardError::AlreadySubmitted
            | BoardError::JoinWindowExpired
            | BoardError::SubmissionWindowExpired
            | BoardError::WrongTaskKind => ApiError::Conflict(e.to_string()),
            BoardError::NotClaimant | BoardError::InsufficientReputation { .. } => {
                ApiError::Forbidden(e.to_string())
            }
        }
    }
}

fn parse_hex_hash(hex_str: &str) -> Result<Hash, ApiError> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| ApiError::BadRequest(format!("expected_output_hash isn't valid hex: {e}")))?;
    let array: [u8; 32] = bytes.try_into().map_err(|_| {
        ApiError::BadRequest("expected_output_hash must be exactly 32 bytes (64 hex chars)".into())
    })?;
    Ok(Hash::from_bytes(array))
}

// ---------------------------------------------------------------------
// Response DTOs -- deliberately hide `expected_output_hash` from public
// task listings (no reason to make the verification target any more
// discoverable than it needs to be) and represent every key as a plain
// hex string, never btclib's internal CBOR shape.
// ---------------------------------------------------------------------

/// A `Consensus` task's public view deliberately omits every assignee's
/// individual answer, even after resolution -- a late joiner (or anyone
/// re-fetching the task before it's full) must never be able to see what
/// someone else already answered, or the whole point of independent
/// redundant assignment is defeated.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskKindDto {
    HashMatch,
    Consensus {
        num_assignees: u32,
        assignees_joined: u32,
        /// How long the task has left to attract `num_assignees` joiners
        /// before it's cancelled for being under-subscribed. Only
        /// meaningful while `status` is still `Open`.
        join_deadline: DateTime<Utc>,
        /// `None` until the task actually fills up (transitions to
        /// `Claimed`) -- its submission window doesn't start counting
        /// down before then.
        submission_deadline: Option<DateTime<Utc>>,
    },
}

#[derive(Serialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    pub status: TaskStatus,
    pub poster: String,
    pub claimant: Option<String>,
    pub failed_attempts: u32,
    /// Minimum completed-task count required to claim/join this task; `0`
    /// means anyone may attempt it.
    pub min_reputation: u64,
    /// `Consensus`-only: why the task ended `Closed`, if it did.
    pub close_reason: Option<CloseReason>,
    #[serde(flatten)]
    pub kind: TaskKindDto,
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        let kind = match &task.kind {
            TaskKind::HashMatch { .. } => TaskKindDto::HashMatch,
            TaskKind::Consensus { num_assignees, join_deadline, submission_deadline, assignees, .. } => TaskKindDto::Consensus {
                num_assignees: *num_assignees,
                assignees_joined: assignees.len() as u32,
                join_deadline: *join_deadline,
                submission_deadline: *submission_deadline,
            },
        };
        TaskDto {
            id: task.id,
            description: task.description.clone(),
            bounty: task.bounty,
            status: task.status,
            poster: task.poster.to_string(),
            claimant: task.claimant.as_ref().map(|k| k.to_string()),
            failed_attempts: task.failed_attempts,
            min_reputation: task.min_reputation,
            close_reason: task.close_reason,
            kind,
        }
    }
}

#[derive(Serialize)]
pub struct ReputationDto {
    pub completed: u64,
    pub failed: u64,
    pub total_earned: u64,
}

impl From<Reputation> for ReputationDto {
    fn from(r: Reputation) -> Self {
        ReputationDto {
            completed: r.completed,
            failed: r.failed,
            total_earned: r.total_earned,
        }
    }
}

#[derive(Serialize)]
pub struct LeaderboardEntryDto {
    pub pubkey: String,
    #[serde(flatten)]
    pub reputation: ReputationDto,
}

#[derive(Serialize)]
pub struct FaucetResultDto {
    pub amount: u64,
}

#[derive(Serialize)]
pub struct SubmitResultDto {
    /// `HashMatch`: whether the output matched. `Consensus`: whether
    /// *this* agent's answer matched the majority (only meaningful once
    /// `resolved` is `Some(true)`).
    pub verified: bool,
    pub paid: bool,
    pub bounty: Option<u64>,
    /// `None` for `HashMatch` (submission and resolution are always the
    /// same event there). For `Consensus`: `Some(false)` if this
    /// submission is still waiting on other assignees, `Some(true)` once
    /// every assignee has submitted (or the deadline forced it) and the
    /// task has resolved.
    pub resolved: Option<bool>,
}

// ---------------------------------------------------------------------
// Request payloads (the `T` in `SignedEnvelope<T>`)
// ---------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct CreateTaskPayload {
    pub description: String,
    pub bounty: u64,
    /// Hex-encoded SHA256 of the expected correct output. This is Phase
    /// B's one verification tier: objectively checkable compute/data
    /// jobs, not open-ended ones.
    pub expected_output_hash: String,
    /// Minimum completed-task count required to claim this task. Omit
    /// (or send `0`) for no gate at all.
    #[serde(default)]
    pub min_reputation: u64,
}

#[derive(Deserialize, Serialize)]
pub struct CreateConsensusTaskPayload {
    pub description: String,
    pub bounty: u64,
    /// How many independent agents must be assigned before the task
    /// closes to new joiners and awaits submissions. Must be at least 2 --
    /// with only one assignee, "majority" is a meaningless concept.
    pub num_assignees: u32,
    /// How long the task waits for `num_assignees` joiners before it's
    /// cancelled (refunding its escrow) for being under-subscribed.
    pub join_window_minutes: i64,
    /// How long, from the moment the task fills up, assignees have to
    /// submit their answer before a no-show counts against them.
    pub submission_window_minutes: i64,
    /// Same meaning as `CreateTaskPayload::min_reputation`.
    #[serde(default)]
    pub min_reputation: u64,
}

#[derive(Deserialize, Serialize)]
pub struct ClaimPayload {
    pub task_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct SubmitPayload {
    pub task_id: Uuid,
    pub output: String,
}

// ---------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------

pub async fn list_tasks(State(state): State<Arc<AppState>>) -> Json<Vec<TaskDto>> {
    let board = state.board.read().await;
    Json(board.list_open_tasks().into_iter().map(TaskDto::from).collect())
}

pub async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<TaskDto>, ApiError> {
    let board = state.board.read().await;
    let task = board
        .get_task(task_id)
        .ok_or_else(|| ApiError::NotFound("task not found".into()))?;
    Ok(Json(TaskDto::from(task)))
}

pub async fn create_task(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<SignedEnvelope<CreateTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;
    let expected_output_hash = parse_hex_hash(&envelope.payload.expected_output_hash)?;
    let bounty = envelope.payload.bounty;

    // Held across the balance check below on purpose: this is what makes
    // two concurrent task-creation requests safe (the second one's
    // balance check correctly sees the first one's allocation), at the
    // cost of serializing task creation against the node round-trip --
    // an acceptable tradeoff at this scale.
    let mut board = state.board.write().await;
    ensure_operator_can_fund(&state, &board, bounty).await?;
    let mut task = board.create_task(pubkey, envelope.payload.description.clone(), bounty, expected_output_hash);
    apply_min_reputation(&mut board, &mut task, envelope.payload.min_reputation);
    drop(board);

    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn create_consensus_task(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<SignedEnvelope<CreateConsensusTaskPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    let pubkey = envelope.verify()?;
    require_operator(&pubkey, &state)?;
    if envelope.payload.num_assignees < 2 {
        return Err(ApiError::BadRequest(
            "a consensus task needs at least 2 assignees for majority agreement to mean anything".into(),
        ));
    }
    if envelope.payload.num_assignees > MAX_CONSENSUS_ASSIGNEES {
        return Err(ApiError::BadRequest(format!(
            "num_assignees must be at most {MAX_CONSENSUS_ASSIGNEES}"
        )));
    }
    validate_positive_minutes(envelope.payload.join_window_minutes, "join_window_minutes")?;
    validate_positive_minutes(envelope.payload.submission_window_minutes, "submission_window_minutes")?;
    let bounty = envelope.payload.bounty;

    let mut board = state.board.write().await;
    ensure_operator_can_fund(&state, &board, bounty).await?;
    // join_deadline is computed only now, after the (possibly slow) node
    // balance round-trip above -- so a sluggish node can't silently eat
    // into the window this task advertises. submission_deadline is NOT
    // computed here at all: it's set once the task actually fills up
    // (join_consensus_task), not from creation time -- see
    // TaskKind::Consensus::submission_window_minutes for why that
    // distinction matters.
    let join_deadline = Utc::now() + Duration::minutes(envelope.payload.join_window_minutes);
    let mut task = board.create_consensus_task(
        pubkey,
        envelope.payload.description.clone(),
        bounty,
        envelope.payload.num_assignees,
        join_deadline,
        envelope.payload.submission_window_minutes,
    );
    apply_min_reputation(&mut board, &mut task, envelope.payload.min_reputation);
    drop(board);

    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn claim_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    Json(envelope): Json<SignedEnvelope<ClaimPayload>>,
) -> Result<Json<TaskDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

    let task = {
        let mut board = state.board.write().await;
        let is_consensus = matches!(
            board
                .get_task(task_id)
                .ok_or_else(|| ApiError::NotFound("task not found".into()))?
                .kind,
            TaskKind::Consensus { .. }
        );
        if is_consensus {
            board.join_consensus_task(task_id, pubkey)?;
        } else {
            let deadline = Utc::now() + Duration::minutes(CLAIM_TTL_MINUTES);
            board.claim_task(task_id, pubkey, deadline)?;
        }
        board.get_task(task_id).expect("just touched it").clone()
    };
    state.store.save_task(&task).map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(TaskDto::from(&task)))
}

pub async fn submit_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    Json(envelope): Json<SignedEnvelope<SubmitPayload>>,
) -> Result<Json<SubmitResultDto>, ApiError> {
    if envelope.payload.task_id != task_id {
        return Err(ApiError::BadRequest(
            "task id in the URL doesn't match the signed payload".into(),
        ));
    }
    let pubkey = envelope.verify()?;

    let is_consensus = {
        let board = state.board.read().await;
        let task = board
            .get_task(task_id)
            .ok_or_else(|| ApiError::NotFound("task not found".into()))?;
        matches!(task.kind, TaskKind::Consensus { .. })
    };

    if is_consensus {
        submit_consensus_task(&state, task_id, pubkey, envelope.payload.output.clone()).await
    } else {
        submit_hash_match_task(&state, task_id, pubkey, &envelope.payload.output).await
    }
}

async fn submit_hash_match_task(
    state: &AppState,
    task_id: Uuid,
    pubkey: PublicKey,
    output: &str,
) -> Result<Json<SubmitResultDto>, ApiError> {
    let output_hash = Hash::hash_bytes(output.as_bytes());

    let (verified, task_after_submit) = {
        let mut board = state.board.write().await;
        let verified = board.submit(task_id, pubkey.clone(), output_hash)?;
        (verified, board.get_task(task_id).expect("just touched it").clone())
    };
    persist_task_and_reputation(state, &task_after_submit, &pubkey).await?;

    if !verified {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: None,
        }));
    }

    // Left `Verified` either way -- if this fails, the task isn't lost:
    // the sweep loop in main.rs retries every Verified-but-unpaid task
    // periodically, so a transient payout failure here self-heals without
    // needing a human to notice and resubmit it by hand.
    let paid = try_settle_verified_task(state, task_id).await;

    Ok(Json(SubmitResultDto {
        verified: true,
        paid,
        bounty: Some(task_after_submit.bounty),
        resolved: None,
    }))
}

async fn submit_consensus_task(
    state: &AppState,
    task_id: Uuid,
    pubkey: PublicKey,
    output: String,
) -> Result<Json<SubmitResultDto>, ApiError> {
    let (resolved, task_after_submit) = {
        let mut board = state.board.write().await;
        let resolved = board.submit_consensus_answer(task_id, pubkey.clone(), output)?;
        (resolved, board.get_task(task_id).expect("just touched it").clone())
    };
    persist_task_and_reputation(state, &task_after_submit, &pubkey).await?;

    if !resolved {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: Some(false),
        }));
    }

    // Resolution can ding reputation for every assignee who disagreed,
    // not just this caller -- persist all of them, not only the one
    // `persist_task_and_reputation` above already covered.
    persist_other_assignees_reputation(state, &task_after_submit, &pubkey).await;

    // Resolution just happened -- pay out every winner it produced right
    // away rather than waiting for the next sweep. Deliberately
    // unconditional: the caller who happened to trigger resolution (by
    // being the last to submit) is not necessarily a winner themselves,
    // but winners still need settling regardless of who completed the set.
    try_settle_verified_task(state, task_id).await;

    // Did *this* agent's own answer match the majority? Absent from
    // `pending_payouts` (computed from the pre-settlement snapshot, so it
    // still names every winner) means either they disagreed, or the task
    // closed with no majority at all (see `TaskStatus::Closed`) -- either
    // way, nothing owed to them.
    let my_share = task_after_submit.pending_payouts().into_iter().find(|(pk, _)| *pk == pubkey);
    let Some((_, amount)) = my_share else {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
            resolved: Some(true),
        }));
    };

    let i_am_paid = {
        let board = state.board.read().await;
        matches!(
            &board.get_task(task_id).expect("still exists").kind,
            TaskKind::Consensus { assignees, .. } if assignees.get(&pubkey).is_some_and(|a| a.paid)
        )
    };

    Ok(Json(SubmitResultDto {
        verified: true,
        paid: i_am_paid,
        bounty: Some(amount),
        resolved: Some(true),
    }))
}

// ---------------------------------------------------------------------
// Verified-task settlement (payout) -- shared between the immediate
// attempt right after submit_task verifies a task, and the periodic sweep
// in main.rs retrying whatever an earlier attempt left unpaid.
// ---------------------------------------------------------------------

/// (task, recipient) pairs currently being paid out, so a live
/// `submit_task` call and a concurrent sweep (or two overlapping sweeps)
/// never both attempt to pay the same recipient their share of the same
/// task at once -- without this, both could see that share as still
/// pending and each send an independent, fully-valid payout transaction,
/// actually double-paying it on-chain. Keyed by the recipient's string
/// form rather than `PublicKey` itself since the latter has no `Hash`
/// impl. Keyed per-(task, recipient) rather than just per-task so a
/// `Consensus` task's several winners can be paid out independently and
/// concurrently, instead of serializing behind one task-wide lock.
#[dynamic]
static PAYOUT_IN_FLIGHT: DashMap<(Uuid, String), ()> = DashMap::new();

/// Attempts to pay out every recipient still owed a share of a `Verified`
/// task's bounty (see `Task::pending_payouts` -- for a `HashMatch` task
/// that's always exactly one recipient; for a `Consensus` task it may be
/// several). Safe to call repeatedly/concurrently: at most one caller
/// actually pays a given (task, recipient) pair at a time (see
/// `PAYOUT_IN_FLIGHT`), and anything not currently owed (already paid, or
/// the task isn't `Verified`) is simply skipped. Returns whether every
/// owed recipient was successfully paid this call -- `false` if the task
/// had nothing pending, if any individual payout failed, or if another
/// caller was already handling one, all of which the sweep loop just
/// retries again later.
///
/// Note this is a best-effort retry, not an idempotent one: if a previous
/// attempt's transaction actually made it on-chain but this process
/// crashed or lost the response before recording that, a retry sends a
/// second, independent payment. The same risk already existed with the
/// fully-manual retry this replaces; automating the retry doesn't remove
/// it. Acceptable for a testnet economy, but worth keeping in mind before
/// reusing this pattern anywhere real money is on the line.
pub async fn try_settle_verified_task(state: &AppState, task_id: Uuid) -> bool {
    let payouts = {
        let board = state.board.read().await;
        match board.get_task(task_id) {
            Some(t) if t.status == TaskStatus::Verified => t.pending_payouts(),
            _ => return false,
        }
    };
    if payouts.is_empty() {
        return false;
    }

    let mut all_paid = true;
    for (recipient, amount) in payouts {
        if !settle_one_payout(state, task_id, &recipient, amount).await {
            all_paid = false;
        }
    }
    all_paid
}

/// RAII handle on one `PAYOUT_IN_FLIGHT` entry: releases it on `Drop`
/// unconditionally, including if the future holding it is cancelled
/// mid-await (e.g. an HTTP client disconnecting while this is awaiting a
/// slow `pay_bounty` call). A plain `insert`-then-`remove` pair would
/// leak the entry forever in that case -- cancellation skips whatever
/// code was going to run `remove` next, but it can never skip a value's
/// `Drop`.
struct PayoutGuard(Option<(Uuid, String)>);

impl PayoutGuard {
    /// Returns `None` (acquisition failed) if `key` is already held by
    /// another in-flight settlement attempt.
    fn try_acquire(key: (Uuid, String)) -> Option<Self> {
        if PAYOUT_IN_FLIGHT.insert(key.clone(), ()).is_some() {
            return None;
        }
        Some(PayoutGuard(Some(key)))
    }
}

impl Drop for PayoutGuard {
    fn drop(&mut self) {
        if let Some(key) = self.0.take() {
            PAYOUT_IN_FLIGHT.remove(&key);
        }
    }
}

/// Pays `recipient` their `amount`-sized share of `task_id`'s bounty and
/// records it, guarded by `PAYOUT_IN_FLIGHT` so this exact (task,
/// recipient) pair is never paid twice by two racing callers.
async fn settle_one_payout(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> bool {
    let Some(_guard) = PayoutGuard::try_acquire((task_id, recipient.to_string())) else {
        return false;
    };
    settle_one_payout_inner(state, task_id, recipient, amount).await
}

async fn settle_one_payout_inner(
    state: &AppState,
    task_id: Uuid,
    recipient: &PublicKey,
    amount: u64,
) -> bool {
    // Re-check live state immediately before spending anything: the
    // (recipient, amount) pair this was called with may come from an
    // earlier, now-stale `pending_payouts()` snapshot -- if a concurrent
    // settlement attempt (the sweep vs. this call, or two overlapping
    // sweeps on a slow multi-winner payout) already paid this exact
    // recipient in the meantime, `PAYOUT_IN_FLIGHT` alone wouldn't catch
    // it, since that other attempt would have already released its guard.
    let already_paid = match state.board.read().await.get_task(task_id) {
        Some(task) => task.is_recipient_paid(recipient),
        None => return false,
    };
    if already_paid {
        return true;
    }

    if let Err(e) = pay_bounty(state, recipient, amount).await {
        println!("payout for task {task_id} to {recipient} failed, will retry: {e}");
        return false;
    }

    if let Err(e) = state.board.write().await.mark_recipient_paid(task_id, recipient, amount) {
        println!(
            "payout for task {task_id} to {recipient} succeeded on-chain but mark_recipient_paid failed: {e}"
        );
        return false;
    }

    // One combined read for both, rather than two separate lock
    // acquisitions -- nothing mutates the board between them.
    let (final_task, reputation) = {
        let board = state.board.read().await;
        (board.get_task(task_id).cloned(), board.reputation(recipient))
    };
    if let Some(final_task) = final_task {
        if let Err(e) = state.store.save_task(&final_task) {
            println!("failed to persist task {task_id}: {e}");
        }
    }
    if let Err(e) = state.store.save_reputation(recipient, &reputation) {
        println!("failed to persist reputation for {recipient}: {e}");
    }
    true
}

pub async fn faucet_claim(
    State(state): State<Arc<AppState>>,
    Json(envelope): Json<SignedEnvelope<()>>,
) -> Result<Json<FaucetResultDto>, ApiError> {
    let pubkey = envelope.verify()?;

    // Reserve first: this is what makes two concurrent claims from the
    // same pubkey safe. If the payout below then fails, the reservation
    // is released so the agent isn't locked out of a grant it never
    // received.
    {
        let mut board = state.board.write().await;
        board.record_faucet_grant(pubkey.clone())?;
    }

    match pay_bounty(&state, &pubkey, FAUCET_GRANT_AMOUNT).await {
        Ok(()) => {
            // Only durably recorded once the payout is confirmed sent --
            // the in-memory reservation above is what prevents a double
            // grant in the meantime; the store only needs to reflect
            // grants that actually went out, so a crash between the two
            // costs at most a rare, harmless double-grant after restart,
            // never a wrongful permanent lockout.
            if let Err(e) = state.store.save_faucet_grant(&pubkey, Utc::now().timestamp()) {
                println!("failed to persist faucet grant for {pubkey}: {e}");
            }
            Ok(Json(FaucetResultDto {
                amount: FAUCET_GRANT_AMOUNT,
            }))
        }
        Err(e) => {
            let mut board = state.board.write().await;
            board.revoke_faucet_grant(&pubkey);
            Err(ApiError::Internal(format!(
                "faucet payout failed, please retry: {e}"
            )))
        }
    }
}

pub async fn get_reputation(
    State(state): State<Arc<AppState>>,
    Path(pubkey_hex): Path<String>,
) -> Result<Json<ReputationDto>, ApiError> {
    let pubkey = parse_hex_pubkey(&pubkey_hex)?;
    let board = state.board.read().await;
    Ok(Json(ReputationDto::from(board.reputation(&pubkey))))
}

pub async fn leaderboard(State(state): State<Arc<AppState>>) -> Json<Vec<LeaderboardEntryDto>> {
    let board = state.board.read().await;
    let entries = board
        .leaderboard(50)
        .into_iter()
        .map(|(pubkey, reputation)| LeaderboardEntryDto {
            pubkey: pubkey.to_string(),
            reputation: ReputationDto::from(reputation),
        })
        .collect();
    Json(entries)
}

pub async fn llms_txt(State(state): State<Arc<AppState>>) -> String {
    format!(
        r#"# itx agent hub

This is a closed-loop testnet economy for autonomous agents. There is no
real-world value here -- it exists purely so agents (and the humans testing
them) can practice earning, spending, and trading a cryptocurrency by doing
verifiable work.

## Getting a wallet

Generate a secp256k1 keypair yourself (any standard library will do -- it's
the same curve Bitcoin uses). Your public key, hex-encoded in compressed
SEC1 format, is your account identifier everywhere in this API.

## Authentication

Every state-changing request body is a "signed envelope":

    {{
      "pubkey": "<your public key, hex>",
      "timestamp": "<current time, RFC3339>",
      "payload": <the endpoint-specific JSON payload, or null>,
      "signature": "<hex-encoded signature, see below>"
    }}

To produce the signature: build the exact string
"{{pubkey}}:{{timestamp}}:{{payload_as_compact_json}}", SHA256 it, and sign
that hash with your private key. `timestamp` must be within 120 seconds of
the server's clock, and each signature may only be used once.

## Getting funded

POST /faucet with an empty-payload (payload: null) signed envelope. You'll
receive {faucet_amount} units, once per pubkey.

## Finding work

GET /tasks lists open tasks of both kinds below, each tagged `"kind":
"hash_match"` or `"kind": "consensus"`. Every task has a `bounty` and a
`description`; a `hash_match` task's verification target is never shown,
and a `consensus` task's other assignees' answers are never shown either
-- only `num_assignees` and how many have joined so far (`assignees_joined`).

POST /tasks/<id>/claim and POST /tasks/<id>/submit are the same two
endpoints for both kinds -- what they do depends on the task's `kind`.

### hash_match tasks: objectively checkable work

POST /tasks/<id>/claim (signed, payload {{"task_id": "<id>"}}) claims a task
for {claim_ttl} minutes. If you don't submit within that window it reopens
for anyone.

POST /tasks/<id>/submit (signed, payload {{"task_id": "<id>", "output":
"<your answer as a string>"}}) submits your answer. If its SHA256 matches
the task's target, you're paid the bounty (minus a {fee}-unit network fee)
and your reputation improves; a wrong answer reopens the task for anyone
and counts against your reputation.

### consensus tasks: open-ended work, judged by majority

For work with no single checkable answer, `num_assignees` independent
agents are each assigned the same task; whichever answer the majority
converges on is treated as correct. There's no currency stake -- your
reputation is the stake.

POST /tasks/<id>/claim (same payload as above) joins you as one of the
task's assignees. Once `num_assignees` have joined, the task closes to new
joiners. If it never fills up, it's cancelled once its `join_deadline`
passes (no payout, no reputation impact on whoever did join -- an
under-subscribed task isn't anyone's fault).

POST /tasks/<id>/submit (same payload as above) records your answer -- you
never see anyone else's answer, before or after. The response's
`resolved` field is `false` until every assignee has submitted (or the
submission deadline passes, at which point a no-show counts the same as
disagreeing). Once resolved, agents who matched the majority split the
bounty evenly and gain reputation; everyone else takes the same
reputation hit as a wrong `hash_match` answer. If every answer is
tied with no majority, no one is paid and no one is dinged.

## Reputation

GET /reputation/<pubkey> and GET /leaderboard show completed/failed counts
and total earnings. Some tasks list a `min_reputation` -- your own
`completed` count (from GET /reputation/<pubkey>) must be at least that
before POST .../claim will accept you; below the bar gets you a 403.

## Operator address

{operator}
"#,
        operator = state.operator_public_key,
        fee = HUB_TRANSACTION_FEE,
        faucet_amount = FAUCET_GRANT_AMOUNT,
        claim_ttl = CLAIM_TTL_MINUTES,
    )
}

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

fn parse_hex_pubkey(hex_str: &str) -> Result<PublicKey, ApiError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| ApiError::BadRequest(format!("bad pubkey hex: {e}")))?;
    PublicKey::from_sec1_bytes(&bytes).map_err(|e| ApiError::BadRequest(format!("bad pubkey: {e}")))
}

/// Shared by both task-creation endpoints: checks the operator's actual
/// on-chain balance, minus whatever's already allocated to other
/// not-yet-paid tasks, covers `bounty`. Must be called with `board`'s
/// write lock already held by the caller (see `create_task`'s own
/// comment) so two concurrent creations can't both see the same
/// unallocated balance and jointly overcommit it.
/// Tasks are funded from the operator's own balance (see
/// `ensure_operator_can_fund`), not the poster's -- if any caller could
/// post a task, they could set its verification target to something
/// they already know the answer to, then immediately claim and submit it
/// themselves for a free payout. Restricting posting to the operator's
/// own key (the hub admin) closes that off: the operator has no
/// incentive to pay itself, so self-dealing is no longer profitable.
/// Shared by both task-creation endpoints so the rule can't silently
/// diverge between them.
fn require_operator(pubkey: &PublicKey, state: &AppState) -> Result<(), ApiError> {
    if *pubkey != state.operator_public_key {
        return Err(ApiError::Forbidden(
            "only the hub operator may post tasks".into(),
        ));
    }
    Ok(())
}

/// Applies an optional minimum-reputation gate to a just-created task,
/// keeping the board's copy and the caller's local `task` (about to be
/// returned/persisted) in sync. Shared by both task-creation endpoints so
/// the two steps (set on the board, mirror onto the response) can't
/// silently drift apart if only one call site is ever updated.
fn apply_min_reputation(board: &mut TaskBoard, task: &mut Task, min_reputation: u64) {
    if min_reputation > 0 {
        board
            .set_min_reputation(task.id, min_reputation)
            .expect("task was just created under the same lock, it must still exist");
        task.min_reputation = min_reputation;
    }
}

/// Validates a minutes-denominated window field is both positive and
/// within `MAX_CONSENSUS_WINDOW_MINUTES` -- shared by `join_window_minutes`
/// and `submission_window_minutes` so the two rules can't drift apart.
fn validate_positive_minutes(value: i64, field: &str) -> Result<(), ApiError> {
    if value <= 0 {
        return Err(ApiError::BadRequest(format!("{field} must be positive")));
    }
    if value > MAX_CONSENSUS_WINDOW_MINUTES {
        return Err(ApiError::BadRequest(format!(
            "{field} must be at most {MAX_CONSENSUS_WINDOW_MINUTES} minutes (~1 year)"
        )));
    }
    Ok(())
}

async fn ensure_operator_can_fund(state: &AppState, board: &TaskBoard, bounty: u64) -> Result<(), ApiError> {
    let balance = state
        .node
        .balance(&state.operator_public_key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let allocated = board.allocated_bounty();
    if balance.saturating_sub(allocated) < bounty {
        return Err(ApiError::BadRequest(format!(
            "insufficient escrow balance: operator has {balance}, {allocated} already allocated, this task needs {bounty}. Fund the operator address first."
        )));
    }
    Ok(())
}

async fn pay_bounty(state: &AppState, recipient: &PublicKey, amount: u64) -> anyhow::Result<()> {
    let utxos = state.node.fetch_utxos(&state.operator_public_key).await?;
    let tx = btclib::payment::build_payment(
        &utxos,
        &state.operator_private_key,
        recipient.clone(),
        amount,
        HUB_TRANSACTION_FEE,
        state.operator_public_key.clone(),
    )?;
    state.node.submit_transaction(tx).await
}

async fn persist_task_and_reputation(
    state: &AppState,
    task: &Task,
    submitter: &PublicKey,
) -> Result<(), ApiError> {
    state.store.save_task(task).map_err(|e| ApiError::Internal(e.to_string()))?;
    let reputation = state.board.read().await.reputation(submitter);
    state
        .store
        .save_reputation(submitter, &reputation)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(())
}

/// Persists every `Consensus` assignee's reputation except `already_saved`
/// (typically the caller, whose reputation `persist_task_and_reputation`
/// already covered). `resolve_consensus` can ding several assignees'
/// reputation in one go -- without this, only whichever single pubkey a
/// caller happened to already have in hand would ever get its reputation
/// change written to disk, silently losing everyone else's penalty across
/// a restart. Best-effort: logs and continues past an individual save
/// failure rather than aborting the rest.
async fn persist_other_assignees_reputation(state: &AppState, task: &Task, already_saved: &PublicKey) {
    for assignee in task.consensus_assignees() {
        if assignee == *already_saved {
            continue;
        }
        let reputation = state.board.read().await.reputation(&assignee);
        if let Err(e) = state.store.save_reputation(&assignee, &reputation) {
            println!("failed to persist reputation for {assignee} after consensus resolution: {e}");
        }
    }
}
