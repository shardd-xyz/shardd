use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use shardd_types::{Event, NodeMeta, PeersFile};

/// Async storage backend abstraction for persist operations.
pub trait StorageBackend: Send + Sync + 'static {
    fn append_event(&self, event: &Event) -> impl Future<Output = Result<()>> + Send;
    fn save_node_meta(&self, meta: &NodeMeta) -> impl Future<Output = Result<()>> + Send;
    fn save_peers(&self, pf: &PeersFile) -> impl Future<Output = Result<()>> + Send;
}

impl StorageBackend for Storage {
    async fn append_event(&self, event: &Event) -> Result<()> {
        Storage::append_event(self, event).await
    }
    async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        Storage::save_node_meta(self, meta).await
    }
    async fn save_peers(&self, pf: &PeersFile) -> Result<()> {
        Storage::save_peers(self, pf).await
    }
}

/// No-op storage for testing. Discards all writes.
#[derive(Debug, Clone, Default)]
pub struct NullStorage;

impl StorageBackend for NullStorage {
    async fn append_event(&self, _: &Event) -> Result<()> {
        Ok(())
    }
    async fn save_node_meta(&self, _: &NodeMeta) -> Result<()> {
        Ok(())
    }
    async fn save_peers(&self, _: &PeersFile) -> Result<()> {
        Ok(())
    }
}

/// Handles all file-based persistence under a config directory.
#[derive(Debug, Clone)]
pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    fn node_path(&self) -> PathBuf {
        self.dir.join("node.json")
    }

    fn peers_path(&self) -> PathBuf {
        self.dir.join("peers.json")
    }

    fn events_dir(&self) -> PathBuf {
        self.dir.join("events")
    }

    fn origin_log_path(&self, origin: &str) -> PathBuf {
        self.events_dir().join(format!("{origin}.jsonl"))
    }

    /// Create directories if missing.
    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .await
            .context("create config dir")?;
        fs::create_dir_all(self.events_dir())
            .await
            .context("create events dir")?;
        Ok(())
    }

    // ── Node meta ──

    pub async fn load_node_meta(&self) -> Result<Option<NodeMeta>> {
        let path = self.node_path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).await?;
        let meta: NodeMeta = serde_json::from_str(&data)?;
        Ok(Some(meta))
    }

    pub async fn save_node_meta(&self, meta: &NodeMeta) -> Result<()> {
        let data = serde_json::to_string_pretty(meta)?;
        fs::write(self.node_path(), data).await?;
        Ok(())
    }

    // ── Peers ──

    pub async fn load_peers(&self) -> Result<PeersFile> {
        let path = self.peers_path();
        if !path.exists() {
            return Ok(PeersFile::default());
        }
        let data = fs::read_to_string(&path).await?;
        let pf: PeersFile = serde_json::from_str(&data)?;
        Ok(pf)
    }

    pub async fn save_peers(&self, pf: &PeersFile) -> Result<()> {
        let data = serde_json::to_string_pretty(pf)?;
        fs::write(self.peers_path(), data).await?;
        Ok(())
    }

    // ── Events ──

    /// Load all events from all per-origin JSONL files.
    pub async fn load_all_events(&self) -> Result<BTreeMap<String, BTreeMap<u64, Event>>> {
        let mut map: BTreeMap<String, BTreeMap<u64, Event>> = BTreeMap::new();
        let events_dir = self.events_dir();
        if !events_dir.exists() {
            return Ok(map);
        }
        let mut entries = fs::read_dir(&events_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("jsonl") {
                continue;
            }
            let origin = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let data = fs::read_to_string(&path).await?;
            let origin_map = map.entry(origin).or_default();
            for line in data.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let event: Event = serde_json::from_str(line)
                    .with_context(|| format!("parse event line in {path:?}"))?;
                origin_map.insert(event.origin_seq, event);
            }
        }
        Ok(map)
    }

    /// Append a single event to the appropriate origin JSONL file.
    pub async fn append_event(&self, event: &Event) -> Result<()> {
        let path = self.origin_log_path(&event.origin_node_id);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}
