use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn bundled_cua_binary() -> anyhow::Result<PathBuf> {
    let current_exe = std::env::current_exe().context("resolve current executable")?;
    sibling_cua_binary_from(&current_exe)
}

fn sibling_cua_binary_from(current_exe: &Path) -> anyhow::Result<PathBuf> {
    let Some(parent) = current_exe.parent() else {
        bail!(
            "current executable has no parent: {}",
            current_exe.display()
        );
    };
    let candidate = parent.join("cua");
    if candidate.is_file() {
        return Ok(candidate);
    }
    bail!(
        "bundled cua binary not found next to {}",
        current_exe.display()
    )
}

pub fn spawn_profile_daemon(profile: &str) -> anyhow::Result<()> {
    let binary = bundled_cua_binary()?;
    Command::new(&binary)
        .args(["--profile", profile, "serve", "--addr", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    Ok(())
}

pub async fn wait_until_ready<F, Fut>(timeout: Duration, mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let result = check().await;
        if result.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return result.context("timed out waiting for cua daemon socket");
        }
        tokio::time::sleep(Duration::from_millis(35)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_binary_resolves_next_to_voice_binary() {
        let path =
            sibling_cua_binary_from(Path::new("/Users/example/App.app/Contents/MacOS/cua-voice"))
                .unwrap_err()
                .to_string();

        assert!(path.contains("bundled cua binary not found"));
    }

    #[tokio::test]
    async fn wait_until_ready_retries_until_success() {
        let mut attempts = 0;

        wait_until_ready(Duration::from_secs(1), || {
            attempts += 1;
            async move {
                if attempts < 3 {
                    bail!("not yet");
                }
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(attempts, 3);
    }
}
