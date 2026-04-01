//! PostgreSQL storage backend for production use.
//! Each node has its own Postgres instance.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use sqlx::PgPool;

use shardd_types::{Event, NodeMeta};

use crate::{InsertResult, StorageBackend};

#[derive(Debug, Clone)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn run_migrations(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("run migrations")?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl StorageBackend for PostgresStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let result = sqlx::query(
            "INSERT INTO events (event_id, origin_node_id, origin_seq, created_at_unix_ms,
                                 bucket, account, amount, note)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (origin_node_id, origin_seq) DO NOTHING",
        )
        .bind(&event.event_id)
        .bind(&event.origin_node_id)
        .bind(event.origin_seq as i64)
        .bind(event.created_at_unix_ms as i64)
        .bind(&event.bucket)
        .bind(&event.account)
        .bind(event.amount)
        .bind(&event.note)
        .execute(&self.pool)
        .await;

        match result {
            Ok(r) if r.rows_affected() == 1 => Ok(InsertResult::Inserted),
            Ok(_) => {
                // ON CONFLICT fired — check for payload mismatch
                let existing = sqlx::query_as::<_, (String,)>(
                    "SELECT event_id FROM events WHERE origin_node_id = $1 AND origin_seq = $2",
                )
                .bind(&event.origin_node_id)
                .bind(event.origin_seq as i64)
                .fetch_optional(&self.pool)
                .await?;

                match existing {
                    Some((existing_eid,)) if existing_eid == event.event_id => {
                        Ok(InsertResult::Duplicate)
                    }
                    Some((existing_eid,)) => Ok(InsertResult::Conflict {
                        details: format!(
                            "({}, {}) has event_id {} in DB but received {}",
                            event.origin_node_id, event.origin_seq,
                            existing_eid, event.event_id
                        ),
                    }),
                    None => Ok(InsertResult::Duplicate), // race condition, treat as dup
                }
            }
            Err(e) => {
                // Could be event_id PK collision
                let err_str = e.to_string();
                if err_str.contains("events_pkey") || err_str.contains("event_id") {
                    Ok(InsertResult::Conflict {
                        details: format!("event_id {} PK collision: {}", event.event_id, err_str),
                    })
                } else {
                    Err(e.into())
                }
            }
        }
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        sqlx::query(
            "INSERT INTO node_meta (node_id, host, port, next_seq)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (node_id) DO UPDATE SET host = $2, port = $3, next_seq = $4",
        )
        .bind(&meta.node_id)
        .bind(&meta.host)
        .bind(meta.port as i32)
        .bind(meta.next_seq as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn save_peer(&self, addr: &str) -> Result<()> {
        sqlx::query("INSERT INTO peers (addr) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(addr)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn remove_peer(&self, addr: &str) -> Result<()> {
        sqlx::query("DELETE FROM peers WHERE addr = $1")
            .bind(addr)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn allocate_seq(&self, node_id: &str) -> Result<u64> {
        let row = sqlx::query_as::<_, (i64,)>(
            "UPDATE node_meta SET next_seq = next_seq + 1 WHERE node_id = $1 RETURNING next_seq - 1",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await
        .context("allocate_seq")?;
        Ok(row.0 as u64)
    }

    async fn query_events_range(
        &self, origin: &str, from_seq: u64, to_seq: u64,
    ) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT event_id, origin_node_id, origin_seq, created_at_unix_ms,
                    bucket, account, amount, note
             FROM events
             WHERE origin_node_id = $1 AND origin_seq >= $2 AND origin_seq <= $3
             ORDER BY origin_seq",
        )
        .bind(origin)
        .bind(from_seq as i64)
        .bind(to_seq as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(
            "SELECT event_id, origin_node_id, origin_seq, created_at_unix_ms,
                    bucket, account, amount, note
             FROM events
             ORDER BY created_at_unix_ms, origin_node_id, origin_seq",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into()).collect())
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT bucket, account, SUM(amount)::BIGINT FROM events GROUP BY bucket, account",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn sequences_by_origin(&self) -> Result<BTreeMap<String, Vec<u64>>> {
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT origin_node_id, origin_seq FROM events ORDER BY origin_node_id, origin_seq",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut map: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for (origin, seq) in rows {
            map.entry(origin).or_default().push(seq as u64);
        }
        Ok(map)
    }

    async fn sequences_from(&self, origin: &str, from_seq: u64) -> Result<Vec<u64>> {
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT origin_seq FROM events WHERE origin_node_id = $1 AND origin_seq >= $2 ORDER BY origin_seq",
        )
        .bind(origin)
        .bind(from_seq as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(s,)| s as u64).collect())
    }

    async fn event_count(&self) -> Result<usize> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    async fn checksum_data(&self) -> Result<String> {
        // Canonical format: {origin}:{seq}:{event_id}:{bucket}:{account}:{amount}
        let rows = sqlx::query_as::<_, (String, i64, String, String, String, i64)>(
            "SELECT origin_node_id, origin_seq, event_id, bucket, account, amount
             FROM events ORDER BY origin_node_id, origin_seq",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut hasher = Sha256::new();
        let mut first = true;
        for (origin, seq, eid, bucket, account, amount) in &rows {
            if !first { hasher.update(b"\n"); }
            first = false;
            hasher.update(format!("{origin}:{seq}:{eid}:{bucket}:{account}:{amount}"));
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn origin_account_mapping(&self) -> Result<Vec<(String, String, String)>> {
        Ok(sqlx::query_as(
            "SELECT DISTINCT origin_node_id, bucket, account FROM events",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn max_origin_seq(&self, origin: &str) -> Result<u64> {
        let row: (Option<i64>,) = sqlx::query_as(
            "SELECT MAX(origin_seq) FROM events WHERE origin_node_id = $1",
        )
        .bind(origin)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0.unwrap_or(0) as u64)
    }

    async fn load_node_meta_by_id(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        let row = sqlx::query_as::<_, (String, String, i32, i64)>(
            "SELECT node_id, host, port, next_seq FROM node_meta WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, host, port, next_seq)| NodeMeta {
            node_id: id,
            host,
            port: port as u16,
            next_seq: next_seq as u64,
        }))
    }

    async fn derive_next_seq(&self, node_id: &str) -> Result<u64> {
        let (max,): (Option<i64>,) = sqlx::query_as(
            "SELECT MAX(origin_seq) FROM events WHERE origin_node_id = $1",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((max.unwrap_or(0) + 1) as u64)
    }

    async fn load_peers(&self) -> Result<Vec<String>> {
        let rows = sqlx::query_as::<_, (String,)>("SELECT addr FROM peers ORDER BY addr")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(a,)| a).collect())
    }
}

// ── Additional methods (not on trait — Postgres-specific) ────────────

impl PostgresStorage {
    /// Bulk insert events with ON CONFLICT DO NOTHING. Returns count of newly inserted.
    pub async fn insert_events_bulk(&self, events: &[Event]) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }

        // Build multi-row INSERT
        let mut sql = String::from(
            "INSERT INTO events (event_id, origin_node_id, origin_seq, created_at_unix_ms, bucket, account, amount, note) VALUES "
        );
        let mut params: Vec<String> = Vec::new();
        for (i, _) in events.iter().enumerate() {
            let base = i * 8;
            if i > 0 { sql.push_str(", "); }
            sql.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                base + 1, base + 2, base + 3, base + 4,
                base + 5, base + 6, base + 7, base + 8
            ));
        }
        sql.push_str(" ON CONFLICT (origin_node_id, origin_seq) DO NOTHING");

        let mut query = sqlx::query(&sql);
        for event in events {
            query = query
                .bind(&event.event_id)
                .bind(&event.origin_node_id)
                .bind(event.origin_seq as i64)
                .bind(event.created_at_unix_ms as i64)
                .bind(&event.bucket)
                .bind(&event.account)
                .bind(event.amount)
                .bind(&event.note);
        }

        let result = query.execute(&self.pool).await?;
        Ok(result.rows_affected() as usize)
    }

    /// Read balance_summary materialized view. Returns [(bucket, account, balance)].
    pub async fn read_balance_summary(&self) -> Result<Vec<(String, String, i64)>> {
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT bucket, account, balance FROM balance_summary",
        )
        .fetch_all(&self.pool)
        .await;

        match rows {
            Ok(r) => Ok(r),
            Err(_) => {
                // Fallback if materialized view doesn't exist
                self.aggregate_balances().await
            }
        }
    }

    /// Refresh the materialized view (called by BatchWriter after flush).
    pub async fn refresh_balance_summary(&self) -> Result<()> {
        sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY balance_summary")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// Helper struct for mapping DB rows to Event
#[derive(sqlx::FromRow)]
struct EventRow {
    event_id: String,
    origin_node_id: String,
    origin_seq: i64,
    created_at_unix_ms: i64,
    bucket: String,
    account: String,
    amount: i64,
    note: Option<String>,
}

impl From<EventRow> for Event {
    fn from(r: EventRow) -> Self {
        Event {
            event_id: r.event_id,
            origin_node_id: r.origin_node_id,
            origin_seq: r.origin_seq as u64,
            created_at_unix_ms: r.created_at_unix_ms as u64,
            bucket: r.bucket,
            account: r.account,
            amount: r.amount,
            note: r.note,
        }
    }
}
