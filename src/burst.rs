//! User-requested folds over consecutive dense seconds in the filtered timeline.
//! Detection streams records; display pagination retains only fold descriptors.
use crate::{
    model::{Burst, DisplayRow, EventKey, Tag, TagChange, View, validate_view},
    query::{self, Cancellation, Matcher, Page, ResetCancellation},
    store::Store,
};
use anyhow::Result;
use rusqlite::params;

/// Assessments do not change timestamp density in views without a tag filter.
/// Repair summary counts there, and invalidate only folds whose own membership
/// changes in tag-filtered views.
pub fn apply_tag_change(view: &mut View, change: &TagChange) -> Result<()> {
    if view.bursts.is_empty() {
        return Ok(());
    }
    let matcher = Matcher::new(&view.predicate)?;
    let before = matcher.matches(&change.path, &change.host, change.before);
    let after = matcher.matches(&change.path, &change.host, change.after);
    if !before && !after {
        return Ok(());
    }
    view.bursts.retain_mut(|burst| {
        let affected = change
            .events
            .iter()
            .filter(|(key, macb)| {
                key.epoch >= burst.first.epoch
                    && key.epoch <= burst.last.epoch
                    && macb & view.time_mask != 0
            })
            .count() as u64;
        if affected == 0 {
            return true;
        }
        if burst.host != change.host {
            if !before && after {
                // A formerly filtered-out host is now visible in this interval.
                // Seeking past it would hide its newly matching events.
                burst.exclusive = false;
            }
            return true;
        }
        if before != after {
            return false;
        }
        if change.before == Tag::None && change.after != Tag::None {
            burst.tagged += affected;
        } else if change.before != Tag::None && change.after == Tag::None {
            burst.tagged = burst.tagged.saturating_sub(affected);
        }
        true
    });
    Ok(())
}

/// Find the maximal consecutive run of seconds with strictly more than
/// `threshold` matching events on `host`. A missing/sparse second ends the run.
pub fn detect(
    store: &Store,
    view: &View,
    origin: EventKey,
    host: &str,
    threshold: u64,
    cancel: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<Option<Burst>> {
    validate_view(view)?;
    cancel.check()?;
    cancel.install(&store.conn)?;
    let _reset = ResetCancellation(&store.conn);
    let matcher = Matcher::new(&view.predicate)?;
    let lo = view
        .after
        .unwrap_or(i64::MIN)
        .max(if view.bogus { i64::MIN } else { 1 });
    let hi = view.before.unwrap_or(i64::MAX);
    let mut statement = store.conn.prepare_cached(
        "SELECT e.record,e.macb,r.host,r.path,r.tag,r.matches!='[]'
         FROM events e JOIN records r ON r.id=e.record
         WHERE e.epoch=? AND (e.macb & ?) != 0 ORDER BY e.record",
    )?;
    let mut checked = 0_u64;
    let mut found_origin = false;
    let mut second = |epoch: i64| -> Result<Option<Burst>> {
        cancel.check()?;
        if epoch < lo || epoch > hi {
            return Ok(None);
        }
        let mut burst = Burst {
            first: EventKey {
                epoch,
                record: i64::MAX,
            },
            last: EventKey {
                epoch,
                record: i64::MIN,
            },
            host: host.into(),
            count: 0,
            macb: 0,
            tagged: 0,
            ioc_matches: 0,
            exclusive: true,
            origin,
        };
        let mut rows = statement.query(params![epoch, view.time_mask])?;
        while let Some(row) = rows.next()? {
            cancel.check()?;
            checked += 1;
            if checked.is_multiple_of(1024) {
                progress(checked);
            }
            let row_host: String = row.get(2)?;
            let path: String = row.get(3)?;
            let tag = Tag::from_i64(row.get(4)?);
            if !matcher.matches(&path, &row_host, tag) {
                continue;
            }
            if row_host != host {
                burst.exclusive = false;
                continue;
            }
            let key = EventKey {
                epoch,
                record: row.get(0)?,
            };
            found_origin |= key == origin;
            burst.first = burst.first.min(key);
            burst.last = key;
            burst.count += 1;
            burst.macb |= row.get::<_, u8>(1)?;
            burst.tagged += u64::from(tag != Tag::None);
            burst.ioc_matches += u64::from(row.get::<_, bool>(5)?);
        }
        Ok((burst.count > threshold && burst.count > 0).then_some(burst))
    };
    let Some(mut burst) = second(origin.epoch)? else {
        return Ok(None);
    };
    for reverse in [true, false] {
        loop {
            let next = if reverse {
                burst.first.epoch.checked_sub(1)
            } else {
                burst.last.epoch.checked_add(1)
            };
            let Some(epoch) = next else { break };
            let Some(adjacent) = second(epoch)? else {
                break;
            };
            burst.first = burst.first.min(adjacent.first);
            burst.last = burst.last.max(adjacent.last);
            burst.count += adjacent.count;
            burst.macb |= adjacent.macb;
            burst.tagged += adjacent.tagged;
            burst.ioc_matches += adjacent.ioc_matches;
            burst.exclusive &= adjacent.exclusive;
        }
    }
    progress(checked);
    cancel.check()?;
    Ok(found_origin.then_some(burst))
}

/// Merge summary rows into the remaining events in their original key order.
/// The summary's key is its first actual event, usable in either direction.
pub fn page(
    store: &Store,
    view: &View,
    page: Page,
    cancel: &Cancellation,
    progress: impl FnMut(u64),
) -> Result<Vec<DisplayRow>> {
    let mut rows: Vec<_> = query::page_omitting(store, view, page, &view.bursts, cancel, progress)?
        .into_iter()
        .map(DisplayRow::Event)
        .collect();
    for burst in &view.bursts {
        let key = burst.first;
        let within_page = page.anchor.is_none_or(|anchor| {
            if page.reverse {
                key < anchor || (page.inclusive && key == anchor)
            } else {
                key > anchor || (page.inclusive && key == anchor)
            }
        });
        if within_page {
            rows.push(DisplayRow::Burst(burst.clone()));
        }
    }
    rows.sort_by_key(DisplayRow::key);
    if page.reverse {
        rows.reverse();
    }
    rows.truncate(page.limit.min(4096));
    if page.reverse {
        rows.reverse();
    }
    cancel.check()?;
    Ok(rows)
}

/// Keep earlier display rows available when replacing a page around a fold.
/// The context is included in the page limit, and the focused row remains in
/// the forward page. Both queries finish before the UI adopts the new view.
pub fn page_with_context(
    store: &Store,
    view: &View,
    mut request: Page,
    context_before: usize,
    cancel: &Cancellation,
    mut progress: impl FnMut(u64),
) -> Result<Vec<DisplayRow>> {
    let context_before = context_before.min(request.limit.min(4096).saturating_sub(1));
    if context_before > 0 && request.anchor.is_some() && !request.reverse && request.inclusive {
        let previous = page(
            store,
            view,
            Page {
                anchor: request.anchor,
                reverse: true,
                inclusive: false,
                limit: context_before,
            },
            cancel,
            &mut progress,
        )?;
        if let Some(first) = previous.first() {
            request.anchor = Some(first.key());
        }
    }
    page(store, view, request, cancel, progress)
}
