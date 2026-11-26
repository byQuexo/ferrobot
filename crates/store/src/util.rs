use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::StoreError;

pub(crate) fn now() -> OffsetDateTime {
    to_micros(OffsetDateTime::now_utc())
}

/// Drops what `fmt_ts` cannot store, so a row built in memory equals the one read back.
fn to_micros(ts: OffsetDateTime) -> OffsetDateTime {
    ts.replace_nanosecond(ts.microsecond() * 1_000)
        .unwrap_or(ts)
}

/// Fixed-width UTC RFC 3339, so SQLite's text comparison orders timestamps correctly.
pub(crate) fn fmt_ts(ts: OffsetDateTime) -> String {
    let ts = ts.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        ts.year(),
        u8::from(ts.month()),
        ts.day(),
        ts.hour(),
        ts.minute(),
        ts.second(),
        ts.microsecond()
    )
}

pub(crate) fn parse_ts(raw: &str) -> Result<OffsetDateTime, StoreError> {
    OffsetDateTime::parse(raw, &Rfc3339).map_err(StoreError::corrupt)
}

pub(crate) fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

pub(crate) fn is_fk_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    )
}

/// A unique-constraint violation as a [`StoreError::Conflict`] on `key`; other errors as they are.
pub(crate) fn conflict_or(e: rusqlite::Error, entity: &'static str, key: &str) -> StoreError {
    if is_unique_violation(&e) {
        StoreError::Conflict {
            entity,
            key: key.to_owned(),
        }
    } else {
        e.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_sort_as_text_and_round_trip() {
        let whole = OffsetDateTime::from_unix_timestamp(1_795_694_400).unwrap();
        let later = whole.replace_millisecond(500).unwrap();
        let (a, b) = (fmt_ts(whole), fmt_ts(later));
        assert_eq!(a, "2026-11-26T12:00:00.000000Z");
        assert!(a < b, "{a} must sort before {b}");
        assert_eq!(parse_ts(&b).unwrap(), later);
        let elsewhere = whole.to_offset(UtcOffset::from_hms(1, 0, 0).unwrap());
        assert_eq!(fmt_ts(elsewhere), a);
    }

    #[test]
    fn now_keeps_only_what_is_stored() {
        let ns = OffsetDateTime::from_unix_timestamp(1_795_694_400)
            .unwrap()
            .replace_nanosecond(123_456_789)
            .unwrap();
        let stored = to_micros(ns);
        assert_eq!(stored.nanosecond(), 123_456_000);
        assert_eq!(parse_ts(&fmt_ts(ns)).unwrap(), stored);
        assert_eq!(now().nanosecond() % 1_000, 0);
    }
}
