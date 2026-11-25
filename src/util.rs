//! Shared low-level helpers: JSON I/O (order-preserving, git-diffable),
//! time, text scrubbing, deadline parsing.

use crate::error::{Result, SapperError};
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const GOAL_ID: &str = "goal";
pub const DEFAULT_STORE: &str = ".sapper";
pub const LOCK_NAME: &str = ".lock";
pub const PEERS_FILE: &str = "ai-peers.json";
pub const AI_CHANNEL_NAME: &str = "ai-channel.txt";
pub const FACT_KINDS: [&str; 7] = ["host", "service", "cred", "vuln", "loot", "flag", "note"];

/// JSON output matching the Python port: 2-space indent, key order preserved
/// (serde_json `preserve_order`), non-ASCII kept readable, trailing newline.
pub fn dump_json(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("serialize");
    s.push('\n');
    s
}

pub fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| SapperError::new(format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|e| SapperError::new(format!("cannot read {}: {e}", path.display())))
}

pub fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest: Vec<char> = chars.collect();
    rest.len() <= 16
        && rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
}

pub fn is_peer_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn control_char_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"[\x00-\x1f\x7f]+").unwrap())
}

/// Strip control characters (newlines, tabs, ...) so a single node's
/// title/description always occupies one line in rendered output.
pub fn scrub_inline(text: Option<&Value>) -> String {
    let s = match text {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(v) => v.to_string(),
        None => return String::new(),
    };
    control_char_re().replace_all(&s, " ").into_owned()
}

/// Make graph text safe to place inside markdown prompt fences.
pub fn prompt_inline(text: Option<&Value>) -> String {
    scrub_inline(text).replace('`', "'")
}

pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Local `YYYY-MM-DD HH:MM` stamp used by `sapper send`.
pub fn now_local_stamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M").to_string()
}

fn local_offset_secs() -> i64 {
    Local::now().offset().local_minus_utc() as i64
}

/// Resolve --deadline <ISO8601> / --minutes <n> into an epoch-seconds float.
/// Mirrors the Python `deadline_from_args` contract: Z suffix, naive strings
/// interpreted as LOCAL time, exactly one of the two.
pub fn deadline_from_args(deadline: Option<&str>, minutes: Option<i64>) -> Result<Option<f64>> {
    if deadline.is_some() && minutes.is_some() {
        return Err(SapperError::new(
            "pass either --deadline or --minutes, not both",
        ));
    }
    if let Some(d) = deadline {
        let text = d.trim();
        let with_z = text
            .strip_suffix('Z')
            .map(|t| format!("{t}+00:00"))
            .unwrap_or_else(|| text.to_string());
        if let Ok(parsed) = DateTime::parse_from_rfc3339(&with_z) {
            return Ok(Some(parsed.timestamp() as f64));
        }
        // naive local time
        for fmt in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M:%S",
        ] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(&with_z, fmt) {
                let ts = naive.and_utc().timestamp() - local_offset_secs();
                return Ok(Some(ts as f64));
            }
        }
        return Err(SapperError::new(format!(
            "--deadline must be ISO 8601 (e.g. 2026-10-17T13:00:00+08:00): cannot parse {text}"
        )));
    }
    if let Some(m) = minutes {
        if m <= 0 {
            return Err(SapperError::new("--minutes must be positive"));
        }
        return Ok(Some(Utc::now().timestamp() as f64 + m as f64 * 60.0));
    }
    Ok(None)
}

/// Convert an epoch-seconds deadline into a local `DateTime<Local>` (if any).
fn local_from_epoch(secs: f64) -> Option<DateTime<Local>> {
    let dt = chrono::DateTime::from_timestamp(secs.round() as i64, 0)?;
    Some(dt.with_timezone(&Local))
}

/// Format an epoch-seconds deadline as local `HH:MM`.
pub fn local_hhmm(secs: f64) -> String {
    local_from_epoch(secs)
        .map(|l| l.format("%H:%M").to_string())
        .unwrap_or_else(|| "??:??".to_string())
}

/// Format an epoch-seconds deadline as local `YYYY-MM-DD HH:MM`.
pub fn local_datetime(secs: f64) -> String {
    local_from_epoch(secs)
        .map(|l| l.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// Human-readable remaining time for prompt injection.
pub fn format_time_budget(deadline: Option<f64>) -> String {
    let Some(d) = deadline else {
        return String::new();
    };
    let now = Utc::now().timestamp() as f64;
    let remaining = d - now;
    if remaining <= 0.0 {
        return "TIME IS UP — no time remains; only record/submit what you already have"
            .to_string();
    }
    let end_stamp = local_hhmm(d);
    let whole = remaining as i64;
    let remaining_text = if whole >= 3600 {
        format!("{}h{:02}m", whole / 3600, (whole % 3600) / 60)
    } else {
        format!("{} minutes", whole / 60)
    };
    format!("{remaining_text} remaining until the {end_stamp} deadline (local time)")
}

/// Run `date` for hook time context; returns a graceful placeholder on failure.
pub fn date_context() -> String {
    let out = Command::new("date")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if msg.is_empty() {
                format!("`date` exited {:?}", o.status.code())
            } else {
                msg
            }
        }
        Err(_) => "`date` unavailable".to_string(),
    }
}

/// Open the editor ($EDITOR / $VISUAL / vi), run it on a temp file, return
/// the new content. Mirrors `_edit_via_editor`.
pub fn edit_via_editor(initial: &str, _title: &str) -> Result<String> {
    use std::env::temp_dir;
    use std::io::Write as _;
    let mut editor_opt: Option<String> = None;
    if let Ok(v) = std::env::var("EDITOR") {
        if !v.is_empty() {
            editor_opt = Some(v);
        }
    }
    if editor_opt.is_none() {
        if let Ok(v) = std::env::var("VISUAL") {
            if !v.is_empty() {
                editor_opt = Some(v);
            }
        }
    }
    let editor = editor_opt.unwrap_or_else(|| "vi".to_string());
    let tmp = temp_dir().join(format!(".sapper-edit-{}.md", std::process::id()));
    {
        let mut fh = fs::File::create(&tmp)?;
        fh.write_all(initial.as_bytes())?;
    }
    let status = Command::new(&editor).arg(&tmp).status()?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(SapperError::new(format!(
            "editor {editor} exited with {status}"
        )));
    }
    let content = fs::read_to_string(&tmp)?;
    let _ = fs::remove_file(&tmp);
    Ok(content)
}

/// Crypto entropy: `secrets.token_hex(n)` equivalent.
pub fn random_hex(n: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| format!("{:02x}", rng.gen_range(0..256u16)))
        .collect()
}

/// Expand a leading `~` against the user's home directory.
pub fn expand_user(s: &str) -> PathBuf {
    let s = if let Some(rest) = s.strip_prefix('~') {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        format!("{home}{rest}")
    } else {
        s.to_string()
    };
    PathBuf::from(s)
}

/// Walk upward from the directory of the running executable (and, as a
/// fallback, the current directory) looking for `marker` under any ancestor.
/// Lets a binary built under `.../target/release` find `.../assets/templates`
/// or `.../assets/kb` at the project root.
pub fn find_ancestor_asset(marker: &Path) -> Option<PathBuf> {
    let roots: Vec<PathBuf> = vec![
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_default(),
        std::env::current_dir().unwrap_or_default(),
    ];
    for start in roots {
        let mut dir = Some(start.clone());
        for _ in 0..8 {
            let cand = dir.as_ref().unwrap().join(marker);
            if cand.is_dir() {
                return Some(cand);
            }
            dir = dir
                .as_ref()
                .and_then(|d| d.parent().map(|p| p.to_path_buf()));
        }
    }
    None
}

/// Best-effort "open in browser" (Windows: start; Linux: xdg-open/open/wslview).
pub fn try_open(path_or_url: &str) -> bool {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/c", "start", "/b"])
            .arg(path_or_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        let opener = ["xdg-open", "open", "wslview"]
            .iter()
            .find(|name| Command::new(name).arg("--version").output().is_ok());
        match opener {
            Some(o) => Command::new(o)
                .arg(path_or_url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids() {
        assert!(is_valid_id("f001"));
        assert!(!is_valid_id("F001"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("1abc"));
    }

    #[test]
    fn scrub_strips_control() {
        let v = serde_json::json!("a\nb\tc");
        assert_eq!(scrub_inline(Some(&v)), "a b c");
    }

    #[test]
    fn deadline_minutes() {
        let d = deadline_from_args(None, Some(5)).unwrap().unwrap();
        assert!(d > 0.0);
    }

    #[test]
    fn deadline_both_errors() {
        assert!(deadline_from_args(Some("2026-01-01T00:00:00Z"), Some(5)).is_err());
    }

    // -- byte-level Python compatibility (ensure_ascii=False, indent=2, \n) --
    // Golden files are generated by tests/golden/make_golden.py using the exact
    // serialization the Python version performs:
    //   json.dump(data, ensure_ascii=False, indent=2, sort_keys=False) + "\n"
    // `dump_json` must be byte-for-byte identical, including raw CJK bytes
    // (no \uXXXX escaping), key order, 2-space indent, and the trailing newline.
    const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");

    fn golden(name: &str) -> Vec<u8> {
        std::fs::read(format!("{GOLDEN}{name}"))
            .unwrap_or_else(|e| panic!("cannot read golden {name}: {e}"))
    }

    fn assert_matches_golden(name: &str, v: &Value) {
        let expected = golden(name);
        let actual = dump_json(v).into_bytes();
        assert_eq!(
            actual,
            expected,
            "byte mismatch for {name}\n--- actual ---\n{}\n--- expected ---\n{}",
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(&expected)
        );
        // explicitly: no CJK must have been escaped to \uXXXX
        let text = String::from_utf8_lossy(&actual);
        assert!(
            !text.contains("\\u"),
            "{name}: found \\u escape; CJK must be written raw (ensure_ascii=False parity)"
        );
    }

    #[test]
    fn golden_project() {
        let v = serde_json::json!({
            "id": "proj",
            "origin": "smoke-test",
            "goal": "capture the flag",
            "status": "active",
            "created_at": "2026-01-01T00:00:00Z",
            "counters": {"fact": 2, "intent": 1, "hint": 1, "run": 0}
        });
        assert_matches_golden("project.json", &v);
    }

    #[test]
    fn golden_goal() {
        let v = serde_json::json!({
            "id": "goal",
            "description": "capture the flag",
            "created_at": "2026-01-01T00:00:00Z",
            "creator": "system",
            "source_intent": null
        });
        assert_matches_golden("goal.json", &v);
    }

    #[test]
    fn golden_fact_cjk() {
        let v = serde_json::json!({
            "id": "f001",
            "title": "主机",
            "description": "目标 服务 host",
            "doc": null,
            "kind": "service",
            "data": {"addr": "10.0.0.5", "port": "22"},
            "created_at": "2026-01-01T00:00:00Z",
            "creator": "tester",
            "source_intent": null
        });
        assert_matches_golden("f001.json", &v);
    }

    #[test]
    fn golden_intent() {
        let v = serde_json::json!({
            "id": "i001",
            "title": null,
            "from": ["goal", "f001"],
            "to": null,
            "description": "扫描 services",
            "doc": null,
            "creator": "tester",
            "status": "open",
            "worker": null,
            "requires_confirmation": false,
            "confirmed_at": null,
            "created_at": "2026-01-01T00:00:00Z",
            "concluded_at": null
        });
        assert_matches_golden("i001.json", &v);
    }

    #[test]
    fn golden_hint_cjk() {
        let v = serde_json::json!({
            "id": "h001",
            "content": "尝试 中文 提示",
            "creator": "tester",
            "created_at": "2026-01-01T00:00:00Z"
        });
        assert_matches_golden("h001.json", &v);
    }
}
