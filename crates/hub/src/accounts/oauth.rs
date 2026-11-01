//! The hub as its own OAuth 2.1 authorization server for AI clients (D25 §14, increment
//! 32b): Claude, ChatGPT, Codex and any MCP client register, a person approves them with
//! the hub's own sign-in, and the hub issues and checks its own tokens. Nothing here talks
//! to another service, so AI clients keep working with no identity provider deployed.
//!
//! - Clients register themselves (RFC 7591) with their redirect URIs: https, or http on
//!   this machine's loopback for CLIs. They are public clients: PKCE (S256) is required.
//! - A code works once, for its client, redirect URI, verifier and resource.
//! - Tokens are opaque: `dgh_` for access and `dgr_` for refresh. Only their SHA-256 is
//!   stored. How long each lives is the config's `auth.tokens`.
//! - Refresh tokens rotate. Presenting a spent one ends its whole family: a stolen copy
//!   and the real client cannot both go on.
//! - Every token is for one resource, the hub's `/mcp`, and dies with its account.

use super::*;

/// A registered client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
}

/// What a successful code or refresh exchange returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub expires_in: u64,
    pub scope: String,
}

/// Who an access token speaks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub account: Account,
    pub client: String,
}

fn invalid_grant() -> BridgeError {
    BridgeError::new(
        "invalid_grant",
        "the code or token is unknown, used, expired or not this client's",
    )
}

/// A redirect URI a client may register: https anywhere, or http on loopback (any port),
/// absolute, without a fragment, at most 2 KiB.
pub fn valid_redirect_uri(uri: &str) -> bool {
    if uri.len() > 2048 || uri.contains('#') || uri.chars().any(char::is_whitespace) {
        return false;
    }
    let Ok(parsed) = uri.parse::<axum::http::Uri>() else {
        return false;
    };
    let Some(host) = parsed.host() else {
        return false;
    };
    match parsed.scheme_str() {
        Some("https") => true,
        Some("http") => matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"),
        _ => false,
    }
}

/// The PKCE S256 transform of `verifier` (RFC 7636 §4.2).
pub fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// What a code was issued for, read back when it is exchanged.
struct IssuedCode {
    client: String,
    account: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    scope: String,
    expires_at: u64,
}

/// A refresh token as stored.
struct StoredRefresh {
    family: String,
    client: String,
    account: String,
    scope: String,
    resource: String,
    expires_at: u64,
    used: bool,
}

impl Accounts {
    /// Register a public client with its redirect URIs. Past `auth.tokens.max_clients`,
    /// the oldest clients never used make room.
    pub async fn register_client(
        &self,
        name: &str,
        redirect_uris: &[String],
    ) -> BridgeResult<Client> {
        let name = name.trim();
        let name = if name.is_empty() { "An MCP client" } else { name };
        if name.len() > 128 || name.chars().any(char::is_control) {
            return Err(BridgeError::new(
                "invalid_client_metadata",
                "client_name is at most 128 printable characters",
            ));
        }
        if redirect_uris.is_empty()
            || redirect_uris.len() > 10
            || !redirect_uris.iter().all(|uri| valid_redirect_uri(uri))
        {
            return Err(BridgeError::new(
                "invalid_redirect_uri",
                "1 to 10 redirect URIs, each https or http on loopback, with no fragment",
            ));
        }
        let id = format!("dgc_{}", URL_SAFE_NO_PAD.encode(crate::link::random::<16>()));
        let uris = serde_json::to_string(redirect_uris).unwrap_or_default();
        let now = now_secs();
        let max = secs(self.tokens.max_clients);
        transaction!(&self.store, |connection| {
            let held: i64 = sqlx::query("SELECT COUNT(*) FROM oauth_clients")
                .fetch_one(&mut *connection)
                .await?
                .try_get(0)?;
            if held >= max {
                sqlx::query(
                    "DELETE FROM oauth_clients WHERE id IN (
                       SELECT id FROM oauth_clients WHERE last_used IS NULL
                       ORDER BY created_at, id LIMIT $1)",
                )
                .bind(held - max + 1)
                .execute(&mut *connection)
                .await?;
            }
            sqlx::query(
                "INSERT INTO oauth_clients (id, name, redirect_uris, created_at) VALUES ($1, $2, $3, $4)",
            )
            .bind(&id)
            .bind(name)
            .bind(&uris)
            .bind(secs(now))
            .execute(&mut *connection)
            .await?;
            Ok(())
        })?;
        Ok(Client {
            id,
            name: name.to_string(),
            redirect_uris: redirect_uris.to_vec(),
        })
    }

    pub async fn client(&self, id: &str) -> BridgeResult<Option<Client>> {
        let mut connection = self.store.read().await?;
        let row = sqlx::query("SELECT name, redirect_uris FROM oauth_clients WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(store_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let name: String = row.try_get(0).map_err(store_error)?;
        let uris: String = row.try_get(1).map_err(store_error)?;
        Ok(Some(Client {
            id: id.to_string(),
            name,
            redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
        }))
    }

    /// A code for `account`'s approval of `client`, bound to the redirect URI, the PKCE
    /// challenge and the resource.
    pub async fn issue_code(
        &self,
        client: &str,
        account: &str,
        redirect_uri: &str,
        challenge: &str,
        resource: &str,
        scope: &str,
    ) -> BridgeResult<String> {
        self.issue_code_at(client, account, redirect_uri, challenge, resource, scope, now_secs())
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn issue_code_at(
        &self,
        client: &str,
        account: &str,
        redirect_uri: &str,
        challenge: &str,
        resource: &str,
        scope: &str,
        now: u64,
    ) -> BridgeResult<String> {
        let code = format!("dga_{}", URL_SAFE_NO_PAD.encode(crate::link::random::<32>()));
        let expires = secs(now.saturating_add(self.tokens.code_secs));
        transaction!(&self.store, |connection| {
            sqlx::query("DELETE FROM oauth_codes WHERE expires_at <= $1")
                .bind(secs(now))
                .execute(&mut *connection)
                .await?;
            sqlx::query(
                "INSERT INTO oauth_codes (hash, client, account, redirect_uri, challenge, resource, scope, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(hash(&code))
            .bind(client)
            .bind(account)
            .bind(redirect_uri)
            .bind(challenge)
            .bind(resource)
            .bind(scope)
            .bind(expires)
            .execute(&mut *connection)
            .await?;
            Ok(())
        })?;
        Ok(code)
    }

    /// Trade a code for tokens, once: the client, redirect URI, PKCE verifier and resource
    /// must be the ones the code was issued for. A code is spent by the first try, right or
    /// wrong.
    pub async fn exchange_code(
        &self,
        code: &str,
        client: &str,
        redirect_uri: &str,
        verifier: &str,
        resource: Option<&str>,
    ) -> BridgeResult<Tokens> {
        self.exchange_code_at(code, client, redirect_uri, verifier, resource, now_secs())
            .await
    }

    pub(crate) async fn exchange_code_at(
        &self,
        code: &str,
        client: &str,
        redirect_uri: &str,
        verifier: &str,
        resource: Option<&str>,
        now: u64,
    ) -> BridgeResult<Tokens> {
        let family = URL_SAFE_NO_PAD.encode(crate::link::random::<16>());
        let fresh = (crate::link::random::<32>(), crate::link::random::<32>());
        transaction!(&self.store, |connection| {
            let issued = sqlx::query(
                "SELECT client, account, redirect_uri, challenge, resource, scope, expires_at
                 FROM oauth_codes WHERE hash = $1",
            )
            .bind(hash(code))
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| -> Result<IssuedCode, sqlx::Error> {
                Ok(IssuedCode {
                    client: row.try_get(0)?,
                    account: row.try_get(1)?,
                    redirect_uri: row.try_get(2)?,
                    challenge: row.try_get(3)?,
                    resource: row.try_get(4)?,
                    scope: row.try_get(5)?,
                    expires_at: unsecs(row.try_get(6)?),
                })
            })
            .transpose()?;
            // Used once, whatever happens next: this is committed with a refusal too.
            sqlx::query("DELETE FROM oauth_codes WHERE hash = $1")
                .bind(hash(code))
                .execute(&mut *connection)
                .await?;
            let Some(issued) = issued else {
                return Ok(Err(invalid_grant()));
            };
            if now >= issued.expires_at
                || issued.client != client
                || issued.redirect_uri != redirect_uri
                || resource.is_some_and(|asked| asked != issued.resource)
                || !(43..=128).contains(&verifier.len())
                || s256(verifier) != issued.challenge
            {
                return Ok(Err(invalid_grant()));
            }
            if let Err(refused) = live_account(connection, &issued.account).await? {
                return Ok(Err(refused));
            }
            let tokens = self
                .mint(
                    connection,
                    &family,
                    fresh,
                    client,
                    &issued.account,
                    &issued.scope,
                    &issued.resource,
                    now,
                )
                .await?;
            sqlx::query("UPDATE oauth_clients SET last_used = $1 WHERE id = $2")
                .bind(secs(now))
                .bind(client)
                .execute(&mut *connection)
                .await?;
            Ok(Ok(tokens))
        })?
    }

    /// Rotate a refresh token: the new pair in the same family, the old one spent. A spent
    /// one presented again ends the family. `resource`, when given, must be the token's.
    pub async fn refresh(
        &self,
        refresh: &str,
        client: &str,
        resource: Option<&str>,
    ) -> BridgeResult<Tokens> {
        self.refresh_at(refresh, client, resource, now_secs()).await
    }

    pub(crate) async fn refresh_at(
        &self,
        refresh: &str,
        client: &str,
        resource: Option<&str>,
        now: u64,
    ) -> BridgeResult<Tokens> {
        let fresh = (crate::link::random::<32>(), crate::link::random::<32>());
        transaction!(&self.store, |connection| {
            let stored = sqlx::query(
                "SELECT family, client, account, scope, resource, expires_at, used_at
                 FROM oauth_tokens WHERE hash = $1 AND kind = 'refresh'",
            )
            .bind(hash(refresh))
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| -> Result<StoredRefresh, sqlx::Error> {
                Ok(StoredRefresh {
                    family: row.try_get(0)?,
                    client: row.try_get(1)?,
                    account: row.try_get(2)?,
                    scope: row.try_get(3)?,
                    resource: row.try_get(4)?,
                    expires_at: unsecs(row.try_get(5)?),
                    used: row.try_get::<Option<i64>, _>(6)?.is_some(),
                })
            })
            .transpose()?;
            let Some(stored) = stored else {
                return Ok(Err(invalid_grant()));
            };
            if stored.used {
                // A spent refresh token again: a copy is about. The whole family ends, and
                // that is committed with the refusal.
                sqlx::query("DELETE FROM oauth_tokens WHERE family = $1")
                    .bind(&stored.family)
                    .execute(&mut *connection)
                    .await?;
                return Ok(Err(invalid_grant()));
            }
            if now >= stored.expires_at
                || stored.client != client
                || resource.is_some_and(|asked| asked != stored.resource)
            {
                return Ok(Err(invalid_grant()));
            }
            if let Err(refused) = live_account(connection, &stored.account).await? {
                return Ok(Err(refused));
            }
            let spent = sqlx::query(
                "UPDATE oauth_tokens SET used_at = $1 WHERE hash = $2 AND used_at IS NULL",
            )
            .bind(secs(now))
            .bind(hash(refresh))
            .execute(&mut *connection)
            .await?
            .rows_affected();
            if spent != 1 {
                return Ok(Err(invalid_grant()));
            }
            // The family's older access tokens end with the rotation.
            sqlx::query("DELETE FROM oauth_tokens WHERE family = $1 AND kind = 'access'")
                .bind(&stored.family)
                .execute(&mut *connection)
                .await?;
            let tokens = self
                .mint(
                    connection,
                    &stored.family,
                    fresh,
                    client,
                    &stored.account,
                    &stored.scope,
                    &stored.resource,
                    now,
                )
                .await?;
            Ok(Ok(tokens))
        })?
    }

    /// Who an access token speaks for, when it is live and its account is not disabled.
    pub async fn verify_access(&self, token: &str) -> BridgeResult<Option<Grant>> {
        self.verify_access_at(token, now_secs()).await
    }

    pub(crate) async fn verify_access_at(
        &self,
        token: &str,
        now: u64,
    ) -> BridgeResult<Option<Grant>> {
        if !token.starts_with("dgh_") || token.len() > 128 {
            return Ok(None);
        }
        let mut connection = self.store.read().await?;
        sqlx::query(
            "SELECT a.id, a.name, a.owner, a.disabled_at, t.client FROM oauth_tokens t
             JOIN accounts a ON a.id = t.account
             WHERE t.hash = $1 AND t.kind = 'access' AND t.expires_at > $2
               AND a.disabled_at IS NULL",
        )
        .bind(hash(token))
        .bind(secs(now))
        .fetch_optional(&mut *connection)
        .await
        .and_then(|row| {
            row.map(|row| -> Result<Grant, sqlx::Error> {
                Ok(Grant {
                    account: account_row(&row)?,
                    client: row.try_get(4)?,
                })
            })
            .transpose()
        })
        .map_err(store_error)
    }

    /// Revoke a token (RFC 7009): its whole family ends. Unknown tokens are fine.
    pub async fn revoke_token(&self, token: &str) -> BridgeResult<()> {
        let mut connection = self.store.read().await?;
        sqlx::query(
            "DELETE FROM oauth_tokens WHERE family IN (
               SELECT family FROM oauth_tokens WHERE hash = $1)",
        )
        .bind(hash(token))
        .execute(&mut *connection)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    /// End every AI connection of `account` (the account page's revoke, disabling).
    #[allow(dead_code)] // the account page (32a-3)
    pub async fn revoke_connections(&self, account: &str) -> BridgeResult<u64> {
        let mut connection = self.store.read().await?;
        sqlx::query("DELETE FROM oauth_tokens WHERE account = $1")
            .bind(account)
            .execute(&mut *connection)
            .await
            .map(|done| done.rows_affected())
            .map_err(store_error)
    }

    /// A new access and refresh token in `family`, from the random bytes the caller drew
    /// (so a transaction run again mints the same pair); expired tokens are cleared on the
    /// way.
    #[allow(clippy::too_many_arguments)]
    async fn mint(
        &self,
        connection: &mut AnyConnection,
        family: &str,
        (access, refresh): ([u8; 32], [u8; 32]),
        client: &str,
        account: &str,
        scope: &str,
        resource: &str,
        now: u64,
    ) -> Result<Tokens, Failed> {
        sqlx::query("DELETE FROM oauth_tokens WHERE expires_at <= $1")
            .bind(secs(now))
            .execute(&mut *connection)
            .await?;
        let access = format!("dgh_{}", URL_SAFE_NO_PAD.encode(access));
        let refresh = format!("dgr_{}", URL_SAFE_NO_PAD.encode(refresh));
        for (token, kind, ttl) in [
            (&access, "access", self.tokens.access_secs),
            (&refresh, "refresh", self.tokens.refresh_secs),
        ] {
            sqlx::query(
                "INSERT INTO oauth_tokens (hash, kind, family, client, account, scope, resource, created_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(hash(token))
            .bind(kind)
            .bind(family)
            .bind(client)
            .bind(account)
            .bind(scope)
            .bind(resource)
            .bind(secs(now))
            .bind(secs(now.saturating_add(ttl)))
            .execute(&mut *connection)
            .await?;
        }
        Ok(Tokens {
            access,
            refresh,
            expires_in: self.tokens.access_secs,
            scope: scope.to_string(),
        })
    }
}

/// Whether `account` may get tokens: it exists and is not disabled. The refusal is a value,
/// so the caller can commit what it already did (a spent code stays spent).
async fn live_account(
    connection: &mut AnyConnection,
    account: &str,
) -> Result<Result<(), BridgeError>, Failed> {
    Ok(match account_in(connection, account).await? {
        Some(found) if found.disabled_at.is_none() => Ok(()),
        Some(_) => Err(account_disabled(account)),
        None => Err(invalid_grant()),
    })
}

#[cfg(test)]
mod tests;
