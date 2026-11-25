//! TSecBench full-loop driver: list -> start -> solve -> submit -> close.
//! Port of `cmd_bench` + `_bench_parallel` + `_bench_submit_recorded` +
//! `_bench_prepare_graph`. Runs the `sapper auto` CLI as a subprocess per
//! challenge graph (self-spawn, like the Python version's `cli`).

use crate::arena::ArenaClient;
use crate::error::{Result, SapperError};
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct BenchOpts {
    pub root: String,
    pub minutes: Option<i64>,
    pub deadline: Option<String>,
    pub minutes_per_challenge: i64,
    pub max_steps: i64,
    pub round_gap_seconds: i64,
    pub parallel: i64,
    pub poll_seconds: i64,
    pub model: String,
    pub templates: Option<String>,
    pub skip_permissions: bool,
    pub verify: bool,
}

fn self_cli() -> Vec<String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sapper"));
    vec![exe.to_string_lossy().to_string()]
}

fn arena_client_for_root(root: &std::path::Path) -> Result<ArenaClient> {
    for candidate in [root.join(".sapper"), PathBuf::from(".sapper")] {
        if candidate.join("arena.json").exists() {
            return ArenaClient::load(Some(&candidate));
        }
    }
    let fallback = root.join(".sapper");
    ArenaClient::load(Some(&fallback))
}

fn bench_store(root: &std::path::Path, code: &str) -> Store {
    Store::new(root.join(code).join(".sapper"))
}

fn bench_prepare_graph(store: &Store, challenge: &Value, container_addrs: &[String]) -> Result<()> {
    let code = challenge
        .get("unique_code")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    if !store.exists() {
        store.init(
            "TSecBench",
            &format!("solve challenge {code} and capture all flags"),
        )?;
    }
    let facts = store.list_facts()?;

    let add = |kind: &str,
               title: &str,
               description: &str,
               data: &serde_json::Map<String, Value>|
     -> std::result::Result<(), SapperError> {
        let exists = facts.iter().any(|f| {
            f.get("kind").and_then(|v| v.as_str()) == Some(kind)
                && f.get("data").cloned().unwrap_or(Value::Null) == Value::Object(data.clone())
        });
        if !exists {
            store.add_fact(
                description,
                "arena",
                None,
                Some(title),
                None,
                Some(kind),
                Some(data),
            )?;
        }
        Ok(())
    };

    let desc = challenge
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("(no description)");
    let note_data = json!({
        "challenge": code,
        "difficulty": challenge.get("difficulty").map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
    });
    add(
        "note",
        "题目描述",
        &format!("challenge {code}: {desc}"),
        &note_data.as_object().cloned().unwrap(),
    )?;
    for addr in container_addrs {
        let host_data = json!({
            "addr": addr,
            "challenge": code,
        });
        add(
            "host",
            "靶标",
            &format!("target container address {addr}"),
            &host_data.as_object().cloned().unwrap(),
        )?;
    }
    let flag_count = challenge
        .get("flag_count")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let correct = challenge
        .get("correct_flag_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let targets: String = if container_addrs.is_empty() {
        "none".into()
    } else {
        container_addrs.join(", ")
    };
    store.add_hint(
        &format!(
            "challenge {code}: {flag_count} flag(s) required, {correct} already correct. \
             Targets: {targets}. Submit flags with `sapper flag <value> -c <code> --submit`."
        ),
        "arena",
    )?;
    Ok(())
}

fn bench_submit_recorded(store: &Store, code: &str, client: &ArenaClient) -> Result<i32> {
    let mut submitted = 0;
    let facts = store.list_facts()?;
    let empty = Value::Object(serde_json::Map::new());
    for f in facts {
        if f.get("kind").and_then(|v| v.as_str()) != Some("flag") {
            continue;
        }
        let data_v = f.get("data").cloned().unwrap_or(Value::Null);
        let data: &Value = if data_v.is_object() { &data_v } else { &empty };
        let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if !(status.is_empty() || status == "recorded") {
            continue;
        }
        let value = data.get("value").and_then(|v| v.as_str()).unwrap_or("");
        if value.is_empty() {
            continue;
        }
        let fid = f["id"].as_str().unwrap_or("").to_string();
        match client.submit_flag(code, value) {
            Ok(reply) => {
                let correct = reply
                    .get("correct")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let awarded = reply.get("awarded").and_then(|v| v.as_i64()).unwrap_or(0);
                let progress = format!(
                    "{}/{}",
                    reply
                        .get("correct_flag_count")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?"),
                    reply
                        .get("total_flag_count")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                );
                let status = if correct { "submitted" } else { "rejected" };
                let result_str = serde_json::to_string(&reply)
                    .unwrap_or_default()
                    .chars()
                    .take(400)
                    .collect::<String>();
                store.update_flag_fact(&fid, status, Some(&result_str))?;
                println!(
                    "[bench]   {fid} {} +{awarded} ({progress})",
                    if correct { "CORRECT" } else { "wrong" }
                );
                submitted += 1;
            }
            Err(exc) => {
                store.update_flag_fact(&fid, "rejected", Some(&exc.0))?;
                println!("[bench]   submit failed for {fid}: {}", exc.0);
            }
        }
    }
    Ok(submitted)
}

fn engine_args(o: &BenchOpts) -> Vec<String> {
    let mut args = vec!["--model".to_string(), o.model.clone()];
    if o.skip_permissions {
        args.push("--skip-permissions".into());
    }
    if o.verify {
        args.push("--verify".into());
    }
    if let Some(t) = &o.templates {
        args.push("--templates".into());
        args.push(t.clone());
    }
    args
}

pub fn cmd_bench(o: BenchOpts) -> Result<i32> {
    let root = PathBuf::from(&o.root);
    std::fs::create_dir_all(&root)?;
    let deadline = crate::util::deadline_from_args(o.deadline.as_deref(), o.minutes)?;

    let client = arena_client_for_root(&root)?;
    println!("[bench] arena {}", client.base_url);
    let challenges = match client.list_challenges() {
        Ok(c) => c,
        Err(exc) => {
            println!("[bench] cannot list challenges: {}", exc.0);
            return Ok(1);
        }
    };
    let todo: Vec<Value> = challenges
        .iter()
        .filter(|c| {
            !c.get("is_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    println!(
        "[bench] {} challenges, {} unfinished",
        challenges.len(),
        todo.len()
    );

    if o.parallel > 1 {
        return bench_parallel(&o, &client, &root, todo, deadline);
    }
    bench_serial(&o, &client, &root, &challenges, deadline)
}

fn bench_serial(
    o: &BenchOpts,
    client: &ArenaClient,
    root: &std::path::Path,
    challenges: &[Value],
    deadline: Option<f64>,
) -> Result<i32> {
    let cli = self_cli();
    let eargs = engine_args(o);
    let mut todo: Vec<Value> = challenges
        .iter()
        .filter(|c| {
            !c.get("is_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let mut rounds = 0;
    while !todo.is_empty() && deadline.map(|d| now_epoch() < d).unwrap_or(true) {
        rounds += 1;
        let mut progressed = false;
        for ch in todo.clone() {
            if deadline.map(|d| now_epoch() >= d).unwrap_or(false) {
                println!("[bench] deadline reached");
                break;
            }
            let code = ch
                .get("unique_code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let store = bench_store(root, &code);
            let started = match client.start_challenge(&code) {
                Ok(s) => s,
                Err(exc) => {
                    if exc.0.to_lowercase().contains("max active") {
                        println!(
                            "[bench] active-container cap reached; closing the oldest and retrying next round"
                        );
                        break;
                    }
                    println!("[bench] {code}: cannot start ({})", exc.0);
                    continue;
                }
            };
            let addrs: Vec<String> = started
                .get("container_addr")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "\n[bench] {code} started -> {}",
                if addrs.is_empty() {
                    "(no addr)".into()
                } else {
                    addrs.join(", ")
                }
            );
            if let Err(exc) = bench_prepare_graph(&store, &ch, &addrs) {
                println!(
                    "[bench] {code}: graph prep failed ({}); skipping this round",
                    exc.0
                );
                let _ = client.close_challenge(&code);
                continue;
            }
            run_auto_subprocess(&cli, &store, o, &eargs);
            if let Err(exc) = bench_submit_recorded(&store, &code, client) {
                println!(
                    "[bench] {code}: flag submission failed ({}); \
                     flags stay recorded and will retry next round",
                    exc.0
                );
            }
            if let Ok(fresh) = client.list_challenges() {
                let done = fresh.iter().any(|c| {
                    c.get("unique_code").and_then(|v| v.as_str()) == Some(code.as_str())
                        && c.get("is_completed")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                });
                if done {
                    println!("[bench] {code} COMPLETE");
                }
            }
            match client.close_challenge(&code) {
                Ok(_) => println!("[bench] {code} closed"),
                Err(exc) => {
                    println!("[bench] {code} close failed (non-fatal): {}", exc.0)
                }
            }
            progressed = true;
        }
        match client.list_challenges() {
            Ok(refreshed) => {
                todo = refreshed
                    .iter()
                    .filter(|c| {
                        !c.get("is_completed")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
            }
            Err(exc) => {
                println!("[bench] cannot refresh challenges: {}", exc.0);
                break;
            }
        }
        if !progressed {
            println!("[bench] no progress this round — stopping");
            break;
        }
        println!("[bench] round {rounds}: {} still unfinished", todo.len());
        std::thread::sleep(std::time::Duration::from_secs(
            o.round_gap_seconds.max(0) as u64
        ));
    }
    let challenges = client.list_challenges().unwrap_or_default();
    let done = challenges
        .iter()
        .filter(|c| {
            c.get("is_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    println!(
        "[bench] finished: {done}/{} complete after {rounds} round(s)",
        challenges.len()
    );
    Ok(0)
}

fn bench_parallel(
    o: &BenchOpts,
    client: &ArenaClient,
    root: &std::path::Path,
    todo: Vec<Value>,
    deadline: Option<f64>,
) -> Result<i32> {
    let cli = self_cli();
    let eargs = engine_args(o);
    let limit = o.parallel.max(1);
    let deadline_str = deadline
        .map(crate::util::local_hhmm)
        .unwrap_or_else(|| "none".into());
    println!("[bench] parallel mode: up to {limit} workers (deadline {deadline_str})");
    let mut pending: VecDeque<Value> = todo.into();
    let mut running: HashMap<String, (std::process::Child, Store)> = HashMap::new();
    let mut done_flags: i32 = 0;

    let finish = |code: &str,
                  proc: &mut std::process::Child,
                  store: &Store,
                  client: &ArenaClient,
                  done_flags: &mut i32| {
        let _ = proc.wait();
        match bench_submit_recorded(store, code, client) {
            Ok(n) => *done_flags += n,
            Err(exc) => {
                println!(
                    "[bench] {code}: flag submission failed ({}); retry next round",
                    exc.0
                )
            }
        }
        if let Ok(fresh) = client.list_challenges() {
            let completed = fresh.iter().any(|c| {
                c.get("unique_code").and_then(|v| v.as_str()) == Some(code)
                    && c.get("is_completed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
            });
            if completed {
                println!("[bench] {code} COMPLETE");
            }
        }
        match client.close_challenge(code) {
            Ok(_) => println!("[bench] {code} closed"),
            Err(exc) => {
                println!("[bench] {code} close failed (non-fatal): {}", exc.0)
            }
        }
    };

    while (!pending.is_empty() || !running.is_empty())
        && deadline.map(|d| now_epoch() < d).unwrap_or(true)
    {
        // backfill free slots
        while !pending.is_empty() && (running.len() as i64) < limit {
            if deadline.map(|d| now_epoch() >= d).unwrap_or(false) {
                break;
            }
            let ch = pending.pop_front().unwrap();
            let code = ch
                .get("unique_code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let started = match client.start_challenge(&code) {
                Ok(s) => s,
                Err(exc) => {
                    if exc.0.to_lowercase().contains("max active") {
                        pending.push_front(ch);
                        println!(
                            "[bench] platform active-container cap reached; waiting for a slot"
                        );
                        break;
                    }
                    println!(
                        "[bench] {code}: cannot start ({}); will retry next round",
                        exc.0
                    );
                    pending.push_back(ch);
                    break;
                }
            };
            let addrs: Vec<String> = started
                .get("container_addr")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "[bench] {code} started -> {} (workers: {}/{} )",
                if addrs.is_empty() {
                    "(no addr)".into()
                } else {
                    addrs.join(", ")
                },
                running.len() + 1,
                limit
            );
            let store = bench_store(root, &code);
            if let Err(exc) = bench_prepare_graph(&store, &ch, &addrs) {
                println!("[bench] {code}: graph prep failed ({}); skipping", exc.0);
                let _ = client.close_challenge(&code);
                continue;
            }
            let mut cmd_args = vec![
                "--store".into(),
                store.root.to_string_lossy().into(),
                "auto".into(),
                "--minutes".into(),
                o.minutes_per_challenge.to_string(),
                "--max-steps".into(),
                o.max_steps.to_string(),
            ];
            cmd_args.extend(eargs.iter().cloned());
            let child = Command::new(&cli[0])
                .args(&cmd_args)
                .current_dir(root.join(&code))
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| SapperError::new(format!("cannot spawn auto worker: {e}")))?;
            running.insert(code.clone(), (child, store));
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        // reap finished workers
        for code in running.keys().cloned().collect::<Vec<_>>() {
            if let Some((child, _store)) = running.get_mut(&code) {
                if let Ok(Some(_status)) = child.try_wait() {
                    let mut entry = running.remove(&code).expect("just checked");
                    finish(&code, &mut entry.0, &entry.1, client, &mut done_flags);
                }
            }
        }
        if !running.is_empty() {
            std::thread::sleep(std::time::Duration::from_secs(o.poll_seconds.max(0) as u64));
        } else if !pending.is_empty() {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
    // deadline: submit whatever the still-running workers already recorded
    let codes: Vec<String> = running.keys().cloned().collect();
    for code in &codes {
        if let Some((child, _)) = running.get_mut(code) {
            let _ = child.kill();
        }
    }
    for code in codes {
        if let Some(mut entry) = running.remove(&code) {
            finish(&code, &mut entry.0, &entry.1, client, &mut done_flags);
        }
    }
    let challenges = client.list_challenges().unwrap_or_default();
    let done = challenges
        .iter()
        .filter(|c| {
            c.get("is_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .count();
    println!(
        "[bench] finished: {done}/{} complete, {done_flags} flag submission(s) accepted",
        challenges.len()
    );
    Ok(0)
}

fn run_auto_subprocess(cli: &[String], store: &Store, o: &BenchOpts, eargs: &[String]) {
    let mut args = vec![
        "--store".into(),
        store.root.to_string_lossy().into(),
        "auto".into(),
        "--minutes".into(),
        o.minutes_per_challenge.to_string(),
        "--max-steps".into(),
        o.max_steps.to_string(),
    ];
    args.extend(eargs.iter().cloned());
    let cwd = store
        .root
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let _ = Command::new(&cli[0])
        .args(&args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    // non-zero exit is not fatal: flags stay recorded and retry next round
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
