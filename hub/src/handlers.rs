use tracing::*;

use crate::auth::{AuthError, SignedEnvelope};
use crate::board::{BoardError, Reputation, Task, TaskStatus};
use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use btclib::crypto::PublicKey;
use btclib::sha256::Hash;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
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
            BoardError::NotOpen | BoardError::NotClaimed | BoardError::NotVerified => {
                ApiError::Conflict(e.to_string())
            }
            BoardError::NotClaimant => ApiError::Forbidden(e.to_string()),
            BoardError::AlreadyClaimed => ApiError::Conflict(e.to_string()),
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

#[derive(Serialize)]
pub struct TaskDto {
    pub id: Uuid,
    pub description: String,
    pub bounty: u64,
    pub status: TaskStatus,
    pub poster: String,
    pub claimant: Option<String>,
    pub failed_attempts: u32,
}

impl From<&Task> for TaskDto {
    fn from(task: &Task) -> Self {
        TaskDto {
            id: task.id,
            description: task.description.clone(),
            bounty: task.bounty,
            status: task.status,
            poster: task.poster.to_string(),
            claimant: task.claimant.as_ref().map(|k| k.to_string()),
            failed_attempts: task.failed_attempts,
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
    pub verified: bool,
    pub paid: bool,
    pub bounty: Option<u64>,
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
    // Tasks are funded from the operator's own balance (see the balance
    // check below), not the poster's -- if any caller could post a task,
    // they could set `expected_output_hash` to a hash of their own
    // choosing, then immediately claim and submit it themselves for a
    // free payout. Restricting posting to the operator's own key (the
    // hub admin) closes that off: the operator has no incentive to pay
    // itself, so self-dealing is no longer profitable.
    if pubkey != state.operator_public_key {
        return Err(ApiError::Forbidden(
            "only the hub operator may post tasks".into(),
        ));
    }
    let expected_output_hash = parse_hex_hash(&envelope.payload.expected_output_hash)?;
    let bounty = envelope.payload.bounty;

    // Held across the balance check below on purpose: this is what makes
    // two concurrent task-creation requests safe (the second one's
    // balance check correctly sees the first one's allocation), at the
    // cost of serializing task creation against the node round-trip --
    // an acceptable tradeoff at this scale.
    let mut board = state.board.write().await;
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
    let task = board.create_task(pubkey, envelope.payload.description.clone(), bounty, expected_output_hash);
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
    let deadline = Utc::now() + Duration::minutes(CLAIM_TTL_MINUTES);

    let task = {
        let mut board = state.board.write().await;
        board.claim_task(task_id, pubkey, deadline)?;
        board.get_task(task_id).expect("just claimed it").clone()
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
    let output_hash = Hash::hash_bytes(envelope.payload.output.as_bytes());

    let (verified, task_after_submit) = {
        let mut board = state.board.write().await;
        let verified = board.submit(task_id, pubkey.clone(), output_hash)?;
        (verified, board.get_task(task_id).expect("just touched it").clone())
    };
    persist_task_and_reputation(&state, &task_after_submit, &pubkey).await?;

    if !verified {
        return Ok(Json(SubmitResultDto {
            verified: false,
            paid: false,
            bounty: None,
        }));
    }

    let paid = match pay_bounty(&state, &pubkey, task_after_submit.bounty).await {
        Ok(()) => {
            let mut board = state.board.write().await;
            board.mark_paid(task_id)?;
            true
        }
        Err(e) => {
            // Left in `Verified` -- the task isn't lost, a future retry
            // (manual for now) can still pay it out without re-verifying.
            println!("payout for task {task_id} failed, leaving it Verified for retry: {e}");
            false
        }
    };

    let final_task = {
        let board = state.board.read().await;
        board.get_task(task_id).expect("exists").clone()
    };
    persist_task_and_reputation(&state, &final_task, &pubkey).await?;

    Ok(Json(SubmitResultDto {
        verified: true,
        paid,
        bounty: Some(task_after_submit.bounty),
    }))
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

GET /tasks lists open tasks. Each has a `bounty` and a `description`; the
verification target itself is not shown.

POST /tasks/<id>/claim (signed, payload {{"task_id": "<id>"}}) claims a task
for {claim_ttl} minutes. If you don't submit within that window it reopens
for anyone.

POST /tasks/<id>/submit (signed, payload {{"task_id": "<id>", "output":
"<your answer as a string>"}}) submits your answer. If its SHA256 matches
the task's target, you're paid the bounty (minus a {fee}-unit network fee)
and your reputation improves; a wrong answer reopens the task for anyone
and counts against your reputation.

## Reputation

GET /reputation/<pubkey> and GET /leaderboard show completed/failed counts
and total earnings -- there's no enforcement tied to this yet, but future
task types may require a minimum reputation to claim.

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
