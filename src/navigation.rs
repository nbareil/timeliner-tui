use crate::{
    model::{EventKey, Row, Tag, View},
    query::{self, Cancellation, Page},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike,
};
use chrono_tz::{GapInfo, Tz};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    TimeBoundary(i64),
    PreviousTimeBoundary(i64),
    TaggedAfter(EventKey),
    TaggedBefore(EventKey),
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
        let reverse = matches!(self, Self::PreviousTimeBoundary(_) | Self::TaggedBefore(_));
        let (anchor, inclusive) = match self {
            Self::TimeBoundary(epoch) | Self::PreviousTimeBoundary(epoch) => (
                EventKey {
                    epoch,
                    record: if reverse { i64::MAX } else { i64::MIN },
                },
                true,
            ),
            Self::TaggedAfter(key) | Self::TaggedBefore(key) => {
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
                reverse,
                inclusive,
                limit: 1,
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
        let datetime = tz.timestamp_opt(epoch, 0).single().context(RANGE_ERROR)?;
        let local = checked_local(datetime)?;
        let seconds = match self {
            Self::Minute => Some(60),
            Self::Hour => Some(3600),
            _ => None,
        };
        if let Some(seconds) = seconds {
            return clock_boundary(datetime, seconds, false);
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
        .context(RANGE_ERROR)?;
        let target = match tz.from_local_datetime(&next) {
            LocalResult::Single(time) => Some(time),
            LocalResult::Ambiguous(first, second) => [first, second]
                .into_iter()
                .filter(|time| time.timestamp() > epoch)
                .min(),
            // Some zones skip midnight, or even an entire date.
            LocalResult::None => GapInfo::new(&next, &tz).and_then(|gap| gap.end),
        }
        .context(RANGE_ERROR)?
        .timestamp();
        ensure!(target > epoch, "No later calendar boundary is available");
        Ok(target)
    }

    /// Find the most recent clock or calendar boundary strictly before an event.
    pub fn previous_boundary(self, epoch: i64, tz: Tz) -> Result<i64> {
        let datetime = tz.timestamp_opt(epoch, 0).single().context(RANGE_ERROR)?;
        let local = checked_local(datetime)?;
        match self {
            Self::Minute => return clock_boundary(datetime, 60, true),
            Self::Hour => return clock_boundary(datetime, 3600, true),
            _ => {}
        }
        let mut date = match self {
            Self::Day => Some(local.date()),
            Self::Month => NaiveDate::from_ymd_opt(local.year(), local.month(), 1),
            Self::Year => NaiveDate::from_ymd_opt(local.year(), 1, 1),
            Self::Minute | Self::Hour => unreachable!(),
        }
        .context(RANGE_ERROR)?;
        loop {
            let midnight = date.and_hms_opt(0, 0, 0).context(RANGE_ERROR)?;
            let candidates = match tz.from_local_datetime(&midnight) {
                LocalResult::Single(time) => [Some(time), None],
                LocalResult::Ambiguous(first, second) => [Some(first), Some(second)],
                LocalResult::None => [GapInfo::new(&midnight, &tz).and_then(|gap| gap.end), None],
            };
            if let Some(target) = candidates
                .into_iter()
                .flatten()
                .map(|time| time.timestamp())
                .filter(|time| *time < epoch)
                .max()
            {
                return Ok(target);
            }
            // An event exactly on a boundary must keep moving backward. A zone
            // that skipped a date can require stepping past more than one midnight.
            date = match self {
                Self::Day => date.pred_opt(),
                Self::Month => {
                    let (year, month) = if date.month() == 1 {
                        (date.year() - 1, 12)
                    } else {
                        (date.year(), date.month() - 1)
                    };
                    NaiveDate::from_ymd_opt(year, month, 1)
                }
                Self::Year => NaiveDate::from_ymd_opt(date.year() - 1, 1, 1),
                Self::Minute | Self::Hour => unreachable!(),
            }
            .context(RANGE_ERROR)?;
        }
    }
}

const RANGE_ERROR: &str = "Timestamp is outside the supported calendar range";

fn checked_local(datetime: DateTime<Tz>) -> Result<NaiveDateTime> {
    // Unlike naive_local(), checked arithmetic also handles extreme source timestamps.
    datetime
        .naive_utc()
        .checked_add_signed(Duration::seconds(
            datetime.offset().fix().local_minus_utc().into(),
        ))
        .context(RANGE_ERROR)
}

fn clock_boundary(mut datetime: DateTime<Tz>, seconds: i64, reverse: bool) -> Result<i64> {
    // Move in real time so repeated hours remain reachable during a clock rollback.
    // Recheck alignment for offsets changing by a partial hour.
    loop {
        let elapsed = i64::from(checked_local(datetime)?.num_seconds_from_midnight()) % seconds;
        let delta = if reverse {
            -if elapsed == 0 { seconds } else { elapsed }
        } else {
            seconds - elapsed
        };
        datetime = datetime
            .checked_add_signed(Duration::seconds(delta))
            .context(RANGE_ERROR)?;
        if i64::from(checked_local(datetime)?.num_seconds_from_midnight()) % seconds == 0 {
            return Ok(datetime.timestamp());
        }
    }
}
