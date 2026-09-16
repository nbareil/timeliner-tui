use chrono::{DateTime, Utc};
use timeliner_tui::navigation::TimeUnit;

fn epoch(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text).unwrap().timestamp()
}

fn check(unit: TimeUnit, timezone: &str, from: &str, expected: &str) {
    assert_eq!(
        unit.next_boundary(epoch(from), timezone.parse().unwrap())
            .unwrap(),
        epoch(expected),
        "{timezone}: {from} → {expected}"
    );
}

#[test]
fn time_jumps_use_strict_clock_and_calendar_boundaries() {
    for (unit, expected) in [
        (TimeUnit::Minute, "2024-05-12T10:26:00Z"),
        (TimeUnit::Hour, "2024-05-12T11:00:00Z"),
        (TimeUnit::Day, "2024-05-13T00:00:00Z"),
        (TimeUnit::Month, "2024-06-01T00:00:00Z"),
        (TimeUnit::Year, "2025-01-01T00:00:00Z"),
    ] {
        check(unit, "UTC", "2024-05-12T10:25:40Z", expected);
        assert!(
            unit.next_boundary(epoch(expected), chrono_tz::UTC).unwrap() > epoch(expected),
            "An event on a boundary must advance to the next one"
        );
    }
}

#[test]
fn calendar_jumps_handle_leap_days_and_month_and_year_rollovers() {
    for (unit, from, expected) in [
        (
            TimeUnit::Day,
            "2024-02-28T12:00:00Z",
            "2024-02-29T00:00:00Z",
        ),
        (
            TimeUnit::Day,
            "2023-02-28T12:00:00Z",
            "2023-03-01T00:00:00Z",
        ),
        (
            TimeUnit::Month,
            "2024-01-31T23:59:59Z",
            "2024-02-01T00:00:00Z",
        ),
        (
            TimeUnit::Month,
            "2024-02-29T12:00:00Z",
            "2024-03-01T00:00:00Z",
        ),
        (
            TimeUnit::Year,
            "2024-02-29T12:00:00Z",
            "2025-01-01T00:00:00Z",
        ),
    ] {
        check(unit, "UTC", from, expected);
    }
    for unit in [
        TimeUnit::Minute,
        TimeUnit::Hour,
        TimeUnit::Day,
        TimeUnit::Month,
        TimeUnit::Year,
    ] {
        check(unit, "UTC", "2024-12-31T23:59:59Z", "2025-01-01T00:00:00Z");
    }
}

#[test]
fn jumps_use_local_time_including_fractional_offsets_and_dst() {
    for (unit, expected) in [
        (TimeUnit::Minute, "2024-05-12T23:51:00+05:45"),
        (TimeUnit::Hour, "2024-05-13T00:00:00+05:45"),
        (TimeUnit::Day, "2024-05-13T00:00:00+05:45"),
        (TimeUnit::Month, "2024-06-01T00:00:00+05:45"),
        (TimeUnit::Year, "2025-01-01T00:00:00+05:45"),
    ] {
        check(
            unit,
            "Asia/Kathmandu",
            "2024-05-12T23:50:40+05:45",
            expected,
        );
    }
    for (unit, from, expected) in [
        (
            TimeUnit::Day,
            "2024-03-10T00:00:00-05:00",
            "2024-03-11T00:00:00-04:00",
        ),
        (
            TimeUnit::Day,
            "2024-11-03T00:00:00-04:00",
            "2024-11-04T00:00:00-05:00",
        ),
        (
            TimeUnit::Hour,
            "2024-03-10T01:30:00-05:00",
            "2024-03-10T03:00:00-04:00",
        ),
        (
            TimeUnit::Minute,
            "2024-03-10T01:59:50-05:00",
            "2024-03-10T03:00:00-04:00",
        ),
        (
            TimeUnit::Hour,
            "2024-11-03T01:30:00-04:00",
            "2024-11-03T01:00:00-05:00",
        ),
        (
            TimeUnit::Hour,
            "2024-11-03T01:30:00-05:00",
            "2024-11-03T02:00:00-05:00",
        ),
        (
            TimeUnit::Minute,
            "2024-11-03T01:59:50-04:00",
            "2024-11-03T01:00:00-05:00",
        ),
    ] {
        check(unit, "America/New_York", from, expected);
    }
    check(
        TimeUnit::Hour,
        "Australia/Lord_Howe",
        "2024-04-07T01:45:00+11:00",
        "2024-04-07T02:00:00+10:30",
    );
}

#[test]
fn calendar_jumps_resolve_missing_and_repeated_midnights() {
    check(
        TimeUnit::Day,
        "America/Sao_Paulo",
        "2018-11-03T12:00:00-03:00",
        "2018-11-04T01:00:00-02:00",
    );
    check(
        TimeUnit::Day,
        "Pacific/Apia",
        "2011-12-29T12:00:00-10:00",
        "2011-12-31T00:00:00+14:00",
    );
    check(
        TimeUnit::Day,
        "America/Havana",
        "2024-11-02T12:00:00-04:00",
        "2024-11-03T00:00:00-04:00",
    );
}

#[test]
fn calendar_limits_and_invalid_evidence_timestamps_return_errors() {
    for unit in [
        TimeUnit::Minute,
        TimeUnit::Hour,
        TimeUnit::Day,
        TimeUnit::Month,
        TimeUnit::Year,
    ] {
        for timestamp in [i64::MIN, i64::MAX, DateTime::<Utc>::MAX_UTC.timestamp()] {
            assert!(unit.next_boundary(timestamp, chrono_tz::UTC).is_err());
        }
        assert!(
            unit.next_boundary(DateTime::<Utc>::MAX_UTC.timestamp(), chrono_tz::Asia::Tokyo,)
                .is_err()
        );
    }
}

fn check_back(unit: TimeUnit, timezone: &str, from: &str, expected: &str) {
    assert_eq!(
        unit.previous_boundary(epoch(from), timezone.parse().unwrap())
            .unwrap(),
        epoch(expected),
        "{timezone}: {from} → {expected}"
    );
}

#[test]
fn backward_jumps_find_the_preceding_boundary_and_advance_again_on_exact_boundaries() {
    for (unit, expected, previous) in [
        (
            TimeUnit::Minute,
            "2024-05-12T10:25:00Z",
            "2024-05-12T10:24:00Z",
        ),
        (
            TimeUnit::Hour,
            "2024-05-12T10:00:00Z",
            "2024-05-12T09:00:00Z",
        ),
        (
            TimeUnit::Day,
            "2024-05-12T00:00:00Z",
            "2024-05-11T00:00:00Z",
        ),
        (
            TimeUnit::Month,
            "2024-05-01T00:00:00Z",
            "2024-04-01T00:00:00Z",
        ),
        (
            TimeUnit::Year,
            "2024-01-01T00:00:00Z",
            "2023-01-01T00:00:00Z",
        ),
    ] {
        check_back(unit, "UTC", "2024-05-12T10:25:40Z", expected);
        check_back(unit, "UTC", expected, previous);
    }
}

#[test]
fn backward_calendar_jumps_handle_rollovers_and_fractional_timezones() {
    for (unit, from, expected) in [
        (
            TimeUnit::Day,
            "2024-03-01T00:00:00Z",
            "2024-02-29T00:00:00Z",
        ),
        (
            TimeUnit::Day,
            "2023-03-01T00:00:00Z",
            "2023-02-28T00:00:00Z",
        ),
        (
            TimeUnit::Month,
            "2024-01-01T00:00:00Z",
            "2023-12-01T00:00:00Z",
        ),
        (
            TimeUnit::Year,
            "2024-02-29T12:00:00Z",
            "2024-01-01T00:00:00Z",
        ),
    ] {
        check_back(unit, "UTC", from, expected);
    }
    for (unit, expected) in [
        (TimeUnit::Minute, "2024-05-12T23:50:00+05:45"),
        (TimeUnit::Hour, "2024-05-12T23:00:00+05:45"),
        (TimeUnit::Day, "2024-05-12T00:00:00+05:45"),
        (TimeUnit::Month, "2024-05-01T00:00:00+05:45"),
        (TimeUnit::Year, "2024-01-01T00:00:00+05:45"),
    ] {
        check_back(
            unit,
            "Asia/Kathmandu",
            "2024-05-12T23:50:40+05:45",
            expected,
        );
    }
}

#[test]
fn backward_jumps_follow_dst_rollbacks_and_skipped_midnights() {
    for (unit, from, expected) in [
        (
            TimeUnit::Hour,
            "2024-03-10T03:00:00-04:00",
            "2024-03-10T01:00:00-05:00",
        ),
        (
            TimeUnit::Minute,
            "2024-03-10T03:00:00-04:00",
            "2024-03-10T01:59:00-05:00",
        ),
        (
            TimeUnit::Hour,
            "2024-11-03T01:30:00-05:00",
            "2024-11-03T01:00:00-05:00",
        ),
        (
            TimeUnit::Hour,
            "2024-11-03T01:00:00-05:00",
            "2024-11-03T01:00:00-04:00",
        ),
        (
            TimeUnit::Day,
            "2024-03-11T00:00:00-04:00",
            "2024-03-10T00:00:00-05:00",
        ),
    ] {
        check_back(unit, "America/New_York", from, expected);
    }
    for (timezone, from, expected) in [
        (
            "America/Havana",
            "2024-11-03T00:00:00-05:00",
            "2024-11-03T00:00:00-04:00",
        ),
        (
            "America/Sao_Paulo",
            "2018-11-04T01:00:00-02:00",
            "2018-11-03T00:00:00-03:00",
        ),
        (
            "America/Sao_Paulo",
            "2018-11-04T01:30:00-02:00",
            "2018-11-04T01:00:00-02:00",
        ),
        (
            "Pacific/Apia",
            "2011-12-31T00:00:00+14:00",
            "2011-12-29T00:00:00-10:00",
        ),
    ] {
        check_back(TimeUnit::Day, timezone, from, expected);
    }
    check_back(
        TimeUnit::Hour,
        "Australia/Lord_Howe",
        "2024-04-07T02:00:00+10:30",
        "2024-04-07T01:00:00+11:00",
    );
}

#[test]
fn backward_jumps_reject_timestamps_before_the_calendar_range() {
    for unit in [
        TimeUnit::Minute,
        TimeUnit::Hour,
        TimeUnit::Day,
        TimeUnit::Month,
        TimeUnit::Year,
    ] {
        for timestamp in [i64::MIN, i64::MAX, DateTime::<Utc>::MIN_UTC.timestamp()] {
            assert!(unit.previous_boundary(timestamp, chrono_tz::UTC).is_err());
        }
        assert!(
            unit.previous_boundary(
                DateTime::<Utc>::MIN_UTC.timestamp(),
                chrono_tz::America::New_York
            )
            .is_err()
        );
    }
}
