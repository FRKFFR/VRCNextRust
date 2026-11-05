//! Persistence for VRChat auth cookies (RULES.md §4).
//!
//! Only the auth cookie *values* are persisted — never passwords, e-mail
//! addresses or 2FA codes. [`KeyringVault`] stores them in the OS credential
//! store; [`MemoryVault`] keeps them in process memory for tests.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::ApiError;

/// Where auth cookies are stored between sessions.
pub trait CookieVault: Send + Sync {
    /// Return the persisted cookie header (`auth=..; twoFactorAuth=..`), or
    /// `None` when nothing is stored.
    fn load(&self) -> Result<Option<String>, ApiError>;
    /// Persist the cookie header.
    fn save(&self, cookies: &str) -> Result<(), ApiError>;
    /// Drop any persisted cookies.
    fn clear(&self) -> Result<(), ApiError>;
}

/// Delegates to the shared vault so callers can keep their own handle.
impl<T: CookieVault + ?Sized> CookieVault for Arc<T> {
    fn load(&self) -> Result<Option<String>, ApiError> {
        (**self).load()
    }

    fn save(&self, cookies: &str) -> Result<(), ApiError> {
        (**self).save(cookies)
    }

    fn clear(&self) -> Result<(), ApiError> {
        (**self).clear()
    }
}

const KEYRING_SERVICE: &str = "VRCNextRust";
const KEYRING_ACCOUNT: &str = "vrchat-auth-cookies";

/// [`CookieVault`] backed by the OS credential store (`keyring` crate).
pub struct KeyringVault {
    entry: keyring::Entry,
}

impl KeyringVault {
    /// Open (or create) the credential-store entry. Fails when no credential
    /// store is available on this platform.
    pub fn new() -> Result<Self, ApiError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
            .map_err(|e| ApiError::Vault(e.to_string()))?;
        Ok(Self { entry })
    }
}

impl fmt::Debug for KeyringVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyringVault(..)")
    }
}

impl CookieVault for KeyringVault {
    fn load(&self) -> Result<Option<String>, ApiError> {
        match self.entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ApiError::Vault(e.to_string())),
        }
    }

    fn save(&self, cookies: &str) -> Result<(), ApiError> {
        self.entry
            .set_password(cookies)
            .map_err(|e| ApiError::Vault(e.to_string()))
    }

    fn clear(&self) -> Result<(), ApiError> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ApiError::Vault(e.to_string())),
        }
    }
}

/// In-memory [`CookieVault`] for tests and headless use. Contents die with the
/// process; `Debug` redacts them.
#[derive(Default)]
pub struct MemoryVault {
    inner: Mutex<Option<String>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Option<String>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl fmt::Debug for MemoryVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemoryVault(..)")
    }
}

impl CookieVault for MemoryVault {
    fn load(&self) -> Result<Option<String>, ApiError> {
        Ok(self.lock().clone())
    }

    fn save(&self, cookies: &str) -> Result<(), ApiError> {
        *self.lock() = Some(cookies.to_string());
        Ok(())
    }

    fn clear(&self) -> Result<(), ApiError> {
        *self.lock() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_vault_round_trips_and_clears() {
        let vault = MemoryVault::new();
        assert_eq!(vault.load().unwrap(), None);
        vault.save("auth=synthetic_cookie").unwrap();
        assert_eq!(
            vault.load().unwrap().as_deref(),
            Some("auth=synthetic_cookie")
        );
        vault.clear().unwrap();
        assert_eq!(vault.load().unwrap(), None);
    }

    #[test]
    fn memory_vault_debug_redacts_contents() {
        let vault = MemoryVault::new();
        vault.save("auth=synthetic_cookie").unwrap();
        assert!(!format!("{vault:?}").contains("synthetic_cookie"));
    }
}
