//! Experimental SQL-side path exclusions. Keep scan checkpoints visible so
//! progress, partial pages, and cursor seeks over saved folds retain their
//! existing behavior even when SQLite rejects most candidates.
use super::{Exclusions, Page};
use crate::model::{Burst, EventKey};
use anyhow::Result;
use rusqlite::{Connection, functions::FunctionFlags};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const FUNCTION: &str = "timeliner_scan_candidate";

#[derive(Default)]
struct State {
    checked: AtomicU64,
    excluded: AtomicBool,
    has_rows: AtomicBool,
}

pub(super) struct Scan<'a> {
    db: &'a Connection,
    state: Arc<State>,
}

impl<'a> Scan<'a> {
    pub(super) fn install(
        db: &'a Connection,
        excludes: Exclusions,
        exclusive_bursts: Vec<Burst>,
        page: Page,
        first_summary: Option<EventKey>,
        scan_budget: Option<u64>,
    ) -> Result<Self> {
        let state = Arc::new(State::default());
        let shared = state.clone();
        db.create_scalar_function(
            FUNCTION,
            3,
            // This is deliberately not deterministic: it emits checkpoints as
            // well as matches. It is used only by our streaming SELECT.
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
            move |ctx| {
                let key = EventKey {
                    epoch: ctx.get(0)?,
                    record: ctx.get(1)?,
                };
                if exclusive_bursts.iter().any(|burst| burst.contains_key(key)) {
                    // Let the outer loop seek across this entire fold without
                    // counting its hidden candidates or evaluating their paths.
                    return Ok(true);
                }
                let path = ctx
                    .get_raw(2)
                    .as_str()
                    .map_err(|e| rusqlite::Error::UserFunctionError(e.into()))?;
                let checked = shared.checked.load(Ordering::Relaxed) + 1;
                shared.checked.store(checked, Ordering::Relaxed);
                let excluded = excludes.is_match(path);
                shared.excluded.store(excluded, Ordering::Relaxed);
                let budget_reached = scan_budget.is_some_and(|budget| checked >= budget)
                    && (shared.has_rows.load(Ordering::Relaxed)
                        || first_summary.is_some_and(|summary| {
                            if page.reverse {
                                summary >= key
                            } else {
                                summary <= key
                            }
                        }));
                Ok(!excluded || checked.is_multiple_of(1024) || budget_reached)
            },
        )?;
        Ok(Self { db, state })
    }

    pub(super) fn checked(&self) -> u64 {
        self.state.checked.load(Ordering::Relaxed)
    }

    pub(super) fn included(&self) -> bool {
        // The cursor query streams directly, with no sorting or buffering.
        // sqlite3_step stops at the returned row, so this is that row's result.
        !self.state.excluded.load(Ordering::Relaxed)
    }

    pub(super) fn found_row(&self) {
        self.state.has_rows.store(true, Ordering::Relaxed);
    }
}

impl Drop for Scan<'_> {
    fn drop(&mut self) {
        // A retained reader must not retain a previous view's matcher or scan
        // state. Statements created after this guard have already been dropped.
        let _ = self.db.remove_function(FUNCTION, 3);
    }
}
