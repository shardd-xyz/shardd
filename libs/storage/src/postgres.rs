//! PostgreSQL storage backend per protocol.md v1.8 §6.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::BTreeMap;

use shardd_types::{EpochKey, Event, EventType, NodeMeta, NodeRegistryEntry, NodeStatus};

use crate::{BucketAllocatorRow, EventsFilter, InsertResult, StorageBackend};

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
    bucket: String,
    origin_node_id: String,
    origin_epoch: i32,
    origin_seq: i64,
    created_at_unix_ms: i64,
    r#type: String,
    account: String,
    amount: i64,
    note: Option<String>,
    idempotency_nonce: String,
    void_ref: Option<String>,
    hold_amount: i64,
    hold_expires_at_unix_ms: i64,
}

impl From<EventRow> for Event {
    fn from(r: EventRow) -> Self {
        let event_type = match r.r#type.as_str() {
            "void" => EventType::Void,
            "hold_release" => EventType::HoldRelease,
            "reservation_create" => EventType::ReservationCreate,
            "bucket_delete" => EventType::BucketDelete,
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

const EVENT_COLS: &str = "event_id, bucket, origin_node_id, origin_epoch, origin_seq, created_at_unix_ms, type, account, amount, note, idempotency_nonce, void_ref, hold_amount, hold_expires_at_unix_ms";

impl StorageBackend for PostgresStorage {
    async fn insert_event(&self, event: &Event) -> Result<InsertResult> {
        let result = sqlx::query(
            "INSERT INTO events (event_id, bucket, origin_node_id, origin_epoch, origin_seq, created_at_unix_ms, type, account, amount, note, idempotency_nonce, void_ref, hold_amount, hold_expires_at_unix_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             ON CONFLICT (bucket, origin_node_id, origin_epoch, origin_seq) DO NOTHING",
        )
        .bind(&event.event_id)
        .bind(&event.bucket)
        .bind(&event.origin_node_id)
        .bind(event.origin_epoch as i32)
        .bind(event.origin_seq as i64)
        .bind(event.created_at_unix_ms as i64)
        .bind(event.r#type.to_string())
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
                    "SELECT event_id FROM events WHERE bucket = $1 AND origin_node_id = $2 AND origin_epoch = $3 AND origin_seq = $4",
                )
                .bind(&event.bucket)
                .bind(&event.origin_node_id)
                .bind(event.origin_epoch as i32)
                .bind(event.origin_seq as i64)
                .fetch_optional(&self.pool)
                .await?;

                match existing {
                    Some((eid,)) if eid == event.event_id => Ok(InsertResult::Duplicate),
                    Some((eid,)) => Ok(InsertResult::Conflict {
                        details: format!(
                            "({}, {}, {}, {}) has event_id {} but received {}",
                            event.bucket,
                            event.origin_node_id,
                            event.origin_epoch,
                            event.origin_seq,
                            eid,
                            event.event_id
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
        // Postgres param limit is 65535. With 14 cols/event, chunk at 4096
        // events (= 57344 params). Ordering events by (bucket, origin,
        // epoch, seq) reduces deadlocks with concurrent writers touching
        // overlapping row ranges.
        const CHUNK: usize = 4096;
        let mut sorted: Vec<&Event> = events.iter().collect();
        sorted.sort_by(|a, b| {
            a.bucket
                .cmp(&b.bucket)
                .then(a.origin_node_id.cmp(&b.origin_node_id))
                .then(a.origin_epoch.cmp(&b.origin_epoch))
                .then(a.origin_seq.cmp(&b.origin_seq))
        });
        let mut total: usize = 0;
        for batch in sorted.chunks(CHUNK) {
            let mut sql = format!("INSERT INTO events ({EVENT_COLS}) VALUES ");
            for (i, _) in batch.iter().enumerate() {
                let b = i * 14;
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!(
                    "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                    b + 1,
                    b + 2,
                    b + 3,
                    b + 4,
                    b + 5,
                    b + 6,
                    b + 7,
                    b + 8,
                    b + 9,
                    b + 10,
                    b + 11,
                    b + 12,
                    b + 13,
                    b + 14
                ));
            }
            sql.push_str(
                " ON CONFLICT (bucket, origin_node_id, origin_epoch, origin_seq) DO NOTHING",
            );

            let mut query = sqlx::query(&sql);
            for event in batch {
                query = query
                    .bind(&event.event_id)
                    .bind(&event.bucket)
                    .bind(&event.origin_node_id)
                    .bind(event.origin_epoch as i32)
                    .bind(event.origin_seq as i64)
                    .bind(event.created_at_unix_ms as i64)
                    .bind(event.r#type.to_string())
                    .bind(&event.account)
                    .bind(event.amount)
                    .bind(&event.note)
                    .bind(&event.idempotency_nonce)
                    .bind(&event.void_ref)
                    .bind(event.hold_amount as i64)
                    .bind(event.hold_expires_at_unix_ms as i64);
            }
            let result = query.execute(&self.pool).await?;
            total += result.rows_affected() as usize;
        }
        Ok(total)
    }

    async fn query_events_range(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE bucket = $1 AND origin_node_id = $2 AND origin_epoch = $3 AND origin_seq >= $4 AND origin_seq <= $5 ORDER BY origin_seq"
        ))
        .bind(bucket)
        .bind(origin)
        .bind(epoch as i32)
        .bind(from_seq as i64)
        .bind(to_seq as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn query_events_by_bucket(&self, bucket: &str) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE bucket = $1 ORDER BY origin_node_id, origin_epoch, origin_seq"
        ))
        .bind(bucket)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn delete_bucket_cascade(&self, bucket: &str) -> Result<()> {
        use shardd_types::META_BUCKET;
        if bucket == META_BUCKET {
            anyhow::bail!("refusing to delete the meta log itself");
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM events WHERE bucket = $1")
            .bind(bucket)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM rolling_digests WHERE bucket = $1")
            .bind(bucket)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM bucket_seq_allocator WHERE bucket = $1")
            .bind(bucket)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn query_all_events_sorted(&self) -> Result<Vec<Event>> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events ORDER BY created_at_unix_ms, bucket, origin_node_id, origin_epoch, origin_seq"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn event_count(&self) -> Result<usize> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as usize)
    }

    async fn query_events_filtered(
        &self,
        filter: &EventsFilter,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64)> {
        // Build WHERE clause + bind list dynamically. Kept parameterised
        // (no string interpolation of user input) to keep SQLi off the
        // table.
        let mut predicates: Vec<String> = Vec::new();
        let mut idx: usize = 0;
        if filter.bucket.is_some() {
            idx += 1;
            predicates.push(format!("bucket = ${idx}"));
        }
        if filter.bucket_prefix.is_some() {
            idx += 1;
            predicates.push(format!("starts_with(bucket, ${idx})"));
        }
        if filter.account.is_some() {
            idx += 1;
            predicates.push(format!("account = ${idx}"));
        }
        if filter.origin.is_some() {
            idx += 1;
            predicates.push(format!("origin_node_id = ${idx}"));
        }
        if filter.event_type.is_some() {
            idx += 1;
            predicates.push(format!("type = ${idx}"));
        }
        if filter.since_unix_ms.is_some() {
            idx += 1;
            predicates.push(format!("created_at_unix_ms >= ${idx}"));
        }
        if filter.until_unix_ms.is_some() {
            idx += 1;
            predicates.push(format!("created_at_unix_ms <= ${idx}"));
        }
        if filter.search.is_some() {
            idx += 1;
            predicates.push(format!("(note ILIKE ${idx} OR event_id ILIKE ${idx})"));
        }
        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", predicates.join(" AND "))
        };

        // COUNT(*). sqlx's QueryAs borrows the query string, which
        // makes a shared helper-closure fight the borrow checker — just
        // inline the identical bind loop twice.
        let count_sql = format!("SELECT COUNT(*)::BIGINT FROM events{where_clause}");
        let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql);
        if let Some(bucket) = &filter.bucket {
            count_q = count_q.bind(bucket);
        }
        if let Some(prefix) = &filter.bucket_prefix {
            count_q = count_q.bind(prefix);
        }
        if let Some(account) = &filter.account {
            count_q = count_q.bind(account);
        }
        if let Some(origin) = &filter.origin {
            count_q = count_q.bind(origin);
        }
        if let Some(t) = &filter.event_type {
            count_q = count_q.bind(t);
        }
        if let Some(since) = filter.since_unix_ms {
            count_q = count_q.bind(since as i64);
        }
        if let Some(until) = filter.until_unix_ms {
            count_q = count_q.bind(until as i64);
        }
        if let Some(search) = &filter.search {
            count_q = count_q.bind(format!("%{search}%"));
        }
        let (total,): (i64,) = count_q.fetch_one(&self.pool).await?;

        // Page
        let limit_idx = idx + 1;
        let offset_idx = idx + 2;
        let page_sql = format!(
            "SELECT {EVENT_COLS} FROM events{where_clause} \
             ORDER BY created_at_unix_ms DESC, event_id DESC \
             LIMIT ${limit_idx} OFFSET ${offset_idx}"
        );
        let mut page_q = sqlx::query_as::<_, EventRow>(&page_sql);
        if let Some(bucket) = &filter.bucket {
            page_q = page_q.bind(bucket);
        }
        if let Some(prefix) = &filter.bucket_prefix {
            page_q = page_q.bind(prefix);
        }
        if let Some(account) = &filter.account {
            page_q = page_q.bind(account);
        }
        if let Some(origin) = &filter.origin {
            page_q = page_q.bind(origin);
        }
        if let Some(t) = &filter.event_type {
            page_q = page_q.bind(t);
        }
        if let Some(since) = filter.since_unix_ms {
            page_q = page_q.bind(since as i64);
        }
        if let Some(until) = filter.until_unix_ms {
            page_q = page_q.bind(until as i64);
        }
        if let Some(search) = &filter.search {
            page_q = page_q.bind(format!("%{search}%"));
        }
        let rows = page_q
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await?;
        let events: Vec<Event> = rows.into_iter().map(Into::into).collect();
        Ok((events, total as u64))
    }

    async fn aggregate_balances(&self) -> Result<Vec<(String, String, i64)>> {
        Ok(sqlx::query_as(
            "SELECT bucket, account, SUM(amount)::BIGINT FROM events GROUP BY bucket, account",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn sequences_by_origin_epoch(&self) -> Result<BTreeMap<EpochKey, Vec<u64>>> {
        let rows = sqlx::query_as::<_, (String, String, i32, i64)>(
            "SELECT bucket, origin_node_id, origin_epoch, origin_seq FROM events ORDER BY bucket, origin_node_id, origin_epoch, origin_seq"
        ).fetch_all(&self.pool).await?;

        let mut map: BTreeMap<EpochKey, Vec<u64>> = BTreeMap::new();
        for (bucket, origin, epoch, seq) in rows {
            map.entry((bucket, origin, epoch as u32))
                .or_default()
                .push(seq as u64);
        }
        Ok(map)
    }

    async fn origin_account_epoch_mapping(&self) -> Result<Vec<(String, String, u32, String)>> {
        let rows = sqlx::query_as::<_, (String, String, i32, String)>(
            "SELECT DISTINCT bucket, origin_node_id, origin_epoch, account FROM events",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(b, o, e, a)| (b, o, e as u32, a))
            .collect())
    }

    async fn find_by_idempotency_key(
        &self,
        nonce: &str,
        bucket: &str,
        account: &str,
        amount: i64,
    ) -> Result<Vec<Event>> {
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
            "SELECT void_ref FROM events WHERE type = 'hold_release' AND void_ref IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(r,)| r).collect())
    }

    async fn checksum_data(&self) -> Result<String> {
        let rows = sqlx::query_as::<_, EventRow>(&format!(
            "SELECT {EVENT_COLS} FROM events ORDER BY bucket, origin_node_id, origin_epoch, origin_seq"
        ))
        .fetch_all(&self.pool)
        .await?;

        let mut hasher = Sha256::new();
        for (i, row) in rows.iter().enumerate() {
            let event: Event = EventRow {
                event_id: row.event_id.clone(),
                bucket: row.bucket.clone(),
                origin_node_id: row.origin_node_id.clone(),
                origin_epoch: row.origin_epoch,
                origin_seq: row.origin_seq,
                created_at_unix_ms: row.created_at_unix_ms,
                r#type: row.r#type.clone(),
                account: row.account.clone(),
                amount: row.amount,
                note: row.note.clone(),
                idempotency_nonce: row.idempotency_nonce.clone(),
                void_ref: row.void_ref.clone(),
                hold_amount: row.hold_amount,
                hold_expires_at_unix_ms: row.hold_expires_at_unix_ms,
            }
            .into();
            if i > 0 {
                hasher.update(b"\n");
            }
            hasher.update(event.canonical().as_bytes());
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn load_node_meta(&self, node_id: &str) -> Result<Option<NodeMeta>> {
        let row = sqlx::query_as::<_, (String, String, i32)>(
            "SELECT node_id, host, port FROM node_meta WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, host, port)| NodeMeta {
            node_id: id,
            host,
            port: port as u16,
        }))
    }

    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        sqlx::query(
            "INSERT INTO node_meta (node_id, host, port)
             VALUES ($1, $2, $3)
             ON CONFLICT (node_id) DO UPDATE SET host = $2, port = $3",
        )
        .bind(&meta.node_id)
        .bind(&meta.host)
        .bind(meta.port as i32)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn load_bucket_allocators(&self, node_id: &str) -> Result<Vec<BucketAllocatorRow>> {
        let rows = sqlx::query_as::<_, (String, String, i32, i64, bool)>(
            "SELECT bucket, node_id, current_epoch, next_seq, needs_bump
             FROM bucket_seq_allocator WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(bucket, node_id, epoch, seq, needs_bump)| BucketAllocatorRow {
                    bucket,
                    node_id,
                    current_epoch: epoch as u32,
                    next_seq: seq as u64,
                    needs_bump,
                },
            )
            .collect())
    }

    async fn mark_bucket_allocators_pending(&self, node_id: &str) -> Result<usize> {
        let result =
            sqlx::query("UPDATE bucket_seq_allocator SET needs_bump = TRUE WHERE node_id = $1")
                .bind(node_id)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() as usize)
    }

    async fn bump_bucket_epoch(&self, bucket: &str, node_id: &str) -> Result<u32> {
        // If a row exists with needs_bump=TRUE, bump and clear the flag.
        // If a row exists with needs_bump=FALSE, return current_epoch as-is
        // (no-op bump: we already bumped earlier this process lifetime).
        // If no row exists, insert a fresh one at epoch=1, seq=1.
        //
        // Done as two statements in a single transaction. UPSERT alone
        // can't express "bump only when flag is set"; the explicit
        // SELECT/UPDATE/INSERT keeps the semantics crystal clear.
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, (i32, bool)>(
            "SELECT current_epoch, needs_bump FROM bucket_seq_allocator
             WHERE bucket = $1 AND node_id = $2
             FOR UPDATE",
        )
        .bind(bucket)
        .bind(node_id)
        .fetch_optional(&mut *tx)
        .await?;

        let epoch = match row {
            Some((epoch, needs_bump)) if needs_bump => {
                let (new_epoch,): (i32,) = sqlx::query_as(
                    "UPDATE bucket_seq_allocator
                     SET current_epoch = current_epoch + 1, next_seq = 1, needs_bump = FALSE
                     WHERE bucket = $1 AND node_id = $2
                     RETURNING current_epoch",
                )
                .bind(bucket)
                .bind(node_id)
                .fetch_one(&mut *tx)
                .await
                .context("bump_bucket_epoch: row disappeared mid-txn")?;
                let _ = epoch;
                new_epoch as u32
            }
            Some((epoch, _)) => epoch as u32,
            None => {
                sqlx::query(
                    "INSERT INTO bucket_seq_allocator (bucket, node_id, current_epoch, next_seq, needs_bump)
                     VALUES ($1, $2, 1, 1, FALSE)",
                )
                .bind(bucket)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
                1
            }
        };
        tx.commit().await?;
        Ok(epoch)
    }

    async fn persist_bucket_next_seq(
        &self,
        bucket: &str,
        node_id: &str,
        next_seq: u64,
    ) -> Result<()> {
        // Only moves next_seq forward. A racing flush with a higher value
        // must not be clobbered.
        sqlx::query(
            "UPDATE bucket_seq_allocator
             SET next_seq = GREATEST(next_seq, $3)
             WHERE bucket = $1 AND node_id = $2",
        )
        .bind(bucket)
        .bind(node_id)
        .bind(next_seq as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn derive_next_seq(&self, bucket: &str, node_id: &str, epoch: u32) -> Result<u64> {
        let (max,): (Option<i64>,) = sqlx::query_as(
            "SELECT MAX(origin_seq) FROM events
             WHERE bucket = $1 AND origin_node_id = $2 AND origin_epoch = $3",
        )
        .bind(bucket)
        .bind(node_id)
        .bind(epoch as i32)
        .fetch_one(&self.pool)
        .await?;
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
               addr = CASE
                        WHEN EXCLUDED.last_seen_at_unix_ms > node_registry.last_seen_at_unix_ms
                          THEN CASE WHEN EXCLUDED.addr <> '' THEN EXCLUDED.addr ELSE node_registry.addr END
                        WHEN node_registry.addr = ''
                          THEN EXCLUDED.addr
                        ELSE node_registry.addr
                      END,
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
        Ok(rows
            .into_iter()
            .map(|(id, addr, first, last, status)| {
                let st = match status.as_str() {
                    "suspect" => NodeStatus::Suspect,
                    "unreachable" => NodeStatus::Unreachable,
                    "decommissioned" => NodeStatus::Decommissioned,
                    _ => NodeStatus::Active,
                };
                NodeRegistryEntry {
                    node_id: id,
                    addr,
                    first_seen_at_unix_ms: first as u64,
                    last_seen_at_unix_ms: last as u64,
                    status: st,
                }
            })
            .collect())
    }

    async fn decommission_node(&self, node_id: &str) -> Result<()> {
        sqlx::query("UPDATE node_registry SET status = 'decommissioned' WHERE node_id = $1")
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn refresh_balance_summary(&self) -> Result<()> {
        sqlx::query("REFRESH MATERIALIZED VIEW CONCURRENTLY balance_summary")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn read_balance_summary(&self) -> Result<Vec<(String, String, i64)>> {
        match sqlx::query_as::<_, (String, String, i64)>(
            "SELECT bucket, account, balance FROM balance_summary",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => Ok(rows),
            Err(_) => self.aggregate_balances().await,
        }
    }

    async fn load_digests(&self) -> Result<BTreeMap<EpochKey, (u64, [u8; 32])>> {
        let rows = sqlx::query_as::<_, (String, String, i32, i64, Vec<u8>)>(
            "SELECT bucket, origin_node_id, origin_epoch, head, digest FROM rolling_digests",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut map = BTreeMap::new();
        for (bucket, origin, epoch, head, digest_bytes) in rows {
            if digest_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&digest_bytes);
                map.insert((bucket, origin, epoch as u32), (head as u64, arr));
            }
        }
        Ok(map)
    }

    async fn save_digest(
        &self,
        bucket: &str,
        origin: &str,
        epoch: u32,
        head: u64,
        digest: &[u8; 32],
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO rolling_digests (bucket, origin_node_id, origin_epoch, head, digest)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (bucket, origin_node_id, origin_epoch) DO UPDATE SET
               head = EXCLUDED.head, digest = EXCLUDED.digest",
        )
        .bind(bucket)
        .bind(origin)
        .bind(epoch as i32)
        .bind(head as i64)
        .bind(&digest[..])
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn run_migrations(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("run migrations")?;
        Ok(())
    }
}
