use anyhow::Context;
use cua_core::{FrameEnvelope, InputResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceRecord {
    Frame { envelope: FrameEnvelope },
    Input { result: InputResult },
    Marker { name: String, at_wall_ms: i64 },
}

#[derive(Debug, Clone)]
pub struct TraceWriter {
    dir: PathBuf,
}

impl TraceWriter {
    pub async fn create(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir)
            .await
            .context("create trace dir")?;
        Ok(Self { dir })
    }

    pub async fn append(&self, record: &TraceRecord) -> anyhow::Result<()> {
        let path = self.dir.join("trajectory.jsonl");
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .context("open trace trajectory")?;
        let line = serde_json::to_vec(record).context("serialize trace record")?;
        file.write_all(&line).await?;
        file.write_all(b"\n").await?;
        Ok(())
    }
}
