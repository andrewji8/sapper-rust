//! The agent-engine call: `claude -p --output-format json` (or a wrapper via
//! SAPPER_BIN), with key-pool rotation and quota-aware retries. Mirrors the
//! Python `run_claude` / `_run_engine_once` / `_rotate_key` behavior.

use crate::error::{Result, SapperError};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Load the SAPPER_KEY_POOL JSON array into in-memory entries.
fn load_key_pool() -> Vec<HashMap<String, String>> {
    let raw = std::env::var("SAPPER_KEY_POOL")
        .unwrap_or_default()
        .trim()
        .to_string();
    if raw.is_empty() {
        return vec![];
    }
    let pool: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("[sapper] SAPPER_KEY_POOL is not valid JSON — ignoring");
            return vec![];
        }
    };
    let arr = match pool.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    let mut out = vec![];
    for item in arr {
        if !item.is_object() {
            continue;
        }
        let token = item["token"].as_str().unwrap_or("").trim().to_string();
        if token.is_empty() {
            continue;
        }
        let mut e = HashMap::new();
        e.insert(
            "base_url".into(),
            item["base_url"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
                .unwrap_or_default(),
        );
        e.insert("token".into(), token.clone());
        e.insert(
            "model".into(),
            item["model"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| std::env::var("SAPPER_MODEL").ok())
                .unwrap_or_else(|| "sonnet".into()),
        );
        out.push(e);
    }
    out
}

fn is_quota_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    QUOTA_PATTERNS.iter().any(|p| lower.contains(p))
}

const QUOTA_PATTERNS: [&str; 16] = [
    "insufficient",
    "balance",
    "quota",
    "exceeded",
    "arrearage",
    "billing",
    "payment",
    "401",
    "unauthorized",
    "欠费",
    "余额",
    "invalid api",
    "invalid api key",
    "invalid apikey",
    "invalid key",
    "invalid auth",
];

/// Advance to the next pool entry by mutating the process environment.
/// Returns false when the pool is empty (nothing to rotate).
fn rotate_key() -> bool {
    let pool = load_key_pool();
    if pool.is_empty() {
        return false;
    }
    let current = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
    let start = pool
        .iter()
        .position(|e| e.get("token").map(|t| t.as_str()) == Some(current.as_str()));
    let idx = match start {
        Some(s) => (s + 1) % pool.len(),
        None => 1 % pool.len(),
    };
    let nxt = &pool[idx];
    if start.is_some() && nxt.get("token").map(|t| t.as_str()) == Some(current.as_str()) {
        return true; // single-entry pool: keep the key, caller retries
    }
    std::env::set_var(
        "ANTHROPIC_BASE_URL",
        nxt.get("base_url").cloned().unwrap_or_default(),
    );
    std::env::set_var(
        "ANTHROPIC_AUTH_TOKEN",
        nxt.get("token").cloned().unwrap_or_default(),
    );
    std::env::set_var(
        "SAPPER_MODEL",
        nxt.get("model").cloned().unwrap_or_default(),
    );
    true
}

fn run_engine_once(
    argv: &[String],
    prompt: &str,
    timeout: u64,
    cwd: &std::path::Path,
    engine: &str,
) -> Result<Value> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            SapperError::new(format!(
                "`{engine}` CLI not found on PATH; install it or fix SAPPER_BIN."
            ))
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
        drop(stdin);
    }
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let mut stdout_thread = stdout_pipe.map(|mut p| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            buf
        })
    });
    let mut stderr_thread = stderr_pipe.map(|mut p| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            buf
        })
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > Duration::from_secs(timeout) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SapperError::new(format!(
                        "{engine} dispatch timed out after {timeout}s"
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(SapperError::new(format!("engine process error: {e}")));
            }
        }
    };
    let stdout = stdout_thread
        .take()
        .map(|h| String::from_utf8_lossy(&h.join().unwrap_or_default()).to_string())
        .unwrap_or_default();
    let stderr = stderr_thread
        .take()
        .map(|h| String::from_utf8_lossy(&h.join().unwrap_or_default()).to_string())
        .unwrap_or_default();

    if !status.success() {
        return Err(SapperError::new(format!(
            "{engine} exited {}: {}",
            status.code().unwrap_or(-1),
            stderr.trim().chars().take(500).collect::<String>()
        )));
    }
    let envelope: Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(_) => {
            return Err(SapperError::new(format!(
                "{engine} did not return JSON. stdout[:300]={:?}",
                stdout.chars().take(300).collect::<String>()
            )));
        }
    };
    if !envelope.is_object() {
        return Err(SapperError::new(format!(
            "{engine} returned a JSON value instead of an object envelope"
        )));
    }
    if envelope
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let result = envelope
            .get("result")
            .map(|v| v.to_string())
            .unwrap_or_default();
        return Err(SapperError::new(format!(
            "{engine} reported error: {}",
            result.chars().take(300).collect::<String>()
        )));
    }
    if !envelope
        .get("result")
        .map(|v| v.is_string())
        .unwrap_or(false)
    {
        return Err(SapperError::new(
            "response envelope has a non-string result",
        ));
    }
    Ok(envelope)
}

/// Call the agent CLI and return the parsed JSON result envelope.
pub fn run_engine(
    prompt: &str,
    model: &str,
    timeout: u64,
    skip_permissions: bool,
    append_system: Option<&str>,
    cwd: &std::path::Path,
) -> Result<Value> {
    if timeout < 1 {
        return Err(SapperError::new("claude timeout must be at least 1 second"));
    }
    if model.trim().is_empty() {
        return Err(SapperError::new("claude model cannot be empty"));
    }
    let prefix: Vec<String> = std::env::var("SAPPER_BIN")
        .unwrap_or_else(|_| "claude".into())
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let mut argv: Vec<String> = prefix.clone();
    argv.extend([
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--model".into(),
    ]);
    argv.push(model.to_string());
    if skip_permissions {
        argv.push("--dangerously-skip-permissions".into());
    }
    if let Some(s) = append_system {
        argv.push("--append-system-prompt".into());
        argv.push(s.to_string());
    }
    let engine = prefix.first().cloned().unwrap_or_default();
    let retry = std::env::var("SAPPER_RETRY_MODEL")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let attempts = 1 + retry.max(0);
    let mut last_error: Option<SapperError> = None;
    for attempt in 1..=attempts {
        match run_engine_once(&argv, prompt, timeout, cwd, &engine) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_error = Some(e.clone());
                let msg = e.0.clone();
                if msg.contains("CLI not found") {
                    return Err(e);
                }
                if is_quota_error(&msg) && rotate_key() {
                    eprintln!(
                        "[sapper] quota/auth error (attempt {attempt}/{attempts}); key rotated — retrying immediately"
                    );
                    continue;
                }
                if attempt < attempts {
                    let exp = 10i64 * 2i64.pow((attempt - 1).min(10) as u32);
                    let backoff = 120i64.min(exp);
                    eprintln!(
                        "[sapper] engine call failed (attempt {attempt}/{attempts}): {e}; retrying in {backoff}s"
                    );
                    std::thread::sleep(Duration::from_secs(backoff as u64));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| SapperError::new("engine call failed")))
}

/// Per-process worker identity so two concurrent executors don't look
/// identical to the claim check.
pub fn worker_id(model: &str) -> String {
    format!("{model}-{}", std::process::id())
}
