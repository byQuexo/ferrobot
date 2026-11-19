use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::StoreError;

pub(crate) fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub(crate) fn fmt_ts(ts: OffsetDateTime) -> String {
    // RFC 3339 formatting of a UTC timestamp cannot fail.
    ts.format(&Rfc3339).unwrap_or_default()
}

pub(crate) fn parse_ts(raw: &str) -> Result<OffsetDateTime, StoreError> {
    OffsetDateTime::parse(raw, &Rfc3339).map_err(StoreError::corrupt)
}

/// True if the error is a UNIQUE constraint violation.
pub(crate) fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}
