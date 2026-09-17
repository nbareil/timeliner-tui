//! Read-only saved-view benchmark. Usage:
//! cargo run --example exclusion_experiment -- WORKSPACE SESSION_JSON viewport
//! Compare with the same command plus --features sqlite-exclusions.
#[path = "../src/lib.rs"]
mod application;
pub use application::*;
#[path = "support/exclusion_rates.rs"]
mod exclusion_rates;

use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};

fn digest(value: impl std::fmt::Debug) -> String {
    format!("{:x}", Sha256::digest(format!("{value:?}")))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 4,
        "Usage: exclusion_experiment WORKSPACE SESSION_JSON viewport|forward|reverse|rates"
    );
    let mut session: model::Session = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    ensure!(
        session.views[session.active].predicate.tags.is_none(),
        "This read-only experiment does not build tag caches"
    );
    let started = Instant::now();
    let store = store::Store::reader(Path::new(&args[1]))?;
    store.conn.execute_batch("PRAGMA query_only=ON; BEGIN;")?;
    let revision = store.revision()?;
    if args[3] == "rates" {
        return exclusion_rates::run(&store, &session);
    }
    let cancel = query::Cancellation::never();
    let mut restored = 0;
    for view in &mut session.views {
        let mut count = 0;
        burst::restore(&store, view, &cancel, |n| count = n)?;
        restored += count;
    }
    let restore_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mode = args[3].as_str();
    let mut checked = 0;
    let mut deliveries = 0;
    let mut initial_ms = None;
    let mut trace = Vec::new();
    let (rows, selection) = if mode == "viewport" {
        let mut app = ui::App::default();
        app.height = 50;
        let mut actions = app.message(worker::Message::Ready(session, String::new()));
        for _ in 0..200 {
            if actions.is_empty() {
                break;
            }
            ensure!(actions.len() == 1, "Unexpected UI actions");
            let ui::Action::Page(request) = actions.pop().unwrap() else {
                bail!("Unexpected non-query action");
            };
            let mut count = 0;
            let batch = burst::page_bounded(
                &store,
                &request.view,
                query::Page {
                    anchor: request.anchor,
                    reverse: request.reverse,
                    inclusive: request.inclusive,
                    limit: request.limit,
                },
                &cancel,
                |n| count = n,
            )?;
            checked += count;
            deliveries += 1;
            initial_ms.get_or_insert(started.elapsed().as_secs_f64() * 1000.0);
            trace.push((count, digest(&batch.rows), batch.continuation));
            actions = app.message(if batch.continuation.is_some() {
                worker::Message::PartialPage(request.generation, batch.rows)
            } else {
                worker::Message::Page(request.generation, batch.rows)
            });
        }
        ensure!(actions.is_empty(), "Viewport did not finish filling");
        ensure!(
            app.rows.len() - app.top >= app.height,
            "Incomplete viewport"
        );
        (
            app.rows[app.top..app.top + app.height].to_vec(),
            app.session.views[app.session.active].selected,
        )
    } else {
        ensure!(matches!(mode, "forward" | "reverse"), "Unknown mode");
        let view = &session.views[session.active];
        let rows = burst::page(
            &store,
            view,
            query::Page {
                anchor: view.top,
                reverse: mode == "reverse",
                inclusive: true,
                limit: 50,
            },
            &cancel,
            |n| checked = n,
        )?;
        deliveries = 1;
        (rows, view.selected)
    };
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{}",
        serde_json::json!({
            "mode": mode,
            "elapsed_ms": elapsed_ms,
            "restore_ms": restore_ms,
            "initial_ms": initial_ms,
            "candidates": checked,
            "restored_candidates": restored,
            "deliveries": deliveries,
            "rows": rows.len(),
            "result_digest": digest((&rows, selection)),
            "trace_digest": digest(trace),
            "revision": revision,
            "sqlite": rusqlite::version(),
        })
    );
    Ok(())
}
