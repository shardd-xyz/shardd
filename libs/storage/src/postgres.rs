//! PostgreSQL storage backend per protocol.md v1.7 §6.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use sqlx::PgPool;

use shardd_types::{Event, EventType, NodeMeta, NodeRegistryEntry, NodeStatus};

use crate::{InsertResult, StorageBackend};

#[derive(Debug, Clone)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

// ── Row mapping helper ───────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct EventRow {
    event_id: String,
    origin_node_id: String,
    origin_epoch: i32,
    origin_seq: i64,
    created_at_unix_ms: i64,
    r#type: String,
    bucket: String,
    account: String,
    amount: i64,
    note: Option<String>,
    idempotency_nonce: Option<String>,
    void_ref: Option<String>,
    hold_amount: i64,
    hold_expires_at_unix_ms: i64,
}

impl From<EventRow> for Event {
    fn from(r: EventRow) -> Self {
        let event_type = match r.r#type.as_str() {
            "void" => EventType::Void,
            "hold_release" => EventType::HoldRelease,
            _ => EventType::Standard,
        };
        Event {
            event_id: r.event_id,
            origin_node_id: r.origin_node_id,
            origin_epoch: r.origin_epoch as u32,
            origin_seq: r.origin_seq as u64,
            created_at_unix_ms: r.created_at_unix_ms as u64,
            r#type: event_type,
            bucket: r.bucket,
            account: r.account,
            amount: r.amount,
            note: r.note,
            idempotency_nonce: r.idempotency_nonce,
            void_ref: r.void_ref,
            hold_amount: r.hold_amount as u64,
            hold_expires_at_unix_ms: r.hold_expires_at_unix_ms as u64,
        }
    }
}

const EVENT_COLS: &str = "event_id, origin_node_id, origin_epoch, origin_seq, created_at_unix_ms, type, bucket, account, amount, note, idempotency_nonce, void_ref, hold_amount, hold_expires_at_unix_ms";

impl StorageBackend for PostgresStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let result = sqlx::query(
            "INSERT INTO events (event_id, origin_node_id, origin_epoch, origin_seq, created_at_unix_ms, type, bucket, account, amount, note, idempotency_nonce, void_ref, hold_amount, hold_expires_at_unix_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             ON CONFLICT (origin_node_id, origin_epoch, origin_seq) DO NOTHING",
        )
        .bind(&event.event_id)
        .bind(&event.origin_node_id)
        .bind(event.origin_epoch as i32)
        .bind(event.origin_seq as i64)
        .bind(event.created_at_unix_ms as i64)
        .bind(event.r#type.to_string())
        .bind(&event.bucket)
        .bind(&event.account)
        .bind(event.amount)
        .bind(&event.note)
        .bind(&event.idempotency_nonce)
        .bind(&event.void_ref)
        .bind(event.hold_amount as i64)
        .bind(event.hold_expires_at_unix_ms as i64)
        .execute(&self.pool)
        .await;

        match result {
            Ok(r) if r.rows_affected() == 1 => Ok(InsertResult::Inserted),
            Ok(_) => {
                // Conflict — check if same event_id (dup) or different (corruption)
                let existing = sqlx::query_as::<_, (String,)>(
                    "SELECT event_id FROM events WHERE origin_node_id = $1 AND origin_epoch = $2 AND origin_seq = $3",
                )
                .bind(&event.origin_node_id)
                .bind(event.origin_epoch as i32)
                .bind(event.origin_seq as i64)
                .fetch_optional(&self.pool)
                .await?;

                match existing {
                    Some((eid,)) if eid == event.event_id => Ok(InsertResult::Duplicate),
                    Some((eid,)) => Ok(InsertResult::Conflict {
                        details: format!(
                            "({}, {}, {}) has event_id {} but received {}",
                            event.origin_node_id, event.origin_epoch, event.origin_seq, eid, event.event_id
                        ),
                    }),
                    None => Ok(InsertResult::Duplicate),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn insert_events_bulk(&self, events: &[Event]) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }

        // Build multi-row INSERT
        let mut sql = format!("INSERT INTO events ({EVENT_COLS}) VALUES ");
        for (i, _) in events.iter().enumerate() {
            let b = i * 14;
            if i > 0 { sql.push_str(", "); }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                b+1, b+2, b+3, b+4, b+5, b+6, b+7, b+8, b+9, b+10, b+11, b+12, b+13, b+14
            ));
        }
        sql.push_str(" ON CONFLICT (origin_node_id, origin_epoch, origin_seq) DO NOTHING");

        let mut query = sqlx::query(&sql);
        for event in events {
            query = query
                .bind(&event.event_id)
                .bind(&event.origin_node_id)
                .bind(event.origin_epoch as i32)
                .bind(event.origin_seq as i64)
                .bind(event.created_at_unix_ms as i64)
                .bind(event.r#type.to_string())
                .bind(&event.bucket)
                .bind(&event.account)
                .bind(event.amount)
                .bind(&event.note)
                .bind(&event.idempotency_nonce)
                .bind(&event.void_ref)
                .bind(event.hold_amount as i64)
                .bind(event.hold_expires_at_unix_ms as i64);
        }

        let result = query.execute(&self.pool).await?;
        Ok(result.rows_affected() as usize)
    }

    async fn query_events_range(&self, origin: &str, epoch: u32, from_seq: u64, to_seq: u64) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE origin_node_id = $1 AND origin_epoch = $2 AND origin_seq >= $3 AND origin_seq <= $4 ORDER BY origin_seq"
        ))
        .bind(origin)
        .bind(epoch as i32)
        .bind(from_seq as i64)
        .bind(to_seq as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events ORDER BY created_at_unix_ms, origin_node_id, origin_epoch, origin_seq"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn event_count(&self) -> Result<usize> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
            .fetch_one(&self.pool).await?;
        Ok(count as usize)
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        Ok(sqlx::query_as("SELECT bucket, account, SUM(amount)::BIGINT FROM events GROUP BY bucket, account")
            .fetch_all(&self.pool).await?)
    }

    async fn sequences_by_origin_epoch(&self) -> Result<BTreeMap<(String, u32), Vec<u64>>> {
        let rows = sqlx::query_as::<_, (String, i32, i64)>(
            "SELECT origin_node_id, origin_epoch, origin_seq FROM events ORDER BY origin_node_id, origin_epoch, origin_seq"
        ).fetch_all(&self.pool).await?;

        let mut map: BTreeMap<(String, u32), Vec<u64>> = BTreeMap::new();
        for (origin, epoch, seq) in rows {
            map.entry((origin, epoch as u32)).or_default().push(seq as u64);
        }
        Ok(map)
    }

    async fn origin_account_epoch_mapping(&self) -> Result<Vec<(String, u32, String, String)>> {
        let rows = sqlx::query_as::<_, (String, i32, String, String)>(
            "SELECT DISTINCT origin_node_id, origin_epoch, bucket, account FROM events"
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(o, e, b, a)| (o, e as u32, b, a)).collect())
    }

    async fn find_by_idempotency_key(&self, nonce: &str, bucket: &str, account: &str, amount: i64) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE idempotency_nonce = $1 AND bucket = $2 AND account = $3 AND amount = $4"
        ))
        .bind(nonce).bind(bucket).bind(account).bind(amount)
        .fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn active_holds(&self, now_ms: u64) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE type = 'standard' AND amount < 0 AND hold_amount > 0 AND hold_expires_at_unix_ms > $1"
        ))
        .bind(now_ms as i64)
        .fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn released_hold_refs(&self) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT void_ref FROM events WHERE type = 'hold_release' AND void_ref IS NOT NULL"
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(r,)| r).collect())
    }

    async fn checksum_data(&self) -> Result<String> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events ORDER BY origin_node_id, origin_epoch, origin_seq"
        )).fetch_all(&self.pool).await?;

        let mut hasher = Sha256::new();
        for (i, row) in rows.iter().enumerate() {
            let event: Event = EventRow {
                event_id: row.event_id.clone(),
                origin_node_id: row.origin_node_id.clone(),
                origin_epoch: row.origin_epoch,
                origin_seq: row.origin_seq,
                created_at_unix_ms: row.created_at_unix_ms,
                r#type: row.r#type.clone(),
                bucket: row.bucket.clone(),
                account: row.account.clone(),
                amount: row.amount,
                note: row.note.clone(),
                idempotency_nonce: row.idempotency_nonce.clone(),
                void_ref: row.void_ref.clone(),
                hold_amount: row.hold_amount,
                hold_expires_at_unix_ms: row.hold_expires_at_unix_ms,
            }.into();
            if i > 0 { hasher.update(b"\n"); }
            hasher.update(event.canonical().as_bytes());
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn load_node_meta(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        let row = sqlx::query_as::<_, (String, String, i32, i32, i64)>(
            "SELECT node_id, host, port, current_epoch, next_seq FROM node_meta WHERE node_id = $1"
        ).bind(node_id).fetch_optional(&self.pool).await?;
        Ok(row.map(|(id, host, port, epoch, seq)| NodeMeta {
            node_id: id, host, port: port as u16, current_epoch: epoch as u32, next_seq: seq as u64,
        }))
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        sqlx::query(
            "INSERT INTO node_meta (node_id, host, port, current_epoch, next_seq)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (node_id) DO UPDATE SET host = $2, port = $3, current_epoch = $4, next_seq = $5"
        )
        .bind(&meta.node_id).bind(&meta.host).bind(meta.port as i32)
        .bind(meta.current_epoch as i32).bind(meta.next_seq as i64)
        .execute(&self.pool).await?;
        Ok(())
    }

    async fn increment_epoch(&self, node_id: &str) -> Result<u32> {
        // Atomic: UPDATE ... RETURNING — single statement, crash-safe (§13.4)
        let (epoch,): (i32,) = sqlx::query_as(
            "UPDATE node_meta SET current_epoch = current_epoch + 1, next_seq = 1 WHERE node_id = $1 RETURNING current_epoch"
        ).bind(node_id).fetch_one(&self.pool).await
            .context("increment_epoch: node_meta row must exist")?;
        Ok(epoch as u32)
    }

    async fn derive_next_seq(&self, node_id: &str, epoch: u32) -> Result<u64> {
        let (max,): (Option<i64>,) = sqlx::query_as(
            "SELECT MAX(origin_seq) FROM events WHERE origin_node_id = $1 AND origin_epoch = $2"
        ).bind(node_id).bind(epoch as i32).fetch_one(&self.pool).await?;
        Ok((max.unwrap_or(0) + 1) as u64)
    }

    async fn upsert_registry_entry(&self, entry: &NodeRegistryEntry) -> Result<()> {
        // CRDT merge per §14.3: decommissioned is monotonic tombstone
        sqlx::query(
            "INSERT INTO node_registry (node_id, addr, first_seen_at_unix_ms, last_seen_at_unix_ms, status)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (node_id) DO UPDATE SET
               first_seen_at_unix_ms = LEAST(node_registry.first_seen_at_unix_ms, EXCLUDED.first_seen_at_unix_ms),
               last_seen_at_unix_ms = GREATEST(node_registry.last_seen_at_unix_ms, EXCLUDED.last_seen_at_unix_ms),
               addr = CASE WHEN EXCLUDED.last_seen_at_unix_ms > node_registry.last_seen_at_unix_ms
                           THEN EXCLUDED.addr ELSE node_registry.addr END,
               status = CASE WHEN node_registry.status = 'decommissioned' OR EXCLUDED.status = 'decommissioned'
                             THEN 'decommissioned'
                             WHEN EXCLUDED.last_seen_at_unix_ms > node_registry.last_seen_at_unix_ms
                             THEN EXCLUDED.status ELSE node_registry.status END"
        )
        .bind(&entry.node_id).bind(&entry.addr)
        .bind(entry.first_seen_at_unix_ms as i64).bind(entry.last_seen_at_unix_ms as i64)
        .bind(entry.status.to_string())
        .execute(&self.pool).await?;
        Ok(())
    }

    async fn load_registry(&self) -> Result<Vec<NodeRegistryEntry>> {
        let rows = sqlx::query_as::<_, (String, String, i64, i64, String)>(
            "SELECT node_id, addr, first_seen_at_unix_ms, last_seen_at_unix_ms, status FROM node_registry ORDER BY node_id"
        ).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id, addr, first, last, status)| {
            let st = match status.as_str() {
                "suspect" => NodeStatus::Suspect,
                "unreachable" => NodeStatus::Unreachable,
                "decommissioned" => NodeStatus::Decommissioned,
                _ => NodeStatus::Active,
            };
            NodeRegistryEntry { node_id: id, addr, first_seen_at_unix_ms: first as u64, last_seen_at_unix_ms: last as u64, status: st }
        }).collect())
    }

    async fn decommission_node(&self, node_id: &str) -> Result<()> {
        sqlx::query("UPDATE node_registry SET status = 'decommissioned' WHERE node_id = $1")
            .bind(node_id).execute(&self.pool).await?;
        Ok(())
    }

    async fn refresh_balance_summary(&self) -> Result<()> {
        sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY balance_summary")
            .execute(&self.pool).await?;
        Ok(())
    }

    async fn read_balance_summary(&self) -> Result<Vec<(String, String, i64)>> {
        match sqlx::query_as::<_, (String, String, i64)>(
            "SELECT bucket, account, balance FROM balance_summary"
        ).fetch_all(&self.pool).await {
            Ok(rows) => Ok(rows),
            Err(_) => self.aggregate_balances().await,
        }
    }

    async fn run_migrations(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("run migrations")?;
        Ok(())
    }
}
