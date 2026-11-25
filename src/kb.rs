//! Shared offline knowledge base (techniques / CVEs / writeups).
//! Port of `kb.py`: one JSON file per entry under `kb/entries/`,
//! token-overlap search with CJK awareness.

use crate::error::{Result, SapperError};
use crate::graph::recall_tokenize;
use crate::util::{dump_json, now_iso};
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct KbError(pub String);

impl KbError {
    pub fn new(msg: impl Into<String>) -> Self {
        KbError(msg.into())
    }
}

impl From<KbError> for SapperError {
    fn from(e: KbError) -> Self {
        SapperError(e.0)
    }
}

fn kb_id_valid(id: &str) -> bool {
    let re = regex::Regex::new(r"^kb\d{3,}$").unwrap();
    re.is_match(id)
}

fn tag_valid(tag: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z0-9][a-z0-9_-]*$").unwrap();
    re.is_match(tag)
}

pub struct KnowledgeBase {
    pub root: PathBuf,
    entries_dir: PathBuf,
}

impl KnowledgeBase {
    pub fn new(root: PathBuf) -> Self {
        let entries_dir = root.join("entries");
        KnowledgeBase { root, entries_dir }
    }

    /// Default root: `SAPPER_KB_ROOT` env, else the `kb/` dir shipped next to
    /// the running binary (or the CWD when run from source).
    pub fn default_root() -> PathBuf {
        if let Ok(env) = std::env::var("SAPPER_KB_ROOT") {
            if !env.is_empty() {
                return expand_user(&env);
            }
        }
        // walk up from the exe / CWD to find a packaged assets/kb (or bare
        // kb/) layout — this works whether the binary lives in
        // target/release/, in the project root, or anywhere under it.
        if let Some(d) = crate::util::find_ancestor_asset(std::path::Path::new("assets/kb")) {
            if d.join("entries").is_dir() {
                return d;
            }
        }
        if let Some(d) = crate::util::find_ancestor_asset(std::path::Path::new("kb")) {
            if d.join("entries").is_dir() {
                return d;
            }
        }
        std::env::current_dir()
            .map(|d| d.join("kb"))
            .unwrap_or_else(|_| PathBuf::from("kb"))
    }

    fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.entries_dir)
    }

    pub fn list_entries(&self) -> Vec<Value> {
        if !self.entries_dir.is_dir() {
            return vec![];
        }
        let mut out = vec![];
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&self.entries_dir)
            .map(|it| {
                it.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .map(|e| e.to_string_lossy() == "json")
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        paths.sort();
        for p in paths {
            let raw = match std::fs::read_to_string(&p) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "sapper: warning: skipping bad kb entry {}: {e}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    );
                    continue;
                }
            };
            let data = match serde_json::from_str::<Value>(&raw) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "sapper: warning: skipping bad kb entry {}: {e}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    );
                    continue;
                }
            };
            if data.get("id").and_then(|v| v.as_str()).is_some() {
                out.push(data);
            }
        }
        out
    }

    fn next_id(&self) -> Result<String> {
        let n = self.list_entries().len() as i64 + 1;
        let mut n = n;
        while self.entries_dir.join(format!("kb{n:03}.json")).exists() {
            n += 1;
        }
        Ok(format!("kb{n:03}"))
    }

    pub fn add(
        &self,
        title: &str,
        description: &str,
        detail: &str,
        commands: &[String],
        tags: &[String],
        source: &str,
        entry_id: Option<&str>,
    ) -> Result<Value> {
        let title = title.trim();
        if title.is_empty() {
            return Err(KbError::new("kb entry needs a title").into());
        }
        let tags: Vec<String> = tags
            .iter()
            .filter_map(|t| {
                let t = t.trim().to_lowercase();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            })
            .collect();
        for t in &tags {
            if !tag_valid(t) {
                return Err(
                    KbError::new(format!("kb tag {t:?} must match [a-z0-9][a-z0-9_-]*")).into(),
                );
            }
        }
        let commands: Vec<String> = commands
            .iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();

        let resolved_id: Option<String> = match entry_id {
            None => {
                self.ensure()?;
                Some(self.next_id()?)
            }
            Some(id) => {
                if !kb_id_valid(id) {
                    return Err(KbError::new(format!(
                        "kb entry id must look like kb001, got {id:?}"
                    ))
                    .into());
                }
                Some(id.to_string())
            }
        };
        let entry_id = resolved_id.ok_or_else(|| KbError::new("kb entry id not resolved"))?;
        let detail = detail.trim();
        let entry = json!({
            "id": entry_id,
            "title": title,
            "description": description.trim(),
            "detail": if detail.is_empty() { Value::Null } else { Value::String(detail.to_string()) },
            "commands": Value::Array(commands.iter().map(|c| Value::String(c.clone())).collect()),
            "tags": Value::Array(tags.iter().map(|t| Value::String(t.clone())).collect()),
            "source": if source.trim().is_empty() { Value::Null } else { Value::String(source.trim().to_string()) },
            "created_at": now_iso(),
        });
        self.ensure()?;
        let path = self.entries_dir.join(format!("{entry_id}.json"));
        if path.exists() {
            return Err(KbError::new(format!(
                "kb entry {entry_id} already exists ({})",
                path.display()
            ))
            .into());
        }
        std::fs::write(&path, dump_json(&entry))?;
        Ok(entry)
    }

    pub fn get(&self, entry_id: &str) -> Result<Value> {
        if !kb_id_valid(entry_id) {
            return Err(KbError::new(format!(
                "kb entry id must look like kb001, got {entry_id:?}"
            ))
            .into());
        }
        let path = self.entries_dir.join(format!("{entry_id}.json"));
        if !path.exists() {
            return Err(KbError::new(format!("kb entry {entry_id} not found")).into());
        }
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw)
            .map_err(|e| KbError::new(format!("bad kb entry {entry_id}: {e}")).into())
    }

    /// Token-overlap search (title x2 + description + tags + detail[:500]).
    pub fn search(&self, query: &str, tag: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        if query.trim().is_empty() {
            return Ok(vec![]);
        }
        let q_tokens: std::collections::HashSet<String> =
            recall_tokenize(query).into_iter().collect();
        if q_tokens.is_empty() {
            return Ok(vec![]);
        }
        let tag = tag.map(|t| t.trim().to_lowercase());
        let mut scored: Vec<(f64, String, Value)> = vec![];
        for e in self.list_entries() {
            if let Some(tag) = &tag {
                let tags_lower: Vec<String> = e["tags"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                            .collect()
                    })
                    .unwrap_or_default();
                if !tags_lower.contains(tag) {
                    continue;
                }
            }
            let title = e
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let desc = e
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let tags: String = e["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
                .to_lowercase();
            let detail = e
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let detail500: String = detail.chars().take(500).collect();
            let haystack = format!("{title} {title} {desc} {tags} {detail500}");
            let h_tokens: std::collections::HashSet<String> =
                recall_tokenize(&haystack).into_iter().collect();
            if h_tokens.is_empty() {
                continue;
            }
            let overlap = q_tokens.iter().filter(|t| h_tokens.contains(*t)).count();
            if overlap == 0 {
                continue;
            }
            let score = (overlap as f64 / q_tokens.len().max(1) as f64) * 1000.0;
            let score = score.round() / 1000.0; // round(score, 3)
            let id = e
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let mut b = brief(&e);
            if let Some(obj) = b.as_object_mut() {
                let num = serde_json::Number::from_f64(score)
                    .unwrap_or_else(|| serde_json::Number::from(0));
                obj.insert("score".into(), Value::Number(num));
            }
            scored.push((score, id, b));
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, _, v)| v).collect())
    }
}

fn brief(e: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), e.get("id").cloned().unwrap_or(Value::Null));
    out.insert(
        "title".into(),
        e.get("title").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "description".into(),
        e.get("description")
            .cloned()
            .unwrap_or(Value::String(String::new())),
    );
    out.insert(
        "tags".into(),
        e.get("tags")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![])),
    );
    out.insert(
        "has_detail".into(),
        Value::Bool(e.get("detail").map(|v| !v.is_null()).unwrap_or(false)),
    );
    let n = e["commands"].as_array().map(|a| a.len()).unwrap_or(0);
    out.insert("n_commands".into(), Value::from(n));
    let mut m = serde_json::Map::new();
    for (k, v) in out {
        m.insert(k, v);
    }
    Value::Object(m)
}

/// Import a markdown file as one entry.
pub fn import_markdown(path: &std::path::Path, base_tags: &[String], source: &str) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let mut title = String::new();
    let re = regex::Regex::new(r"^#\s+(.+)$").unwrap();
    let mut body_start = 0;
    for (i, line) in lines.iter().enumerate() {
        if let Some(m) = re.captures(line.trim()) {
            title = m[1].trim().to_string();
            body_start = i + 1;
            break;
        }
    }
    if title.is_empty() {
        title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(['-', '_'], " ");
    }
    let body = lines[body_start..].join("\n");
    let body = body.trim().to_string();
    let mut description = String::new();
    for line in lines[body_start..].iter() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        if let Some(stripped) = s.strip_prefix('>') {
            description = stripped
                .trim_start()
                .trim_start_matches(' ')
                .trim()
                .to_string();
        }
        break;
    }
    json!({
        "title": title,
        "description": description,
        "detail": body,
        "tags": Value::Array(base_tags.iter().map(|t| Value::String(t.clone())).collect()),
        "source": if source.is_empty() { Value::String(path.file_name().unwrap_or_default().to_string_lossy().to_string()) } else { Value::String(source.to_string()) },
    })
}

fn expand_user(s: &str) -> PathBuf {
    let s = s.replace("~", &home_dir());
    PathBuf::from(s)
}

fn home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default()
}
