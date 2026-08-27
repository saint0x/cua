use anyhow::{bail, Context};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

const CHAT_CONTEXT_LIMIT: usize = 8;
const CTX_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct ChatStore {
    profile: String,
    db_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CtxMemory {
    profile: String,
    binary: PathBuf,
    workspace_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AgentContext {
    pub chat: String,
    pub ctx: String,
}

impl ChatStore {
    pub fn new(profile: impl Into<String>) -> anyhow::Result<Self> {
        let profile = profile.into();
        let db_path = profile_root(&profile)?.join("chat.db");
        Ok(Self { profile, db_path })
    }

    pub async fn recent_context(&self) -> anyhow::Result<String> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT role || char(9) || replace(replace(text, char(10), ' '), char(9), ' ') \
             FROM chat_messages ORDER BY id DESC LIMIT {CHAT_CONTEXT_LIMIT};"
        );
        let output = sqlite_stdout(&self.db_path, &sql).await?;
        let mut rows = output
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(role, text)| format!("{}: {}", role, compact_text(text, 220)))
            .collect::<Vec<_>>();
        rows.reverse();
        if rows.is_empty() {
            return Ok("Recent chat: none.".to_string());
        }
        Ok(format!("Recent chat:\n{}", rows.join("\n")))
    }

    pub async fn append_turn(
        &self,
        turn_id: &str,
        user_text: &str,
        assistant_text: &str,
        action: Option<&Value>,
        evidence: Option<&Value>,
        model: &str,
    ) -> anyhow::Result<()> {
        self.ensure_schema().await?;
        let action_json = action.cloned().unwrap_or(Value::Null).to_string();
        let evidence_json = evidence.cloned().unwrap_or(Value::Null).to_string();
        let sql = format!(
            "BEGIN IMMEDIATE; \
             INSERT INTO chat_messages(turn_id, profile, role, text, action_json, evidence_json, model, created_at_ms) \
             VALUES({turn_id}, {profile}, 'user', {user_text}, NULL, NULL, {model}, CAST(strftime('%s','now') AS INTEGER) * 1000); \
             INSERT INTO chat_messages(turn_id, profile, role, text, action_json, evidence_json, model, created_at_ms) \
             VALUES({turn_id}, {profile}, 'assistant', {assistant_text}, {action_json}, {evidence_json}, {model}, CAST(strftime('%s','now') AS INTEGER) * 1000); \
             COMMIT;",
            turn_id = sql_quote(turn_id),
            profile = sql_quote(&self.profile),
            user_text = sql_quote(user_text),
            assistant_text = sql_quote(assistant_text),
            action_json = sql_quote(&action_json),
            evidence_json = sql_quote(&evidence_json),
            model = sql_quote(model),
        );
        sqlite_exec(&self.db_path, &sql).await
    }

    async fn ensure_schema(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        sqlite_exec(
            &self.db_path,
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; \
             CREATE TABLE IF NOT EXISTS chat_messages( \
             id INTEGER PRIMARY KEY AUTOINCREMENT, \
             turn_id TEXT NOT NULL, \
             profile TEXT NOT NULL, \
             role TEXT NOT NULL, \
             text TEXT NOT NULL, \
             action_json TEXT, \
             evidence_json TEXT, \
             model TEXT NOT NULL, \
             created_at_ms INTEGER NOT NULL); \
             CREATE INDEX IF NOT EXISTS idx_chat_messages_profile_id ON chat_messages(profile, id);",
        )
        .await
    }
}

impl CtxMemory {
    pub fn new(profile: impl Into<String>) -> anyhow::Result<Self> {
        let profile = profile.into();
        Ok(Self {
            workspace_root: profile_root(&profile)?.join("ctx"),
            binary: ctx_binary(),
            profile,
        })
    }

    pub async fn frame(&self, request: &str, chat_context: &str) -> anyhow::Result<String> {
        let request = format!(
            "{request}\n\nCurrent cua chat context:\n{}",
            compact_text(chat_context, 2_000)
        );
        let output = self
            .run([
                "frame".to_string(),
                self.session_id(),
                self.profile.clone(),
                request,
            ])
            .await?;
        Ok(format_ctx_frame(&output)?)
    }

    pub async fn remember_chat_turn(
        &self,
        user_text: &str,
        assistant_text: &str,
    ) -> anyhow::Result<()> {
        let content = format!(
            "User said: {} Assistant replied: {}",
            compact_text(user_text, 360),
            compact_text(assistant_text, 360)
        );
        let _ = self
            .run([
                "remember".to_string(),
                "session".to_string(),
                "chat".to_string(),
                "30".to_string(),
                "80".to_string(),
                content,
            ])
            .await?;
        Ok(())
    }

    async fn run<I>(&self, args: I) -> anyhow::Result<String>
    where
        I: IntoIterator<Item = String>,
    {
        if !self.binary.is_file() {
            bail!("ctx binary is required at {}", self.binary.display());
        }
        tokio::fs::create_dir_all(&self.workspace_root).await?;
        let output = tokio::time::timeout(
            Duration::from_millis(CTX_TIMEOUT_MS),
            Command::new(&self.binary)
                .env("CUA_CTX_WORKSPACE_ROOT", &self.workspace_root)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("ctx command timed out")?
        .with_context(|| format!("launch ctx {}", self.binary.display()))?;
        if !output.status.success() {
            bail!(
                "ctx exited {:?}: {}",
                output.status.code(),
                compact_command_output(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn session_id(&self) -> String {
        format!("cua-{}", self.profile)
    }
}

pub async fn load_agent_context(profile: &str, request: &str) -> anyhow::Result<AgentContext> {
    let chat = load_chat_context(profile).await?;
    load_agent_context_with_chat(profile, request, chat).await
}

pub async fn load_chat_context(profile: &str) -> anyhow::Result<String> {
    ChatStore::new(profile)?.recent_context().await
}

pub async fn load_agent_context_with_chat(
    profile: &str,
    request: &str,
    chat: String,
) -> anyhow::Result<AgentContext> {
    let ctx = CtxMemory::new(profile)?.frame(request, &chat).await?;
    Ok(AgentContext { chat, ctx })
}

fn format_ctx_frame(raw: &str) -> anyhow::Result<String> {
    let value: Value = serde_json::from_str(raw).context("decode ctx frame")?;
    let registers = value.get("registers").cloned().unwrap_or(Value::Null);
    let memory = value
        .get("selected_memory")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let protocol = value.get("protocol").cloned().unwrap_or(Value::Null);
    Ok(format!(
        "Context:\nregisters={}\nselected_memory={}\nprotocol={}",
        compact_text(&registers.to_string(), 1_200),
        compact_text(&memory.to_string(), 2_000),
        compact_text(&protocol.to_string(), 1_200)
    ))
}

fn sqlite_binary() -> PathBuf {
    PathBuf::from("/usr/bin/sqlite3")
}

async fn sqlite_exec(db_path: &PathBuf, sql: &str) -> anyhow::Result<()> {
    let output = Command::new(sqlite_binary())
        .arg("-batch")
        .arg(db_path)
        .arg(sql)
        .output()
        .await
        .context("launch sqlite3")?;
    if !output.status.success() {
        bail!("sqlite3 failed: {}", compact_command_output(&output.stderr));
    }
    Ok(())
}

async fn sqlite_stdout(db_path: &PathBuf, sql: &str) -> anyhow::Result<String> {
    let output = Command::new(sqlite_binary())
        .arg("-batch")
        .arg("-noheader")
        .arg(db_path)
        .arg(sql)
        .output()
        .await
        .context("launch sqlite3")?;
    if !output.status.success() {
        bail!("sqlite3 failed: {}", compact_command_output(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sql_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn profile_root(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?)
        .join(".cua")
        .join("profiles")
        .join(profile))
}

fn ctx_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CUA_CTX_BIN") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("ctx");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("vendor")
        .join("ctx")
        .join("ctx")
}

fn compact_command_output(bytes: &[u8]) -> String {
    compact_text(&String::from_utf8_lossy(bytes), 1_200)
}

fn compact_text(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut truncated = compact
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ctx_frame_as_bounded_context() {
        let raw = r#"{"registers":{"goal":"open safari"},"selected_memory":[{"content":"Use Aegis for browser work"}],"protocol":{"task_mode":"engineering"}}"#;
        let frame = format_ctx_frame(raw).unwrap();

        assert!(frame.contains("Context:"));
        assert!(frame.contains("open safari"));
        assert!(frame.contains("selected_memory"));
    }

    #[test]
    fn sql_quote_escapes_single_quotes() {
        assert_eq!(sql_quote("can't"), "'can''t'");
    }

    #[tokio::test]
    async fn chat_store_appends_and_loads_recent_context() {
        let profile = format!("chat-test-{}", uuid::Uuid::new_v4());
        let store = ChatStore::new(profile.clone()).unwrap();

        store
            .append_turn(
                "turn-1",
                "open safari",
                "Opening Safari.",
                None,
                None,
                "test-model",
            )
            .await
            .unwrap();
        let context = store.recent_context().await.unwrap();

        assert!(context.contains("user: open safari"));
        assert!(context.contains("assistant: Opening Safari."));
    }

    #[tokio::test]
    async fn ctx_is_required_without_fallback() {
        let ctx = CtxMemory {
            profile: "test".to_string(),
            binary: PathBuf::from(format!("/tmp/cua-missing-ctx-{}", uuid::Uuid::new_v4())),
            workspace_root: std::env::temp_dir()
                .join(format!("cua-ctx-missing-{}", uuid::Uuid::new_v4())),
        };
        let error = ctx.frame("hello", "Recent chat: none.").await.unwrap_err();

        assert!(format!("{error:#}").contains("ctx binary is required"));
    }
}
