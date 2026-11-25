//! The `auto` loop driver: reason -> pick frontier -> explore -> verify ->
//! repeat until goal reached or budget exhausted. Port of `cmd_auto`.

use crate::dispatch::{dispatch_reason_printed, run_explore_raw, run_reason_raw, run_verify_raw};
use crate::error::{Result, SapperError};
use crate::graph::{frontier, graph_signature};
use crate::store::Store;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

pub struct AutoOpts {
    pub store: Store,
    pub model: String,
    pub timeout: u64,
    pub templates: PathBuf,
    pub skip_permissions: bool,
    pub dry_run: bool,
    pub verify: bool,
    pub max_steps: i64,
    pub max_explore: i64,
    pub minutes: Option<i64>,
    pub deadline: Option<String>,
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn cmd_auto(o: AutoOpts) -> Result<i32> {
    let store = o.store;
    store.require()?;
    let cwd = std::env::current_dir()?;
    if o.timeout < 1 {
        return Err(SapperError::new("--timeout must be at least 1 second"));
    }
    if o.max_steps < 1 {
        return Err(SapperError::new("--max-steps must be at least 1"));
    }
    if o.max_explore < 1 {
        return Err(SapperError::new("--max-explore must be at least 1"));
    }
    let deadline = crate::util::deadline_from_args(o.deadline.as_deref(), o.minutes)?;
    if let Some(dl) = deadline {
        let stamp = crate::util::local_datetime(dl);
        println!(
            "[auto] deadline {stamp} ({})",
            crate::util::format_time_budget(deadline)
        );
    }

    println!(
        "[auto] start  model={} max_steps={} verify={}",
        o.model, o.max_steps, o.verify
    );
    let mut noop_streak = 0;
    let mut reason_failures = 0;
    let mut explore_count = 0;
    for step in 1..=o.max_steps {
        if deadline.map(|d| now_epoch() >= d).unwrap_or(false) {
            println!("[auto] deadline reached — stopping (record what you have)");
            break;
        }
        let time_budget = crate::util::format_time_budget(deadline);
        let project = store.load_project()?;
        if project["status"].as_str() == Some("completed") {
            println!("[auto] step {step}: project already completed");
            break;
        }
        let before = graph_signature(&store)?;

        // 1) reason
        println!("[auto] step {step}.1 reason");
        if o.dry_run {
            // dry-run prints the rendered prompt; it never mutates the graph,
            // so one pass is enough.
            dispatch_reason_printed(
                &store,
                &o.templates,
                &o.model,
                o.timeout,
                o.skip_permissions,
                &cwd,
                &time_budget,
                true,
            )?;
            break;
        }
        let (kind, summary) = match run_reason_raw(
            &store,
            &o.templates,
            &o.model,
            o.timeout,
            o.skip_permissions,
            &cwd,
            &time_budget,
        ) {
            Ok(v) => {
                reason_failures = 0;
                v
            }
            Err(exc) => {
                reason_failures += 1;
                println!(
                    "[auto] step {step}: reason failed ({reason_failures}): {}",
                    exc.0
                );
                if reason_failures >= 2 {
                    println!(
                        "[auto] reason failed twice in a row — stopping this challenge (graph preserved)"
                    );
                    break;
                }
                std::thread::sleep(Duration::from_secs(15));
                continue;
            }
        };
        println!("[auto]        reason -> {summary}");

        let project = store.load_project()?;
        if project["status"].as_str() == Some("completed") {
            println!("[auto] step {step}: goal completed by reason");
            break;
        }
        if kind == "noop" || kind == "rejected" {
            noop_streak += 1;
            if noop_streak >= 2 {
                println!("[auto] step {step}: reason {kind} twice — stopping");
                break;
            }
            continue;
        }
        noop_streak = 0;

        // 2) explore each ready intent, up to the per-step cap
        let auto_worker = crate::engine::worker_id(&o.model);
        let ready: Vec<Value> = frontier(&store)?
            .into_iter()
            .filter(|i| {
                let w = i.get("worker").and_then(|v| v.as_str()).unwrap_or("");
                w.is_empty() || w == auto_worker.as_str()
            })
            .collect();
        if ready.is_empty() {
            println!("[auto] step {step}.2 no ready intents after reason");
            if graph_signature(&store)? == before {
                println!("[auto] no graph change and no ready work — stopping");
                break;
            }
            continue;
        }
        for intent in ready {
            if (explore_count as i64) >= o.max_explore {
                println!("[auto] explore cap ({}) reached this run", o.max_explore);
                break;
            }
            let iid = intent["id"].as_str().unwrap_or("").to_string();
            let needs = intent
                .get("requires_confirmation")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let confirmed = intent
                .get("confirmed_at")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if needs && !confirmed {
                println!("[auto] step {step}.2 skip {iid} (needs confirmation)");
                continue;
            }
            println!("[auto] step {step}.2 explore {iid}");
            match run_explore_raw(
                &store,
                &o.templates,
                &intent,
                &o.model,
                o.timeout,
                o.skip_permissions,
                &cwd,
                true,
            ) {
                Ok(_) => explore_count += 1,
                Err(exc) => {
                    if exc.0.contains("already concluded") {
                        // the executor concluded the intent itself (`sapper done`)
                        explore_count += 1;
                        println!("[auto]        {iid} concluded by the executor itself");
                        continue;
                    }
                    println!("[auto] explore {iid} failed: {}", exc.0);
                    continue;
                }
            }
            // 3) optional verify
            if o.verify {
                let refreshed = store.get_intent(&iid)?;
                if !refreshed["to"].is_null() {
                    println!("[auto] step {step}.3 verify {iid}");
                    match run_verify_raw(
                        &store,
                        &o.templates,
                        &refreshed,
                        &o.model,
                        o.timeout,
                        o.skip_permissions,
                        &cwd,
                    ) {
                        Ok((verified, issues)) => {
                            let tag = if verified { "VERIFIED" } else { "FAILED" };
                            println!(
                                "[auto]        verify {iid} -> {tag}{}",
                                if issues.is_empty() { "" } else { ": " }
                            );
                            if !issues.is_empty() {
                                println!("{issues}");
                            }
                            if !verified {
                                let content = if issues.is_empty() {
                                    format!("verification failed for {iid}: the produced fact did not pass verification")
                                } else {
                                    format!("verification failed for {iid}: {issues}")
                                };
                                store.add_hint(&content, "verify").ok();
                            }
                        }
                        Err(exc) => {
                            println!("[auto] verify {iid} failed: {}", exc.0);
                            store
                                .add_hint(
                                    &format!(
                                        "verification could not complete for {iid}: {}",
                                        exc.0
                                    ),
                                    "verify",
                                )
                                .ok();
                        }
                    }
                }
            }
        }
        if graph_signature(&store)? == before {
            println!("[auto] step {step}: no graph change after work — stopping");
            break;
        }
    }
    let project = store.load_project()?;
    println!(
        "[auto] done  status={}  explored={}",
        project["status"].as_str().unwrap_or("?"),
        explore_count
    );
    Ok(0)
}
