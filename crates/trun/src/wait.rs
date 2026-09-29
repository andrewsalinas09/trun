//! `trun wait`: block until something worth reacting to happens (docs/08-mcp.md,
//! "the wake-up pattern"). An AI agent runs it as a background command and is woken
//! when it exits: the exit code says what happened, stdout carries the digest.

use crate::client::Client;
use anyhow::{Result, bail};
use clap::Args;
use std::process::ExitCode;
use std::time::Duration;
use trun_hub::Paths;
use trun_proto::{AlertLevel, FleetEvent, Health, Lifecycle, LogPage, RunSummary, now_ms};

#[derive(Args)]
pub struct WaitArgs {
    run: String,
    /// Conditions, comma-separated: done, succeeded, failed, stalled, failing,
    /// alert, lost, preempted
    #[arg(long, default_value = "done,stalled,failing")]
    until: String,
    /// Give up after this long (e.g. 30m, 2h); exit 4
    #[arg(long)]
    timeout: Option<String>,
    /// Print one line instead of the full digest
    #[arg(long)]
    quiet: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cond {
    Done,
    Succeeded,
    Failed,
    Stalled,
    Failing,
    Alert,
    Lost,
    Preempted,
}

fn parse_conds(s: &str) -> Result<Vec<Cond>> {
    s.split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| {
            Ok(match c {
                "done" => Cond::Done,
                "succeeded" => Cond::Succeeded,
                "failed" => Cond::Failed,
                "stalled" => Cond::Stalled,
                "failing" => Cond::Failing,
                "alert" => Cond::Alert,
                "lost" => Cond::Lost,
                "preempted" => Cond::Preempted,
                other => bail!("unknown condition '{other}' (done, succeeded, failed, stalled, failing, alert, lost, preempted)"),
            })
        })
        .collect()
}

/// If a condition holds, the exit code and a short reason.
fn check(r: &RunSummary, conds: &[Cond]) -> Option<(u8, String)> {
    let has = |c: Cond| conds.contains(&c);
    if r.lifecycle.is_terminal() {
        let code = match r.lifecycle {
            Lifecycle::Succeeded => 0,
            Lifecycle::Lost | Lifecycle::Preempted => 3,
            _ => 1,
        };
        let wanted = has(Cond::Done)
            || (has(Cond::Succeeded) && r.lifecycle == Lifecycle::Succeeded)
            || (has(Cond::Failed)
                && matches!(r.lifecycle, Lifecycle::Failed | Lifecycle::Cancelled))
            || (has(Cond::Lost) && r.lifecycle == Lifecycle::Lost)
            || (has(Cond::Preempted) && r.lifecycle == Lifecycle::Preempted);
        // A run that ended can't meet any other condition later: always return.
        return Some((
            code,
            if wanted {
                r.lifecycle.as_str().into()
            } else {
                format!("{} (ended)", r.lifecycle.as_str())
            },
        ));
    }
    if has(Cond::Failing) && r.health == Health::Failing {
        return Some((6, "health failing".into()));
    }
    if has(Cond::Stalled) && r.health == Health::Stalled {
        return Some((2, "stalled".into()));
    }
    if has(Cond::Alert) && r.alerts.iter().any(|a| a.level >= AlertLevel::Warn) {
        return Some((5, "alert".into()));
    }
    None
}

pub async fn wait(paths: &Paths, args: WaitArgs) -> Result<ExitCode> {
    let conds = parse_conds(&args.until)?;
    let timeout = match &args.timeout {
        Some(t) => Some(Duration::from_millis(
            trun_parse::parse_duration_ms(t)
                .ok_or_else(|| anyhow::anyhow!("bad --timeout '{t}'"))?
                .max(0) as u64,
        )),
        None => None,
    };
    let c = Client::connect(paths, false).await?;
    let first: RunSummary = c.get(&format!("/runs/{}", crate::enc(&args.run))).await?;
    let id = first.id.clone();

    let watch = async {
        loop {
            // Subscribe first, then read the current state, so nothing is missed.
            let mut sse = c.sse("/stream").await;
            let r: RunSummary = c.get(&format!("/runs/{id}")).await?;
            if let Some(hit) = check(&r, &conds) {
                return Ok::<_, anyhow::Error>((hit, r));
            }
            let Ok(sse) = sse.as_mut() else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            };
            // Re-read periodically as a safety net in case updates are missed.
            let recheck = tokio::time::sleep(Duration::from_secs(30));
            tokio::pin!(recheck);
            loop {
                tokio::select! {
                    msg = sse.next() => match msg {
                        Ok(Some(m)) if m.event == "run" => {
                            if let Ok(FleetEvent::Run(s)) = serde_json::from_str::<FleetEvent>(&m.data)
                                && s.id == id
                                && let Some(hit) = check(&s, &conds)
                            {
                                return Ok((hit, s));
                            }
                        }
                        Ok(Some(_)) => {}
                        _ => break, // disconnected: resubscribe
                    },
                    _ = &mut recheck => break,
                }
            }
        }
    };

    let ((code, reason), run) = match timeout {
        Some(t) => match tokio::time::timeout(t, watch).await {
            Ok(r) => r?,
            Err(_) => {
                let r: RunSummary = c.get(&format!("/runs/{id}")).await?;
                if args.quiet {
                    println!(
                        "{} {}: timeout after {}",
                        r.id,
                        r.name,
                        args.timeout.as_deref().unwrap_or("")
                    );
                } else {
                    println!(
                        "trun wait: timed out after {}",
                        args.timeout.as_deref().unwrap_or("")
                    );
                    let page: LogPage = c.get(&format!("/runs/{id}/logs?tail=10")).await?;
                    print!("{}", crate::digest::render(&r, &page.lines, now_ms()));
                }
                return Ok(ExitCode::from(4));
            }
        },
        None => watch.await?,
    };
    if args.quiet {
        println!("{} {}: {reason}", run.id, run.name);
    } else {
        println!("trun wait: {reason}");
        let page: LogPage = c.get(&format!("/runs/{id}/logs?tail=10")).await?;
        print!("{}", crate::digest::render(&run, &page.lines, now_ms()));
    }
    Ok(ExitCode::from(code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trun_proto::Alert;

    fn run(l: Lifecycle, h: Health) -> RunSummary {
        RunSummary {
            lifecycle: l,
            health: h,
            ..Default::default()
        }
    }

    #[test]
    fn conditions_and_exit_codes() {
        let d = parse_conds("done,stalled,failing").unwrap();
        assert_eq!(check(&run(Lifecycle::Running, Health::Ok), &d), None);
        assert_eq!(
            check(&run(Lifecycle::Running, Health::Stalled), &d)
                .unwrap()
                .0,
            2
        );
        assert_eq!(
            check(&run(Lifecycle::Running, Health::Failing), &d)
                .unwrap()
                .0,
            6
        );
        assert_eq!(
            check(&run(Lifecycle::Succeeded, Health::Ok), &d).unwrap().0,
            0
        );
        assert_eq!(check(&run(Lifecycle::Failed, Health::Ok), &d).unwrap().0, 1);
        assert_eq!(check(&run(Lifecycle::Lost, Health::Ok), &d).unwrap().0, 3);
        // Only waiting for stalls: a run that ends still returns (it can't stall later).
        let s = parse_conds("stalled").unwrap();
        assert_eq!(
            check(&run(Lifecycle::Succeeded, Health::Ok), &s).unwrap().1,
            "succeeded (ended)"
        );
        assert_eq!(check(&run(Lifecycle::Running, Health::Failing), &s), None);
        let a = parse_conds("alert").unwrap();
        let mut r = run(Lifecycle::Running, Health::Warn);
        r.alerts.push(Alert {
            key: "k".into(),
            check: "c".into(),
            level: AlertLevel::Warn,
            message: "m".into(),
            opened_at: 0,
            last_at: 0,
            count: 1,
        });
        assert_eq!(check(&r, &a).unwrap().0, 5);
        assert!(parse_conds("nope").is_err());
    }
}
