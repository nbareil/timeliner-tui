use crate::{
    model::{EventKey, Row, Tag, View},
    query::{self, Cancellation, Page},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Duration, LocalResult, NaiveDate, Offset, TimeZone, Timelike};
use chrono_tz::{GapInfo, Tz};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    TimeBoundary(i64),
    TaggedAfter(EventKey),
}

impl Jump {
    /// Find a destination without changing the displayed view or its folds.
    pub fn find(
        self,
        store: &Store,
        view: &View,
        cancel: &Cancellation,
        progress: impl FnMut(u64),
    ) -> Result<Option<Row>> {
        let mut view = view.clone();
        let (anchor, inclusive) = match self {
            Self::TimeBoundary(epoch) => (
                EventKey {
                    epoch,
                    record: i64::MIN,
                },
                true,
            ),
            Self::TaggedAfter(key) => {
                // Intersect with the current tag filter rather than broadening it.
                let tags = view.predicate.tags.get_or_insert_default();
                if !tags.is_empty() {
                    tags.retain(|tag| *tag != Tag::None);
                    if tags.is_empty() {
                        return Ok(None);
                    }
                }
                (key, false)
            }
        };
        query::page(
            store,
            &view,
            Page {
                anchor: Some(anchor),
                inclusive,
                limit: 1,
                ..Page::default()
            },
            cancel,
            progress,
        )
        .map(|mut rows| rows.pop())
    }
}

#[derive(Clone, Copy)]
pub enum TimeUnit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

impl TimeUnit {
    /// Find the next clock or calendar boundary, strictly after the given event.
    pub fn next_boundary(self, epoch: i64, tz: Tz) -> Result<i64> {
        let range_error = "Timestamp is outside the supported calendar range";
        let datetime = tz.timestamp_opt(epoch, 0).single().context(range_error)?;
        // Unlike naive_local(), checked arithmetic also handles extreme source timestamps.
        let local = datetime
            .naive_utc()
            .checked_add_signed(Duration::seconds(
                datetime.offset().fix().local_minus_utc().into(),
            ))
            .context(range_error)?;
        let seconds = match self {
            Self::Minute => Some(60),
            Self::Hour => Some(3600),
            _ => None,
        };
        if let Some(seconds) = seconds {
            // Advance in real time so repeated hours remain reachable during a
            // clock rollback. Recheck alignment for offsets changing by a partial hour.
            let mut datetime = datetime;
            loop {
                let elapsed = i64::from(datetime.num_seconds_from_midnight()) % seconds;
                datetime = datetime
                    .checked_add_signed(Duration::seconds(seconds - elapsed))
                    .context(range_error)?;
                if i64::from(datetime.num_seconds_from_midnight()) % seconds == 0 {
                    return Ok(datetime.timestamp());
                }
            }
        }
        let date = local.date();
        let next = match self {
            Self::Day => date.succ_opt(),
            Self::Month => {
                let (year, month) = if date.month() == 12 {
                    (date.year() + 1, 1)
                } else {
                    (date.year(), date.month() + 1)
                };
                NaiveDate::from_ymd_opt(year, month, 1)
            }
            Self::Year => NaiveDate::from_ymd_opt(date.year() + 1, 1, 1),
            Self::Minute | Self::Hour => unreachable!(),
        }
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .context(range_error)?;
        let target = match tz.from_local_datetime(&next) {
            LocalResult::Single(time) => Some(time),
            LocalResult::Ambiguous(first, second) => [first, second]
                .into_iter()
                .filter(|time| time.timestamp() > epoch)
                .min(),
            // Some zones skip midnight, or even an entire date.
            LocalResult::None => GapInfo::new(&next, &tz).and_then(|gap| gap.end),
        }
        .context(range_error)?
        .timestamp();
        ensure!(target > epoch, "No later calendar boundary is available");
        Ok(target)
    }
}
