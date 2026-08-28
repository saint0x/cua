use anyhow::Context;
use cua_core::InputResult;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum TraceRecord {
    Frame { envelope: serde_json::Value },
    Input { result: InputResult },
    ActionTurn(ActionTurnRecord),
    Marker { name: String, at_wall_ms: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTurnRecord {
    pub schema_version: String,
    pub turn_id: String,
    pub at_wall_ms: i64,
    pub action: serde_json::Value,
    pub result: serde_json::Value,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub before_image_path: Option<String>,
    pub after_image_path: Option<String>,
    pub evidence: serde_json::Value,
    pub session: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct TraceWriter {
    dir: PathBuf,
}

impl TraceWriter {
    pub fn from_dir(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).context("create trace dir")?;
        Ok(Self { dir })
    }

    pub async fn create(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir)
            .await
            .context("create trace dir")?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
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

    pub async fn write_artifact(
        &self,
        relative_path: impl AsRef<Path>,
        bytes: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let path = self.dir.join(relative_path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("create trace artifact dir")?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .context("write trace artifact")?;
        Ok(path)
    }
}
