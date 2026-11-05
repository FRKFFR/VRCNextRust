//! Authentication flow: login, 2FA verification, session resume, logout.
//!
//! Implements the community-documented flow (vrchat.community):
//! `GET /auth/user` with Basic auth `base64(urlencode(u):urlencode(p))`,
//! `POST /auth/twofactorauth/<method>/verify`, and `PUT /logout`.
//! Passwords and 2FA codes are [`SecretString`]s held only for the duration
//! of a call; only auth cookies are persisted (RULES.md §4).

use reqwest::Method;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::models::{AuthState, CurrentUser, TwoFactorMethod};
use crate::{ApiError, Client};

/// Result of [`Client::login`].
#[derive(Debug)]
pub enum LoginOutcome {
    /// Credentials accepted and no 2FA pending; the auth cookie was stored in
    /// the vault. Boxed to keep the enum small.
    LoggedIn(Box<CurrentUser>),
    /// Credentials accepted but a second factor is required; call
    /// [`Client::verify_2fa`] with one of these methods.
    Needs2fa(Vec<TwoFactorMethod>),
}

impl Client {
    /// `GET /auth/user` with HTTP Basic credentials.
    ///
    /// Returns [`LoginOutcome::Needs2fa`] when the API answers
    /// `requiresTwoFactorAuth`, [`LoginOutcome::LoggedIn`] once a full session
    /// exists. `password` is never stored or logged.
    pub async fn login(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<LoginOutcome, ApiError> {
        let url = self.url("/auth/user");
        let username = form_encode(username);
        // The encoded credential is still a secret; keep it wrapped until the
        // moment reqwest consumes it.
        let password = SecretString::from(form_encode(password.expose_secret()));
        let resp = self
            .send(Method::GET, |http| {
                http.get(&url)
                    .basic_auth(&username, Some(password.expose_secret()))
            })
            .await?;
        let value: Value = self.decode(resp).await?;
        let state: AuthState = serde_json::from_value(value.clone()).map_err(ApiError::Decode)?;
        if !state.requires_two_factor_auth.is_empty() {
            let methods: Vec<TwoFactorMethod> = state
                .requires_two_factor_auth
                .iter()
                .filter_map(|m| TwoFactorMethod::parse(m))
                .collect();
            return if methods.is_empty() {
                Err(ApiError::Requires2fa)
            } else {
                Ok(LoginOutcome::Needs2fa(methods))
            };
        }
        let user: CurrentUser = serde_json::from_value(value).map_err(ApiError::Decode)?;
        self.persist_auth_cookies()?;
        Ok(LoginOutcome::LoggedIn(Box::new(user)))
    }

    /// `POST /auth/twofactorauth/<method>/verify` with a one-time `code`.
    ///
    /// On success the session cookies are persisted and the freshly logged-in
    /// [`CurrentUser`] is returned.
    pub async fn verify_2fa(
        &self,
        method: TwoFactorMethod,
        code: &SecretString,
    ) -> Result<CurrentUser, ApiError> {
        let path = format!("/auth/twofactorauth/{}/verify", method.verify_path());
        // `serde_json::to_string` cannot fail on a &str.
        let code_json = SecretString::from(
            serde_json::to_string(code.expose_secret()).map_err(ApiError::Decode)?,
        );
        let body = SecretString::from(format!("{{\"code\":{}}}", code_json.expose_secret()));
        let value: Value = self.post_secret_json(&path, &body).await?;
        let verified = value
            .get("verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !verified {
            return Err(ApiError::Requires2fa);
        }
        self.persist_auth_cookies()?;
        self.current_user().await
    }

    /// `GET /auth/user` for an established session.
    ///
    /// Returns [`ApiError::Requires2fa`] when the session cookie still awaits
    /// a second factor.
    pub async fn current_user(&self) -> Result<CurrentUser, ApiError> {
        let value: Value = self.get_json("/auth/user").await?;
        let state: AuthState = serde_json::from_value(value.clone()).map_err(ApiError::Decode)?;
        if !state.requires_two_factor_auth.is_empty() {
            return Err(ApiError::Requires2fa);
        }
        serde_json::from_value(value).map_err(ApiError::Decode)
    }

    /// Try to resume a session from the vault's persisted auth cookies.
    ///
    /// `Ok(None)` when nothing is stored or the session is no longer valid
    /// (the vault is cleared in that case). Other failures propagate.
    pub async fn resume_session(&self) -> Result<Option<CurrentUser>, ApiError> {
        let Some(vault) = self.vault() else {
            return Ok(None);
        };
        let Some(cookies) = vault.load()? else {
            return Ok(None);
        };
        self.inject_auth_cookies(&cookies)?;
        match self.current_user().await {
            Ok(user) => Ok(Some(user)),
            Err(ApiError::Unauthorized) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `PUT /logout`, then drop all local auth state. An already-expired
    /// session counts as a successful logout.
    pub async fn logout(&self) -> Result<(), ApiError> {
        let url = self.url("/logout");
        let result = self.send(Method::PUT, |http| http.put(&url)).await;
        self.clear_auth_state();
        match result {
            Ok(_) | Err(ApiError::Unauthorized) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Percent-encode like `application/x-www-form-urlencoded` (minus the
/// space-as-`+` quirk), as required by `GET /auth/user`'s Basic auth scheme.
fn form_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encode_leaves_unreserved_and_escapes_rest() {
        assert_eq!(form_encode("plain.NAME-9_~"), "plain.NAME-9_~");
        assert_eq!(form_encode("test@example.com"), "test%40example.com");
        assert_eq!(form_encode("a b+c"), "a%20b%2Bc");
        assert_eq!(form_encode("w\u{00fc}rfel"), "w%C3%BCrfel");
    }
}
