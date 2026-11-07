//! Settings repository: a small key/value table for app preferences that
//! are not secrets (media folders, UI toggles). Secrets never go here —
//! webhook URLs live in the OS credential store (RULES.md §4).

use std::collections::HashMap;

use rusqlite::params;

use crate::{lock, Conn, StoreError};

/// Repository for the `settings` table.
#[derive(Clone)]
pub struct SettingsRepo {
    pub(crate) conn: Conn,
}

impl SettingsRepo {
    pub(crate) fn new(conn: Conn) -> Self {
        Self { conn }
    }

    /// Read one setting; `None` when the key was never written.
    pub fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(StoreError::Sql(other)),
        })
    }

    /// Write one setting, replacing any previous value.
    pub fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every setting as a map (used to hydrate `AppState` at startup).
    pub fn all(&self) -> Result<HashMap<String, String>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[test]
    fn get_set_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let repo = store.settings();
        assert_eq!(repo.get("missing").unwrap(), None);
        repo.set("k", "v1").unwrap();
        assert_eq!(repo.get("k").unwrap().as_deref(), Some("v1"));
        repo.set("k", "v2").unwrap();
        assert_eq!(repo.get("k").unwrap().as_deref(), Some("v2"));
        assert_eq!(repo.all().unwrap().get("k").map(String::as_str), Some("v2"));
    }
}
