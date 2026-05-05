use std::collections::HashMap;

use alloy_consensus::{TxLegacy, transaction::RlpEcdsaDecodableTx};
use alloy_primitives::{B256, TxKind, U256, keccak256};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shardd_types::{CreateEventRequest, Event, NodeRpcRequest, NodeRpcResponse};
use uuid::Uuid;

use crate::{
    AppState, AuthorizedBucket, GatewayMachineAction, bearer_token, forbidden,
    gateway_unavailable_response, unauthorized,
};

// ── JSON-RPC wire types ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct EvmRpcRequest {
    #[allow(dead_code)]
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

const EMPTY_BLOOM: &str = "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

const ZERO_HASH: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
const UNCLES_HASH: &str = "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";

// ── Public handler ───────────────────────────────────────────────────

pub async fn evm_rpc_handler(
    State(state): State<AppState>,
    Path(bucket): Path<String>,
    Query(query_params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let req: EvmRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return make_error(null_value(), -32700, &format!("parse error: {e}"));
        }
    };

    // Auth: extract API key from query param or Authorization header
    let api_key = query_params
        .get("api_key")
        .map(|s| s.as_str())
        .or_else(|| bearer_token(&headers));

    let needs_write = method_needs_write(&req.method);
    let resolved_bucket = if bucket == "*" {
        return make_error(
            req.id,
            -32602,
            "wildcard bucket not supported; specify bucket in URL",
        );
    } else {
        bucket
    };

    // Authenticate if API key is provided (required for writes, optional for reads)
    let user_id = if let Some(key) = api_key {
        let action = if needs_write {
            GatewayMachineAction::Write
        } else {
            GatewayMachineAction::Read
        };
        match authorize_evm_call(&state, key, action, &resolved_bucket).await {
            Ok(auth) => Some(auth.user_id),
            Err(resp) => return resp,
        }
    } else if needs_write {
        return make_error(req.id, -32000, "api_key required for write operations");
    } else {
        None
    };

    // Dispatch
    let result = match req.method.as_str() {
        "eth_chainId" | "net_version" => eth_chain_id(&resolved_bucket),
        "eth_accounts" => Ok(json!([])),
        "eth_getBalance" => eth_get_balance(&state, &resolved_bucket, &req.params).await,
        "eth_getTransactionCount" => {
            eth_get_transaction_count(&state, &resolved_bucket, &req.params).await
        }
        "eth_sendRawTransaction" => {
            let uid = match user_id {
                Some(u) => u,
                None => return make_error(req.id, -32000, "auth required"),
            };
            eth_send_raw_transaction(&state, &resolved_bucket, &uid, &req.params).await
        }
        "eth_gasPrice" => Ok(json!("0x0")),
        "eth_estimateGas" => Ok(json!("0x5208")),
        "eth_blockNumber" => eth_block_number(&state, &resolved_bucket).await,
        "eth_getBlockByNumber" => {
            eth_get_block_by_number(&state, &resolved_bucket, &req.params).await
        }
        "eth_getBlockByHash" => eth_get_block_by_hash(&state, &resolved_bucket, &req.params).await,
        "eth_getTransactionByHash" => {
            eth_get_transaction_by_hash(&state, &resolved_bucket, &req.params).await
        }
        "eth_getTransactionReceipt" => {
            eth_get_transaction_receipt(&state, &resolved_bucket, &req.params).await
        }
        "eth_call" => Err(evm_rpc_err(-32601, "smart contracts not supported")),
        "eth_getLogs" => Ok(json!([])),
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

// ── Auth helpers ─────────────────────────────────────────────────────

async fn authorize_evm_call(
    state: &AppState,
    api_key: &str,
    action: GatewayMachineAction,
    bucket: &str,
) -> Result<AuthorizedBucket, Response> {
    let Some(auth) = &state.auth else {
        return Err(gateway_unavailable_response(
            "dashboard auth is not configured".to_string(),
        ));
    };
    let decision = auth
        .authorize(api_key, action, bucket)
        .await
        .map_err(|e| gateway_unavailable_response(e.to_string()))?;

    if decision.allowed {
        let Some(user_id) = decision.user_id else {
            return Err(gateway_unavailable_response(
                "dashboard introspection returned no user id".to_string(),
            ));
        };
        Ok(AuthorizedBucket { user_id })
    } else {
        let reason = decision
            .denial_reason
            .unwrap_or_else(|| "access_denied".to_string());
        if decision.valid {
            Err(forbidden(&reason))
        } else {
            Err(unauthorized(&reason))
        }
    }
}

fn method_needs_write(method: &str) -> bool {
    matches!(method, "eth_sendRawTransaction" | "eth_sendTransaction")
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
    let chain_id = u64::from_be_bytes([hash[0], hash[1], hash[2], hash[3], 0, 0, 0, 0]);
    Ok(json!(format!("0x{:x}", chain_id)))
}

fn chain_id_for_bucket(bucket: &str) -> u64 {
    let hash = keccak256(bucket.as_bytes());
    u64::from_be_bytes([hash[0], hash[1], hash[2], hash[3], 0, 0, 0, 0])
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
    user_id: &Uuid,
    params: &Option<Value>,
) -> Result<Value, EvmRpcErrorBody> {
    let raw_hex = extract_param_str(params, 0)?;
    let raw_bytes = hex::decode(raw_hex.trim_start_matches("0x"))
        .map_err(|_| evm_rpc_err(-32602, "invalid hex"))?;

    // 1. Decode signed legacy transaction via RLP
    let tx = TxLegacy::rlp_decode_signed(&mut &raw_bytes[..])
        .map_err(|e| evm_rpc_err(-32602, &format!("invalid transaction: {e}")))?;

    // 2. Recover signer
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

    // 4. Validate chain_id
    let expected_chain = chain_id_for_bucket(bucket);
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

    // 5a. Check EVM status (enabled, paused, whitelist) via dashboard
    check_evm_write_allowed(state, user_id, bucket, &from_str).await?;

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
    if events.is_empty() {
        return Ok(json!(null));
    }

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
    let node_result = state
        .mesh
        .request_best(NodeRpcRequest::Events)
        .await
        .map_err(|e| evm_rpc_err(-32000, &format!("mesh error: {e}")))?;

    let result = node_result.map_err(|e| evm_rpc_err(-32000, &format!("node error: {e:?}")))?;

    let mut events = match result {
        NodeRpcResponse::Events(resp) => resp.events,
        _ => return Err(evm_rpc_err(-32000, "unexpected response type")),
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
    let node_result = state
        .mesh
        .request_best(NodeRpcRequest::Balances)
        .await
        .map_err(|e| evm_rpc_err(-32000, &format!("mesh error: {e}")))?;

    let result = node_result.map_err(|e| evm_rpc_err(-32000, &format!("node error: {e:?}")))?;

    match result {
        NodeRpcResponse::Balances(resp) => Ok(resp.accounts),
        _ => Err(evm_rpc_err(-32000, "unexpected response type")),
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

async fn check_evm_write_allowed(
    state: &AppState,
    user_id: &Uuid,
    bucket: &str,
    from_address: &str,
) -> Result<(), EvmRpcErrorBody> {
    let Some(auth) = &state.auth else {
        return Err(evm_rpc_err(-32000, "auth not configured"));
    };

    let resp = auth
        .http
        .post(format!("{}/api/machine/evm/check", auth.base_url))
        .header("x-machine-auth-secret", &auth.shared_secret)
        .json(&json!({
            "user_id": user_id.to_string(),
            "bucket_name": bucket,
            "address": from_address.to_lowercase(),
            "action": "write",
        }))
        .send()
        .await
        .map_err(|e| evm_rpc_err(-32000, &format!("evm check failed: {e}")))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| evm_rpc_err(-32000, &format!("evm check response error: {e}")))?;

    let allowed = body
        .get("allowed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !allowed {
        let reason = body
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("access denied");
        return Err(evm_rpc_err(-32000, &format!("access denied: {reason}")));
    }

    Ok(())
}
