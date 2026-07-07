//! End-to-end smoke test for the hub HTTP API. Not a unit test -- this
//! drives a real running `hub` (and the node/miner behind it) over HTTP,
//! exactly like an external agent would. Run manually:
//!
//!   cargo run -p hub --example smoke_agent -- <hub_base_url> <operator_priv_key_file> [node_address]

use anyhow::{Context, Result};
use btclib::crypto::{PrivateKey, Signature};
use btclib::network::Message;
use btclib::sha256::Hash;
use btclib::util::Saveable;
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::net::TcpStream;

// Mirrors of `hub::handlers::{CreateTaskPayload, ClaimPayload,
// SubmitPayload}` -- field name AND declaration order must match exactly.
// The signing string is built from `serde_json::to_string(&payload)`, and
// the server independently recomputes that same string from its own
// deserialized, statically-typed struct (in declaration order), not from
// whatever bytes were on the wire. A `serde_json::Value` built via `json!`
// would serialize its keys alphabetically instead (it's backed by a
// `BTreeMap`), silently producing a different string and failing
// signature verification for any struct whose fields aren't already in
// alphabetical order.
#[derive(Serialize)]
struct CreateTaskPayload {
    description: String,
    bounty: u64,
    expected_output_hash: String,
}

#[derive(Serialize)]
struct ClaimPayload {
    task_id: String,
}

#[derive(Serialize)]
struct SubmitPayload {
    task_id: String,
    output: String,
}

/// Reproduces exactly what `hub::auth::SignedEnvelope::verify` expects:
/// hex(pubkey) + ":" + rfc3339(timestamp) + ":" + json(payload), SHA256'd
/// and signed. Any HTTP client in any language builds requests this way --
/// this function is the reference implementation restated in Rust.
fn build_envelope<T: Serialize>(private_key: &PrivateKey, payload: T) -> Value {
    let pubkey_hex = private_key.public_key().to_string();
    let timestamp = Utc::now().to_rfc3339();
    let payload_json = serde_json::to_string(&payload).expect("payload must serialize");
    let signing_string = format!("{pubkey_hex}:{timestamp}:{payload_json}");
    let hash = Hash::hash_bytes(signing_string.as_bytes());
    let signature = Signature::sign_hash(&hash, private_key);
    json!({
        "pubkey": pubkey_hex,
        "timestamp": timestamp,
        "payload": payload,
        "signature": hex::encode(signature.to_bytes()),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let base_url = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:9100".to_string());
    let operator_key_file = args
        .next()
        .context("usage: smoke_agent <hub_base_url> <operator_priv_key_file>")?;

    let operator_key = PrivateKey::load_from_file(&operator_key_file)
        .map_err(|e| anyhow::anyhow!("failed to load operator private key: {e}"))?;
    let agent_key = PrivateKey::new_key();
    println!("agent pubkey: {}", agent_key.public_key());

    let client = reqwest::Client::new();

    println!("\n== GET /llms.txt ==");
    let llms = client
        .get(format!("{base_url}/llms.txt"))
        .send()
        .await?
        .text()
        .await?;
    println!("({} bytes)", llms.len());
    assert!(llms.contains("itx agent hub"));

    println!("\n== POST /faucet (agent) ==");
    let envelope = build_envelope(&agent_key, ());
    let resp = client
        .post(format!("{base_url}/faucet"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await?;
    println!("status={status} body={body}");
    assert!(status.is_success(), "faucet claim should succeed");

    println!("\n== POST /faucet again (should be rejected, already claimed) ==");
    let envelope = build_envelope(&agent_key, ());
    let resp = client
        .post(format!("{base_url}/faucet"))
        .json(&envelope)
        .send()
        .await?;
    println!("status={}", resp.status());
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);

    println!("\n== POST /tasks as a non-operator (should be rejected) ==");
    let impostor_key = PrivateKey::new_key();
    let payload = CreateTaskPayload {
        description: "should be rejected".to_string(),
        bounty: 10,
        expected_output_hash: hex::encode(Hash::hash_bytes(b"x").as_bytes()),
    };
    let envelope = build_envelope(&impostor_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks"))
        .json(&envelope)
        .send()
        .await?;
    println!("status={}", resp.status());
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    println!("\n== POST /tasks as the operator ==");
    let correct_answer = "the answer is 42";
    let expected_hash = hex::encode(Hash::hash_bytes(correct_answer.as_bytes()).as_bytes());
    let payload = CreateTaskPayload {
        description: "reply with the answer to everything".to_string(),
        bounty: 1_000_000u64,
        expected_output_hash: expected_hash,
    };
    let envelope = build_envelope(&operator_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let task: Value = resp.json().await?;
    println!("status={status} task={task}");
    assert!(status.is_success(), "operator task creation should succeed");
    let task_id = task["id"].as_str().unwrap().to_string();

    println!("\n== GET /tasks (should list the new task) ==");
    let tasks: Value = client
        .get(format!("{base_url}/tasks"))
        .send()
        .await?
        .json()
        .await?;
    println!("{tasks}");
    // >= 1 (not == 1) since the hub's store persists across runs -- a
    // repeat run of this smoke test against the same store will see
    // whatever earlier runs left behind too.
    assert!(tasks
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == task_id));

    println!("\n== POST /tasks/{{id}}/claim (agent) ==");
    let payload = ClaimPayload {
        task_id: task_id.clone(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/claim"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let claimed_task: Value = resp.json().await?;
    println!("status={status} task={claimed_task}");
    assert!(status.is_success());
    assert_eq!(claimed_task["status"], "Claimed");

    println!("\n== POST /tasks/{{id}}/submit with the WRONG answer ==");
    let payload = SubmitPayload {
        task_id: task_id.clone(),
        output: "definitely wrong".to_string(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/submit"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let result: Value = resp.json().await?;
    println!("status={status} result={result}");
    assert!(status.is_success());
    assert_eq!(result["verified"], false);

    println!("\n== re-claim then submit the CORRECT answer ==");
    let payload = ClaimPayload {
        task_id: task_id.clone(),
    };
    let envelope = build_envelope(&agent_key, payload);
    client
        .post(format!("{base_url}/tasks/{task_id}/claim"))
        .json(&envelope)
        .send()
        .await?
        .error_for_status()?;

    let payload = SubmitPayload {
        task_id: task_id.clone(),
        output: correct_answer.to_string(),
    };
    let envelope = build_envelope(&agent_key, payload);
    let resp = client
        .post(format!("{base_url}/tasks/{task_id}/submit"))
        .json(&envelope)
        .send()
        .await?;
    let status = resp.status();
    let result: Value = resp.json().await?;
    println!("status={status} result={result}");
    assert!(status.is_success());
    assert_eq!(result["verified"], true);
    assert_eq!(result["paid"], true);

    println!("\n== GET /reputation/{{agent_pubkey}} ==");
    let agent_pubkey = agent_key.public_key().to_string();
    let reputation: Value = client
        .get(format!("{base_url}/reputation/{agent_pubkey}"))
        .send()
        .await?
        .json()
        .await?;
    println!("{reputation}");
    assert_eq!(reputation["completed"], 1);
    assert_eq!(reputation["failed"], 1);
    assert_eq!(reputation["total_earned"], 1_000_000);

    println!("\n== GET /leaderboard ==");
    let leaderboard: Value = client
        .get(format!("{base_url}/leaderboard"))
        .send()
        .await?
        .json()
        .await?;
    println!("{leaderboard}");
    // Same as the /tasks check above: >= 1, not == 1, since the store
    // persists across repeat runs of this smoke test.
    assert!(leaderboard
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["pubkey"] == agent_pubkey && entry["total_earned"] == 1_000_000));

    println!("\n== verify on-chain settlement directly against the node (bypassing hub) ==");
    let node_address = args.next().unwrap_or_else(|| "127.0.0.1:9000".to_string());
    let mut confirmed_balance = 0u64;
    for attempt in 1..=10 {
        let mut stream = TcpStream::connect(&node_address).await?;
        btclib::network::perform_handshake_initiator(&mut stream)
            .await
            .map_err(|e| anyhow::anyhow!("handshake with {node_address} failed: {e}"))?;
        Message::FetchUTXOs(agent_key.public_key())
            .send_async(&mut stream)
            .await?;
        match Message::receive_async(&mut stream).await? {
            Message::UTXOs(utxos) => {
                confirmed_balance = utxos
                    .iter()
                    .filter(|(_, marked)| !marked)
                    .map(|(output, _)| output.value)
                    .sum();
            }
            other => anyhow::bail!("unexpected response from node: {other:?}"),
        }
        if confirmed_balance > 0 {
            break;
        }
        println!("  (attempt {attempt}: payouts not yet mined, waiting for the next block...)");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    println!("agent's confirmed on-chain balance: {confirmed_balance}");
    assert_eq!(
        confirmed_balance,
        50_000_000 + 1_000_000,
        "agent should have actually received both the faucet grant and the task bounty on-chain, not just a hub-side bookkeeping entry"
    );

    println!("\nALL SMOKE TESTS PASSED");
    Ok(())
}
