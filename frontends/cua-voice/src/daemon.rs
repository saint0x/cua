use anyhow::{bail, Context};
use cua_core::{profile_socket_path, profile_token_path};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS: u64 = 30_000;
const MIN_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS: u64 = 500;
const MAX_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS: u64 = 120_000;

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
    if profile_daemon_is_alive(profile) {
        return Ok(());
    }
    let binary = bundled_cua_binary()?;
    let token = load_or_create_profile_token(profile)?;
    Command::new(&binary)
        .args([
            "--profile",
            profile,
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--hud-mode",
            "headless",
        ])
        .env("CUA_HTTP_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    Ok(())
}

fn load_or_create_profile_token(profile: &str) -> anyhow::Result<String> {
    if http_token_override_allowed() {
        if let Ok(token) = std::env::var("CUA_HTTP_TOKEN") {
            if !token.trim().is_empty() {
                return Ok(token);
            }
        }
    }
    let path = profile_token_path(profile)?;
    if let Ok(token) = std::fs::read_to_string(&path) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = format!("cua-{}", uuid::Uuid::new_v4());
    std::fs::write(path, format!("{token}\n"))?;
    Ok(token)
}

fn http_token_override_allowed() -> bool {
    cfg!(test)
        || std::env::var("CUA_DEV_HTTP_TOKEN_OVERRIDE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

pub fn profile_daemon_is_alive(profile: &str) -> bool {
    let Ok(socket) = profile_socket_path(profile) else {
        return false;
    };
    UnixStream::connect(socket).is_ok()
}

#[cfg(test)]
fn profile_daemon_is_alive_under(home: &Path, profile: &str) -> bool {
    let legacy_socket = home
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("daemon.sock");
    UnixStream::connect(legacy_socket).is_ok()
}

pub fn embedded_daemon_startup_timeout() -> Duration {
    std::env::var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|timeout| *timeout > 0)
        .map(|timeout| {
            timeout.clamp(
                MIN_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS,
                MAX_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS,
            )
        })
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS))
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

    #[test]
    fn profile_daemon_alive_returns_false_for_missing_socket() {
        let profile = format!("missing-{}", uuid::Uuid::new_v4());
        assert!(!profile_daemon_is_alive(&profile));
    }

    #[test]
    fn profile_daemon_alive_detects_bound_socket() {
        let dir = PathBuf::from(format!("/tmp/cua-{}", uuid::Uuid::new_v4().simple()));
        let socket = dir
            .join(".cua")
            .join("profiles")
            .join("v")
            .join("daemon.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        assert!(profile_daemon_is_alive_under(&dir, "v"));

        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_daemon_alive_honors_cua_home() {
        let old_cua_home = std::env::var_os("CUA_HOME");
        let dir = PathBuf::from(format!("/tmp/cua-home-{}", uuid::Uuid::new_v4().simple()));
        let socket = dir.join("profiles").join("v").join("daemon.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::env::set_var("CUA_HOME", &dir);

        assert!(profile_daemon_is_alive("v"));

        drop(listener);
        if let Some(old_cua_home) = old_cua_home {
            std::env::set_var("CUA_HOME", old_cua_home);
        } else {
            std::env::remove_var("CUA_HOME");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_daemon_startup_timeout_is_bounded_and_configurable() {
        let old_timeout = std::env::var_os("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS");

        std::env::remove_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS");
        assert_eq!(embedded_daemon_startup_timeout(), Duration::from_secs(30));

        std::env::set_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS", "750");
        assert_eq!(
            embedded_daemon_startup_timeout(),
            Duration::from_millis(750)
        );

        std::env::set_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS", "1");
        assert_eq!(
            embedded_daemon_startup_timeout(),
            Duration::from_millis(500)
        );

        std::env::set_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS", "300000");
        assert_eq!(embedded_daemon_startup_timeout(), Duration::from_secs(120));

        std::env::set_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS", "nope");
        assert_eq!(embedded_daemon_startup_timeout(), Duration::from_secs(30));

        if let Some(old_timeout) = old_timeout {
            std::env::set_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS", old_timeout);
        } else {
            std::env::remove_var("CUA_EMBEDDED_DAEMON_STARTUP_TIMEOUT_MS");
        }
    }
}
