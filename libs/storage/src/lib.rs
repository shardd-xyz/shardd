use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use shardd_types::{Event, NodeMeta, PeersFile};

pub mod postgres;
pub mod memory;

// ── Insert result for dedup + conflict detection ──

#[derive(Debug, Clone, PartialEq)]
pub enum InsertResult {
    Inserted,
    Duplicate,
    Conflict { details: String },
}

// ── Storage backend trait ──

/// Full storage backend: writes + reads.
/// Implementations: PostgresStorage (production), InMemoryStorage (tests).
pub trait StorageBackend: Send + Sync + 'static {
    // ── Writes ──
    fn insert_event(&self, event: &Event) -> impl Future<Output = Result<InsertResult>> + Send;
    fn save_node_meta(&self, meta: &NodeMeta) -> impl Future<Output = Result<()>> + Send;
    fn save_peer(&self, addr: &str) -> impl Future<Output = Result<()>> + Send;
    fn remove_peer(&self, addr: &str) -> impl Future<Output = Result<()>> + Send;
    fn allocate_seq(&self, node_id: &str) -> impl Future<Output = Result<u64>> + Send;

    // ── Reads ──
    fn query_events_range(
        &self, origin: &str, from_seq: u64, to_seq: u64,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn query_all_events_sorted(&self) -> impl Future<Output = Result<Vec<Event>>> + Send;
    fn aggregate_balances(&self) -> impl Future<Output = Result<Vec<(String, String, i64)>>> + Send;
    fn sequences_by_origin(&self) -> impl Future<Output = Result<BTreeMap<String, Vec<u64>>>> + Send;
    fn sequences_from(
        &self, origin: &str, from_seq: u64,
    ) -> impl Future<Output = Result<Vec<u64>>> + Send;
    fn event_count(&self) -> impl Future<Output = Result<usize>> + Send;
    fn checksum_data(&self) -> impl Future<Output = Result<String>> + Send;
    fn origin_account_mapping(
        &self,
    ) -> impl Future<Output = Result<Vec<(String, String, String)>>> + Send;
    fn max_origin_seq(&self, origin: &str) -> impl Future<Output = Result<u64>> + Send;
    fn load_node_meta_by_id(
        &self, node_id: &str,
    ) -> impl Future<Output = Result<Option<NodeMeta>>> + Send;
    fn derive_next_seq(&self, node_id: &str) -> impl Future<Output = Result<u64>> + Send;
    fn load_peers(&self) -> impl Future<Output = Result<Vec<String>>> + Send;

    // ── Legacy write (for JSONL backward compat) ──
    fn append_event(&self, event: &Event) -> impl Future<Output = Result<()>> + Send {
        let _ = event;
        async { Ok(()) }
    }
    fn save_peers_file(&self, pf: &PeersFile) -> impl Future<Output = Result<()>> + Send {
        let _ = pf;
        async { Ok(()) }
    }
}

// ── JSONL file storage (legacy, kept for backward compatibility) ──

#[derive(Debug, Clone)]
pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    fn node_path(&self) -> PathBuf { self.dir.join("node.json") }
    fn peers_path(&self) -> PathBuf { self.dir.join("peers.json") }
    fn events_dir(&self) -> PathBuf { self.dir.join("events") }
    fn origin_log_path(&self, origin: &str) -> PathBuf {
        self.events_dir().join(format!("{origin}.jsonl"))
    }

    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.dir).await.context("create config dir")?;
        fs::create_dir_all(self.events_dir()).await.context("create events dir")?;
        Ok(())
    }

    pub async fn load_node_meta(&self) -> Result<Option<NodeMeta>> {
        let path = self.node_path();
        if !path.exists() { return Ok(None); }
        let data = fs::read_to_string(&path).await?;
        Ok(Some(serde_json::from_str(&data)?))
    }

    pub async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        fs::write(self.node_path(), serde_json::to_string_pretty(meta)?).await?;
        Ok(())
    }

    pub async fn load_peers(&self) -> Result<PeersFile> {
        let path = self.peers_path();
        if !path.exists() { return Ok(PeersFile::default()); }
        Ok(serde_json::from_str(&fs::read_to_string(&path).await?)?)
    }

    pub async fn save_peers(&self, pf: &PeersFile) -> Result<()> {
        fs::write(self.peers_path(), serde_json::to_string_pretty(pf)?).await?;
        Ok(())
    }

    pub async fn load_all_events(&self) -> Result<BTreeMap<String, BTreeMap<u64, Event>>> {
        let mut map: BTreeMap<String, BTreeMap<u64, Event>> = BTreeMap::new();
        let events_dir = self.events_dir();
        if !events_dir.exists() { return Ok(map); }
        let mut entries = fs::read_dir(&events_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let origin = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let data = fs::read_to_string(&path).await?;
            let origin_map = map.entry(origin).or_default();
            for line in data.lines() {
                let line = line.trim();
                if line.is_empty() { continue; }
                let event: Event = serde_json::from_str(line)
                    .with_context(|| format!("parse event line in {path:?}"))?;
                origin_map.insert(event.origin_seq, event);
            }
        }
        Ok(map)
    }

    pub async fn append_event(&self, event: &Event) -> Result<()> {
        let path = self.origin_log_path(&event.origin_node_id);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true).append(true).open(&path).await?;
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}
