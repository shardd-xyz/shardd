use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_consensus::{TxLegacy, transaction::RlpEcdsaDecodableTx};
use alloy_primitives::{B256, TxKind, U256, keccak256};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shardd_types::{CreateEventRequest, Event, NodeRpcRequest, NodeRpcResponse};
use uuid::Uuid;

use crate::AppState;

// ── JSON-RPC wire types ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct EvmRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Option<Value>,
    id: Value,
}

#[derive(Debug, Serialize)]
struct EvmRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<EvmRpcErrorBody>,
    id: Value,
}

#[derive(Debug, Serialize)]
struct EvmRpcErrorBody {
    code: i64,
    message: String,
}

/// Cached EVM status for a bucket. Refreshed from the dashboard
/// periodically so the edge doesn't call the dashboard on every txn.
#[derive(Clone, Debug)]
pub(crate) struct BucketEvmState {
    enabled: bool,
    paused: bool,
    refreshed_at: Instant,
}

impl BucketEvmState {
    fn disabled() -> Self {
        Self {
            enabled: false,
            paused: false,
            refreshed_at: Instant::now(),
        }
    }
    fn is_stale(&self, ttl: Duration) -> bool {
        self.refreshed_at.elapsed() > ttl
    }
}

const EMPTY_BLOOM: &str = "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const ZERO_HASH: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
const UNCLES_HASH: &str = "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";

/// How long the edge caches a bucket's EVM state before re-querying
/// the dashboard. The dashboard is only hit at most once per bucket
/// per this interval, not per transaction.
const EVM_STATE_CACHE_TTL: Duration = Duration::from_secs(30);

// ── Public handler ───────────────────────────────────────────────────

pub async fn evm_rpc_handler(
    State(state): State<AppState>,
    Path((user_id, bucket)): Path<(String, String)>,
    Query(_query_params): Query<HashMap<String, String>>,
    _headers: HeaderMap,
    body: String,
) -> Response {
    // Handle empty body (GET requests from wallets) — default to eth_chainId
    let req: EvmRpcRequest = if body.trim().is_empty() {
        EvmRpcRequest {
            jsonrpc: "2.0".into(),
            method: "eth_chainId".into(),
            params: None,
            id: Value::Number(0.into()),
        }
    } else {
        match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                return make_error(null_value(), -32700, &format!("parse error: {e}"));
            }
        }
    };

    // Parse user_id and compute the internal bucket name (same format
    // the dashboard uses: `user_{id}__bucket_{hex(name)}`).
    let uid = match Uuid::parse_str(&user_id) {
        Ok(u) => u,
        Err(e) => return make_error(null_value(), -32602, &format!("invalid user id: {e}")),
    };
    let internal_bucket = crate::internal_bucket_for_user(uid, &bucket);

    let cache_key = format!("{user_id}:{bucket}");
    let internal_bucket_ref = internal_bucket.as_str();

    let needs_write = matches!(req.method.as_str(), "eth_sendRawTransaction");
    if needs_write {
        if let Err(resp) = ensure_evm_ready(&state, &cache_key, &user_id, &bucket).await {
            return resp;
        }
    }

    let result = match req.method.as_str() {
        "eth_chainId" | "net_version" => eth_chain_id(&bucket),
        "eth_accounts" => Ok(json!([])),
        "eth_getBalance" => eth_get_balance(&state, internal_bucket_ref, &req.params).await,
        "eth_getTransactionCount" => eth_get_transaction_count(&state, internal_bucket_ref, &req.params).await,
        "eth_sendRawTransaction" => eth_send_raw_transaction(&state, internal_bucket_ref, &bucket, &req.params).await,
        "eth_gasPrice" => Ok(json!("0x0")),
        "eth_estimateGas" => Ok(json!("0x5208")),
        "eth_blockNumber" => eth_block_number(&state, internal_bucket_ref).await,
        "eth_getBlockByNumber" => eth_get_block_by_number(&state, internal_bucket_ref, &req.params).await,
        "eth_getBlockByHash" => eth_get_block_by_hash(&state, internal_bucket_ref, &req.params).await,
        "eth_getTransactionByHash" => {
            eth_get_transaction_by_hash(&state, internal_bucket_ref, &req.params).await
        }
        "eth_getTransactionReceipt" => {
            eth_get_transaction_receipt(&state, internal_bucket_ref, &req.params).await
        }
        "eth_call" => Err(evm_rpc_err(-32601, "smart contracts not supported")),
        "eth_getLogs" => Ok(json!([])),
        "web3_clientVersion" => Ok(json!("shardd-evm/0.1")),
        "net_listening" => Ok(json!(false)),
        "net_peerCount" => Ok(json!("0x0")),
        "eth_feeHistory" => Ok(json!({
            "oldestBlock": "0x0",
            "baseFeePerGas": ["0x0"],
            "gasUsedRatio": [],
            "reward": []
        })),
        _ => Err(evm_rpc_err(
            -32601,
            &format!("unknown method: {}", req.method),
        )),
    };

    match result {
        Ok(val) => make_ok(req.id, val),
        Err(err) => make_error(req.id, err.code, &err.message),
    }
}

// ── Cached EVM state ─────────────────────────────────────────────────

/// Returns Ok(()) if the bucket has EVM enabled and is not paused.
/// Caches the dashboard response for EVM_STATE_CACHE_TTL.
async fn ensure_evm_ready(state: &AppState, cache_key: &str, user_id: &str, bucket: &str) -> Result<(), Response> {
    let cache = state.evm_state.as_ref();
    let Some(cache) = cache else {
        // No dashboard URL configured — cannot verify EVM state.
        // Allow writes (the dashboard is the source of truth; if it's
        // unreachable we default to open).
        return Ok(());
    };

    // Fast path: cache hit and still fresh.
    if let Some(ref entry) = cache.get(cache_key) {
        if !entry.is_stale(EVM_STATE_CACHE_TTL) {
            if !entry.value().enabled {
                return Err(make_error_with(
                    null_value(),
                    -32000,
                    "EVM RPC not enabled for this bucket",
                ));
            }
            if entry.value().paused {
                return Err(make_error_with(null_value(), -32000, "bucket is paused"));
            }
            return Ok(());
        }
    }

    // Slow path: refresh from dashboard (at most once per TTL).
    let state_ref = fetch_evm_state(state, user_id, bucket).await;
    cache.insert(cache_key.to_string(), state_ref.clone());

    if !state_ref.enabled {
        return Err(make_error_with(
            null_value(),
            -32000,
            "EVM RPC not enabled for this bucket",
        ));
    }
    if state_ref.paused {
        return Err(make_error_with(null_value(), -32000, "bucket is paused"));
    }
    Ok(())
}

async fn fetch_evm_state(state: &AppState, user_id: &str, bucket: &str) -> BucketEvmState {
    let Some(auth) = &state.auth else {
        return BucketEvmState::disabled();
    };

    let resp = match auth
        .http
        .post(format!("{}/api/machine/evm/check", auth.base_url))
        .header("x-machine-auth-secret", &auth.shared_secret)
        .json(&json!({
            "user_id": user_id,
            "bucket_name": bucket,
            "address": "",
            "action": "state",
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return BucketEvmState::disabled(),
    };

    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return BucketEvmState::disabled(),
    };

    let enabled = body
        .get("evm_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let paused = body
        .get("evm_paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    BucketEvmState {
        enabled,
        paused,
        refreshed_at: Instant::now(),
    }
}

// ── Error helpers ────────────────────────────────────────────────────

fn evm_rpc_err(code: i64, message: &str) -> EvmRpcErrorBody {
    EvmRpcErrorBody {
        code,
        message: message.to_string(),
    }
}

fn null_value() -> Value {
    Value::Null
}

fn make_ok(id: Value, result: Value) -> Response {
    Json(EvmRpcResponse {
        jsonrpc: "2.0",
        result: Some(result),
        error: None,
        id,
    })
    .into_response()
}

fn make_error(id: Value, code: i64, message: &str) -> Response {
    make_error_with(id, code, message)
}

fn make_error_with(id: Value, code: i64, message: &str) -> Response {
    Json(EvmRpcResponse {
        jsonrpc: "2.0",
        result: None,
        error: Some(EvmRpcErrorBody {
            code,
            message: message.to_string(),
        }),
        id,
    })
    .into_response()
}

// ── Param extraction ─────────────────────────────────────────────────

fn extract_param_str(params: &Option<Value>, index: usize) -> Result<String, EvmRpcErrorBody> {
    let arr = params
        .as_ref()
        .and_then(|v| v.as_array())
        .ok_or_else(|| evm_rpc_err(-32602, "params must be an array"))?;
    let val = arr
        .get(index)
        .ok_or_else(|| evm_rpc_err(-32602, &format!("missing param at index {index}")))?;
    match val {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(ZERO_ADDR.to_string()),
        _ => Ok(val.to_string()),
    }
}

// ── Read-only methods ────────────────────────────────────────────────

fn eth_chain_id(bucket: &str) -> Result<Value, EvmRpcErrorBody> {
    let hash = keccak256(bucket.as_bytes());
    // Use the first 4 bytes as a u32 chain ID — small enough for all wallets
    let chain_id = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) as u64;
    Ok(json!(format!("0x{:x}", chain_id)))
}

fn chain_id_for_bucket(bucket: &str) -> u64 {
    let hash = keccak256(bucket.as_bytes());
    u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) as u64
}

async fn eth_get_balance(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let address = extract_param_str(params, 0)?;
    let balances = query_balances(state, bucket).await?;
    let balance = balances
        .iter()
        .find(|b| b.account.to_lowercase() == address.to_lowercase())
        .map(|b| b.balance)
        .unwrap_or(0);
    let wei = balance.max(0) as u64;
    Ok(json!(format!("0x{:x}", wei)))
}

async fn eth_get_transaction_count(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let address = extract_param_str(params, 0)?;
    let events = get_bucket_events_sorted(state, bucket).await?;
    let nonce = events
        .iter()
        .filter(|e| e.account.to_lowercase() == address.to_lowercase() && e.amount < 0)
        .count() as u64;
    Ok(json!(format!("0x{:x}", nonce)))
}

async fn eth_block_number(state: &AppState, bucket: &str) -> Result<Value, EvmRpcErrorBody> {
    let events = get_bucket_events_sorted(state, bucket).await?;
    let count = events.len() as u64;
    Ok(json!(format!("0x{:x}", count)))
}

// ── eth_sendRawTransaction ───────────────────────────────────────────

async fn eth_send_raw_transaction(
    state: &AppState,
    bucket: &str,
    raw_bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let raw_hex = extract_param_str(params, 0)?;
    let raw_bytes = hex::decode(raw_hex.trim_start_matches("0x"))
        .map_err(|_| evm_rpc_err(-32602, "invalid hex"))?;

    // 1. Decode signed legacy transaction via RLP
    let tx = TxLegacy::rlp_decode_signed(&mut &raw_bytes[..])
        .map_err(|e| evm_rpc_err(-32602, &format!("invalid transaction: {e}")))?;

    // 2. Recover signer — the signature IS the auth
    let from = tx
        .recover_signer()
        .map_err(|_| evm_rpc_err(-32602, "signature recovery failed"))?;
    let from_str = format!("0x{:x}", from);

    // 3. Extract fields
    let tx_obj = tx.tx();
    let to_addr = match tx_obj.to {
        TxKind::Call(addr) => addr,
        TxKind::Create => {
            return Err(evm_rpc_err(-32602, "contract creation not supported"));
        }
    };
    let to_str = format!("0x{:x}", to_addr);
    let value: U256 = tx_obj.value;
    let nonce = tx_obj.nonce as u64;
    let tx_chain_id = tx_obj.chain_id.unwrap_or(0);

    // 4. Validate chain_id (uses raw bucket name — same as what wallets derive)
    let expected_chain = chain_id_for_bucket(raw_bucket);
    if tx_chain_id != expected_chain {
        return Err(evm_rpc_err(
            -32000,
            &format!(
                "wrong chain_id: got {}, expected {}",
                tx_chain_id, expected_chain
            ),
        ));
    }

    // 5. Validate value fits i64
    let value_i64: i64 = value
        .try_into()
        .map_err(|_| evm_rpc_err(-32000, "value exceeds i64 range"))?;

    // 6. Validate nonce (strict sequential)
    let current_nonce = get_nonce(state, bucket, &from_str).await?;
    if nonce != current_nonce {
        return Err(evm_rpc_err(
            -32000,
            &format!(
                "nonce too {}: got {}, expected {}",
                if nonce < current_nonce { "low" } else { "high" },
                nonce,
                current_nonce
            ),
        ));
    }

    // 7. Compute tx_hash
    let tx_hash = format!("0x{:x}", keccak256(&raw_bytes));
    let idempotency_nonce = tx_hash.clone();

    // 8. Create transfer event
    let note = json!({
        "evm": {
            "tx_hash": tx_hash,
            "to": to_str,
            "nonce": nonce,
            "chain_id": expected_chain,
        }
    })
    .to_string();

    let request = CreateEventRequest {
        bucket: bucket.to_string(),
        account: from_str.clone(),
        amount: -value_i64,
        note: Some(note),
        idempotency_nonce: idempotency_nonce.clone(),
        max_overdraft: None,
        min_acks: None,
        ack_timeout_ms: None,
        hold_amount: None,
        hold_expires_at_unix_ms: None,
        settle_reservation: None,
        release_reservation: None,
        skip_hold: Some(true),
        allow_reserved_bucket: false,
        transfer_to: Some(to_str),
    };

    let node_result = state
        .mesh
        .create_event(request)
        .await
        .map_err(|e| evm_rpc_err(-32000, &format!("mesh error: {e}")))?;

    let _response = node_result.map_err(|e| evm_rpc_err(-32000, &format!("node error: {e:?}")))?;

    Ok(json!(tx_hash))
}

// ── Block/Tx/Receipt queries ─────────────────────────────────────────

async fn eth_get_block_by_number(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let block_num_str = extract_param_str(params, 0)?;
    let events = get_bucket_events_sorted(state, bucket).await?;
    if events.is_empty() {
        return Ok(json!(null));
    }
    let block_num = if block_num_str == "latest" || block_num_str == "pending" {
        events.len().saturating_sub(1)
    } else if block_num_str == "earliest" {
        0
    } else {
        let hex_str = block_num_str.trim_start_matches("0x");
        usize::from_str_radix(hex_str, 16).unwrap_or(0)
    };
    if block_num >= events.len() {
        return Ok(json!(null));
    }
    build_block_response(&events, block_num)
}

async fn eth_get_block_by_hash(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let hash_str = extract_param_str(params, 0)?;
    let events = get_bucket_events_sorted(state, bucket).await?;
    for (idx, event) in events.iter().enumerate() {
        let bh = block_hash_for_event(event);
        if format!("0x{:x}", bh) == hash_str.to_lowercase() {
            return build_block_response(&events, idx);
        }
    }
    Ok(json!(null))
}

async fn eth_get_transaction_by_hash(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let tx_hash_str = extract_param_str(params, 0)?;
    let events = get_bucket_events_sorted(state, bucket).await?;
    for (idx, event) in events.iter().enumerate() {
        let event_tx_hash = extract_tx_hash_from_event(event);
        let synthetic = keccak256(event.event_id.as_bytes());
        if format!("0x{:x}", event_tx_hash) == tx_hash_str.to_lowercase()
            || format!("0x{:x}", synthetic) == tx_hash_str.to_lowercase()
        {
            return build_tx_object(event, idx);
        }
    }
    Ok(json!(null))
}

async fn eth_get_transaction_receipt(
    state: &AppState,
    bucket: &str,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let tx_hash_str = extract_param_str(params, 0)?;
    let events = get_bucket_events_sorted(state, bucket).await?;
    for (idx, event) in events.iter().enumerate() {
        let event_tx_hash = extract_tx_hash_from_event(event);
        let synthetic = keccak256(event.event_id.as_bytes());
        if format!("0x{:x}", event_tx_hash) == tx_hash_str.to_lowercase()
            || format!("0x{:x}", synthetic) == tx_hash_str.to_lowercase()
        {
            let to_addr = extract_to_from_event(event);
            let from_addr = &event.account;
            return Ok(json!({
                "transactionHash": tx_hash_str,
                "transactionIndex": "0x0",
                "blockNumber": format!("0x{:x}", idx),
                "blockHash": format!("0x{:x}", block_hash_for_event(event)),
                "from": from_addr,
                "to": to_addr,
                "cumulativeGasUsed": "0x5208",
                "gasUsed": "0x5208",
                "contractAddress": null,
                "logs": [],
                "logsBloom": EMPTY_BLOOM,
                "status": "0x1",
                "effectiveGasPrice": "0x0",
            }));
        }
    }
    Ok(json!(null))
}

// ── Helpers ──────────────────────────────────────────────────────────

fn block_hash_for_event(event: &Event) -> B256 {
    keccak256(event.event_id.as_bytes())
}

fn extract_tx_hash_from_event(event: &Event) -> B256 {
    if let Some(ref note) = event.note {
        if let Ok(parsed) = serde_json::from_str::<Value>(note) {
            if let Some(tx_hash) = parsed
                .get("evm")
                .and_then(|v| v.get("tx_hash"))
                .and_then(|v| v.as_str())
            {
                let stripped = tx_hash.trim_start_matches("0x");
                if let Ok(bytes) = hex::decode(stripped) {
                    if bytes.len() == 32 {
                        return B256::from_slice(&bytes);
                    }
                }
            }
        }
    }
    keccak256(event.event_id.as_bytes())
}

fn extract_to_from_event(event: &Event) -> String {
    if let Some(ref note) = event.note {
        if let Ok(parsed) = serde_json::from_str::<Value>(note) {
            if let Some(to) = parsed
                .get("evm")
                .and_then(|v| v.get("to"))
                .and_then(|v| v.as_str())
            {
                return to.to_string();
            }
        }
    }
    ZERO_ADDR.to_string()
}

fn build_tx_object(event: &Event, block_num: usize) -> Result<Value, EvmRpcErrorBody> {
    let tx_hash = extract_tx_hash_from_event(event);
    let to_addr = extract_to_from_event(event);
    let from_addr = &event.account;
    let block_hash = block_hash_for_event(event);

    let nonce = event.note.as_ref().and_then(|n| {
        serde_json::from_str::<Value>(n)
            .ok()
            .and_then(|v| v.get("evm")?.get("nonce")?.as_u64())
    });

    let value = if event.amount < 0 {
        event.amount.unsigned_abs() as u64
    } else {
        0
    };

    Ok(json!({
        "hash": format!("0x{:x}", tx_hash),
        "nonce": format!("0x{:x}", nonce.unwrap_or(0)),
        "blockHash": format!("0x{:x}", block_hash),
        "blockNumber": format!("0x{:x}", block_num),
        "transactionIndex": "0x0",
        "from": from_addr,
        "to": to_addr,
        "value": format!("0x{:x}", value),
        "gas": "0x5208",
        "gasPrice": "0x0",
        "input": "0x",
        "chainId": "0x0",
        "v": "0x0",
        "r": ZERO_HASH,
        "s": ZERO_HASH,
    }))
}

fn build_block_response(events: &[Event], block_num: usize) -> Result<Value, EvmRpcErrorBody> {
    let event = &events[block_num];
    let block_hash = block_hash_for_event(event);
    let parent_hash = if block_num > 0 {
        block_hash_for_event(&events[block_num - 1])
    } else {
        B256::ZERO
    };
    let tx_hash = extract_tx_hash_from_event(event);

    Ok(json!({
        "number": format!("0x{:x}", block_num),
        "hash": format!("0x{:x}", block_hash),
        "parentHash": format!("0x{:x}", parent_hash),
        "timestamp": format!("0x{:x}", event.created_at_unix_ms / 1000),
        "miner": ZERO_ADDR,
        "difficulty": "0x0",
        "totalDifficulty": "0x0",
        "gasLimit": "0x5208",
        "gasUsed": "0x5208",
        "extraData": "0x",
        "logsBloom": EMPTY_BLOOM,
        "receiptsRoot": ZERO_HASH,
        "stateRoot": ZERO_HASH,
        "transactions": json!([format!("0x{:x}", tx_hash)]),
        "size": "0x0",
        "uncles": [],
        "sha3Uncles": UNCLES_HASH,
        "nonce": "0x0000000000000000",
        "mixHash": ZERO_HASH,
        "baseFeePerGas": "0x0",
    }))
}

async fn get_bucket_events_sorted(
    state: &AppState,
    bucket: &str,
) -> Result<Vec<Event>, EvmRpcErrorBody> {
    let node_result = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.mesh.request_best(NodeRpcRequest::Events),
    )
    .await
    {
        Ok(Ok(r)) => r,
        _ => return Ok(Vec::new()),
    };
    let result = match node_result {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };

    let mut events = match result {
        NodeRpcResponse::Events(resp) => resp.events,
        _ => return Ok(Vec::new()),
    };

    events.retain(|e| e.bucket == bucket);
    events.sort_by(|a, b| {
        a.created_at_unix_ms
            .cmp(&b.created_at_unix_ms)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });

    Ok(events)
}

async fn query_balances(
    state: &AppState,
    _bucket: &str,
) -> Result<Vec<shardd_types::AccountBalance>, EvmRpcErrorBody> {
    let node_result = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        state.mesh.request_best(NodeRpcRequest::Balances),
    )
    .await
    {
        Ok(Ok(r)) => r,
        _ => return Ok(Vec::new()),
    };

    let result = match node_result {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };

    match result {
        NodeRpcResponse::Balances(resp) => Ok(resp.accounts),
        _ => Ok(Vec::new()),
    }
}

async fn get_nonce(state: &AppState, bucket: &str, address: &str) -> Result<u64, EvmRpcErrorBody> {
    let events = get_bucket_events_sorted(state, bucket).await?;
    let nonce = events
        .iter()
        .filter(|e| e.account.to_lowercase() == address.to_lowercase() && e.amount < 0)
        .count() as u64;
    Ok(nonce)
}
