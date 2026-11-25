//! TSecBench (Tencent Cloud) benchmark-platform client for sapper.
//! Rust port of `arena.py` using `ureq` (2.x) instead of stdlib `urllib`.
//!
//!    GET  /openapi/v1/challenges
//!    POST /openapi/v1/challenges/start?unique_code=CODE
//!    GET  /openapi/v1/challenges/hint?unique_code=CODE
//!    POST /openapi/v1/challenges/submit {unique_code, flag}
//!    POST /openapi/v1/challenges/close?unique_code=CODE
//!
//! Errors come back as {"code","message","detail"} with an HTTP status;
//! the common ones (invalid_state "max active", duplicate submit) are
//! surfaced with their message so the caller can react.

use crate::error::{Result, SapperError};
use serde_json::{json, Value};
use std::path::Path;

const ARENA_FILE: &str = "arena.json";
const DEFAULT_TIMEOUT: u64 = 30;
const TOKEN_HEADER: &str = "BENCHMARK_TOKEN";
const CHALLENGES_PATH: &str = "/openapi/v1/challenges";

fn code_valid(s: &str) -> bool {
    let re = regex::Regex::new(r"^[A-Za-z0-9_.:-]+$").unwrap();
    re.is_match(s)
}

pub struct ArenaClient {
    pub base_url: String,
    token: String,
    timeout: u64,
}

impl ArenaClient {
    pub fn new(base_url: String, token: String, timeout: u64) -> Result<Self> {
        if base_url.trim().is_empty() {
            return Err(SapperError::new("arena base_url is empty"));
        }
        if token.trim().is_empty() {
            return Err(SapperError::new(
                "arena token is empty (set BENCHMARK_TOKEN)",
            ));
        }
        Ok(ArenaClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            timeout,
        })
    }

    pub fn config_path(store_root: Option<&Path>) -> std::path::PathBuf {
        let base = store_root
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        base.join(ARENA_FILE)
    }

    /// Resolve config: env vars (hosted-mode injection) first, then an
    /// optional arena.json override in the graph's `.sapper/` dir.
    pub fn load(store_root: Option<&Path>) -> Result<ArenaClient> {
        let mut base_url = std::env::var("BENCHMARK_BASE_URL").unwrap_or_default();
        let mut token = std::env::var("BENCHMARK_TOKEN").unwrap_or_default();
        let mut timeout = DEFAULT_TIMEOUT;
        let mut cfg = Value::Object(serde_json::Map::new());
        if let Some(root) = store_root {
            let path = Self::config_path(Some(root));
            if path.exists() {
                let raw = std::fs::read_to_string(&path).map_err(|e| {
                    SapperError::new(format!("cannot read arena config {path:?}: {e}"))
                })?;
                cfg = serde_json::from_str(&raw).map_err(|e| {
                    SapperError::new(format!("cannot read arena config {path:?}: {e}"))
                })?;
                if !cfg.is_object() {
                    return Err(SapperError::new(format!(
                        "arena config {path:?} must be a JSON object"
                    )));
                }
            }
        }
        if let Some(b) = cfg.get("base_url").and_then(|v| v.as_str()) {
            if !b.trim().is_empty() {
                base_url = b.trim().to_string();
            }
        }
        if let Some(t) = cfg.get("token").and_then(|v| v.as_str()) {
            let raw = t.trim().to_string();
            if !raw.is_empty() {
                if let Some(var) = raw.strip_prefix("ENV:") {
                    let v = std::env::var(var.trim()).unwrap_or_default();
                    if !v.is_empty() {
                        token = v;
                    }
                } else {
                    token = raw;
                }
            }
        }
        if let Some(to) = cfg.get("timeout").and_then(|v| v.as_u64()) {
            if to > 0 {
                timeout = to;
            }
        }
        if base_url.trim().is_empty() {
            return Err(SapperError::new(
                "no arena base_url — set BENCHMARK_BASE_URL (the platform injects it in hosted mode) or write .sapper/arena.json",
            ));
        }
        Self::new(base_url, token, timeout)
    }

    pub fn write_template(store_root: &Path) -> Result<std::path::PathBuf> {
        let path = Self::config_path(Some(store_root));
        if path.exists() {
            return Err(SapperError::new(format!(
                "arena config already exists: {path:?}"
            )));
        }
        std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
        let tpl = json!({
            "base_url": "https://benchmark.example.com",
            "token": "ENV:BENCHMARK_TOKEN",
            "timeout": 30,
            "_comment": "Hosted mode needs nothing here — the platform injects BENCHMARK_BASE_URL and BENCHMARK_TOKEN. This file is an override for local testing / self-hosted runs."
        });
        let mut s = serde_json::to_string_pretty(&tpl).unwrap();
        s.push('\n');
        std::fs::write(&path, s)?;
        Ok(path)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        params: Option<&serde_json::Map<String, Value>>,
        body: Option<&Value>,
    ) -> Result<Value> {
        let mut url = format!("{}{}", self.base_url, path);
        if let Some(p) = params {
            let qs: Vec<String> = p
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                .collect();
            url.push('?');
            url.push_str(&qs.join("&"));
        }

        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout))
            .build();
        let req = match method {
            "GET" => agent.get(&url),
            "POST" => agent.post(&url),
            _ => {
                return Err(SapperError::new(format!(
                    "unsupported arena method {method}"
                )))
            }
        };
        let req = req
            .set(TOKEN_HEADER, &self.token)
            .set("Accept", "application/json");

        let result = if let Some(b) = body {
            req.send_json(b)
        } else {
            req.call()
        };

        match result {
            Ok(resp) => {
                let raw = resp
                    .into_string()
                    .map_err(|e| SapperError::new(format!("arena read failed: {e}")))?;
                if raw.trim().is_empty() {
                    return Ok(json!({}));
                }
                match serde_json::from_str::<Value>(&raw) {
                    Ok(v) => Ok(v),
                    Err(_) => Err(SapperError::new(format!(
                        "arena {path} returned non-JSON: {}",
                        &raw[..raw.len().min(200)]
                    ))),
                }
            }
            Err(ureq::Error::Status(code, resp)) => {
                let raw = resp
                    .into_string()
                    .map(|s| s.chars().take(600).collect::<String>())
                    .unwrap_or_default();
                let mut message = raw.clone();
                if let Ok(payload) = serde_json::from_str::<Value>(&raw) {
                    if payload.is_object() {
                        if let Some(m) = payload.get("message").and_then(|v| v.as_str()) {
                            message = m.to_string();
                        }
                    }
                }
                Err(SapperError::new(format!(
                    "arena {method} {path} -> HTTP {code}: {message}"
                )))
            }
            Err(ureq::Error::Transport(e)) => {
                Err(SapperError::new(format!("arena cannot reach {url}: {e}")))
            }
        }
    }

    fn _code(&self, unique_code: &str) -> Result<()> {
        if !code_valid(unique_code) {
            return Err(SapperError::new(format!(
                "invalid unique_code {unique_code:?}"
            )));
        }
        Ok(())
    }

    pub fn list_challenges(&self) -> Result<Vec<Value>> {
        let data = self.request("GET", CHALLENGES_PATH, None, None)?;
        let data = if data.is_object() && data.get("challenges").is_some() {
            data["challenges"].clone()
        } else {
            data
        };
        let arr = data
            .as_array()
            .ok_or_else(|| SapperError::new("challenges endpoint expected a list"))?;
        let out: Vec<Value> = arr
            .iter()
            .filter(|item| item.get("unique_code").and_then(|v| v.as_str()).is_some())
            .cloned()
            .collect();
        Ok(out)
    }

    pub fn start_challenge(&self, unique_code: &str) -> Result<Value> {
        self._code(unique_code)?;
        let mut params = serde_json::Map::new();
        params.insert("unique_code".into(), Value::String(unique_code.to_string()));
        let data = self.request(
            "POST",
            &format!("{CHALLENGES_PATH}/start"),
            Some(&params),
            None,
        )?;
        Ok(if data.is_object() {
            data
        } else {
            json!({ "raw": data })
        })
    }

    pub fn get_hint(&self, unique_code: &str) -> Result<Value> {
        self._code(unique_code)?;
        let mut params = serde_json::Map::new();
        params.insert("unique_code".into(), Value::String(unique_code.to_string()));
        let data = self.request(
            "GET",
            &format!("{CHALLENGES_PATH}/hint"),
            Some(&params),
            None,
        )?;
        Ok(if data.is_object() {
            data
        } else {
            json!({ "raw": data })
        })
    }

    pub fn submit_flag(&self, unique_code: &str, flag: &str) -> Result<Value> {
        self._code(unique_code)?;
        if flag.trim().is_empty() {
            return Err(SapperError::new("cannot submit an empty flag"));
        }
        let body = json!({ "unique_code": unique_code, "flag": flag.trim() });
        let data = self.request(
            "POST",
            &format!("{CHALLENGES_PATH}/submit"),
            None,
            Some(&body),
        )?;
        Ok(if data.is_object() {
            data
        } else {
            json!({ "raw": data })
        })
    }

    pub fn close_challenge(&self, unique_code: &str) -> Result<Value> {
        self._code(unique_code)?;
        let mut params = serde_json::Map::new();
        params.insert("unique_code".into(), Value::String(unique_code.to_string()));
        let data = self.request(
            "POST",
            &format!("{CHALLENGES_PATH}/close"),
            Some(&params),
            None,
        )?;
        Ok(if data.is_object() {
            data
        } else {
            json!({ "raw": data })
        })
    }
}
