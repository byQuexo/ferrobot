//! Daemon-wide settings, one JSON value per key. Never holds secrets.

use rusqlite::{OptionalExtension, params};

use crate::util::{fmt_ts, now};
use crate::{Store, StoreError};

impl Store {
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        let key = key.to_owned();
        self.with(move |c| {
            Ok(
                c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()?,
            )
        })
        .await
    }

    pub async fn put_setting(&self, key: &str, value: String) -> Result<(), StoreError> {
        let key = key.to_owned();
        self.with(move |c| {
            c.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value,
                                                 updated_at = excluded.updated_at",
                params![key, value, fmt_ts(now())],
            )?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[tokio::test]
    async fn settings_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_setting("voice").await.unwrap(), None);
        store
            .put_setting("voice", "{\"a\":1}".into())
            .await
            .unwrap();
        store
            .put_setting("voice", "{\"a\":2}".into())
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("voice").await.unwrap().as_deref(),
            Some("{\"a\":2}")
        );
    }
}
