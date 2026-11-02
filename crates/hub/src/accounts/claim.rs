//! Claiming a new hub from a browser (16c): a hub in `auth.mode hub` whose owner cannot sign
//! in yet prints a claim code to its own log when it starts, and whoever enters that code
//! on the hub's `/claim` page makes the owner's passkey there. No terminal on the host.
//!
//! Why a code from the log, and not "the first visitor owns it": a new hub is usually on the
//! internet before anyone has claimed it, so an open first-run page is a race anyone can
//! win (Portainer stops its setup after five minutes for this reason, Open WebUI lists it as
//! an accepted risk, Pangolin's users asked for a secret). Reading the log takes access to
//! the host or its container platform, which is what owning the hub should take; Jenkins
//! unlocks its setup the same way.
//!
//! - The code is 120 random bits in Crockford base32 (`XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`),
//!   read leniently: any case, dashes and spaces optional, `O` as `0`, `I` and `L` as `1`.
//! - Only its SHA-256 is stored, with when it stops working (`auth.tokens.claim_secs`). Each
//!   start of an unclaimed hub replaces it, so only the newest code printed works.
//! - Claiming spends nothing: it makes the owner (once) and a one-time owner link, the same
//!   link `devagotchi hub owner` prints, and the page goes straight on to make the passkey
//!   with it. Claiming again supersedes the earlier link, so a cancelled passkey prompt or a
//!   closed tab is retried with the same code.
//! - The owner's first passkey ends it: the code is deleted in the same transaction as the
//!   passkey is stored (`put_passkey`), and a claimed hub refuses every claim.

use devagotchi_contracts::{BridgeError, BridgeResult};
use sqlx::{AnyConnection, Row};

use super::{
    Account, Accounts, LinkPurpose, check_name, create_in, hash, issue_in, link_token,
    new_account_id, owner_in,
};
use crate::{
    devices::now_secs,
    store::{secs, store_error, transaction, unsecs},
};

/// Crockford's base32: no I, L, O or U, so a code read off a screen is typed back right.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// 120 bits, 24 characters.
const CODE_BYTES: usize = 15;
const CODE_CHARS: usize = CODE_BYTES * 8 / 5;

/// A claim code as printed, and when it stops working (Unix seconds).
pub struct ClaimCode {
    pub code: String,
    pub expires_at: u64,
}

impl std::fmt::Debug for ClaimCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClaimCode(<redacted>, expires {})", self.expires_at)
    }
}

/// `random` as a code, in groups of four.
fn encode(random: [u8; CODE_BYTES]) -> String {
    let mut plain = String::with_capacity(CODE_CHARS);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in random {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            plain.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    plain
        .as_bytes()
        .chunks(4)
        .map(|group| std::str::from_utf8(group).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// A code as someone typed or pasted it, in the one form its hash is kept under; `None`
/// when it cannot be a code.
pub fn normalize(input: &str) -> Option<String> {
    let mut out = String::with_capacity(CODE_CHARS);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            '-' | ' ' | '\t' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) || out.len() == CODE_CHARS {
            return None;
        }
        out.push(c);
    }
    (out.len() == CODE_CHARS).then_some(out)
}

fn wrong_code() -> BridgeError {
    BridgeError::new(
        "wrong_code",
        "that claim code is wrong, expired or replaced by a newer one",
    )
}

/// Whether the hub's owner has a passkey to sign in with.
async fn claimed_in(connection: &mut AnyConnection) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT 1 FROM accounts JOIN passkeys ON passkeys.account = accounts.id
         WHERE accounts.owner = 1 LIMIT 1",
    )
    .fetch_optional(&mut *connection)
    .await?
    .is_some())
}

impl Accounts {
    /// Whether this hub is claimed: its owner has a passkey to sign in with.
    pub async fn claimed(&self) -> BridgeResult<bool> {
        let mut connection = self.store.read().await?;
        claimed_in(&mut connection).await.map_err(store_error)
    }

    /// A new claim code when the hub is unclaimed, replacing any earlier one; `None` when
    /// it is claimed. The caller prints it; the store keeps only its hash.
    pub async fn issue_claim_code(&self) -> BridgeResult<Option<ClaimCode>> {
        self.issue_claim_code_at(crate::link::random::<CODE_BYTES>(), now_secs())
            .await
    }

    pub(crate) async fn issue_claim_code_at(
        &self,
        random: [u8; CODE_BYTES],
        now: u64,
    ) -> BridgeResult<Option<ClaimCode>> {
        let code = encode(random);
        let kept = hash(&normalize(&code).unwrap_or_default());
        let expires_at = now + self.tokens.claim_secs;
        let issued = transaction!(&self.store, |connection| {
            if claimed_in(connection).await? {
                return Ok(false);
            }
            sqlx::query("DELETE FROM claim_codes")
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT INTO claim_codes (hash, expires_at) VALUES ($1, $2)")
                .bind(&kept)
                .bind(secs(expires_at))
                .execute(&mut *connection)
                .await?;
            Ok(true)
        })?;
        Ok(issued.then_some(ClaimCode { code, expires_at }))
    }

    /// Claim the hub with the code it printed: the owner made if there is none yet (with
    /// the config's owner id, `configured`, if it names one), named `name`, and a one-time
    /// owner link for the claimant's first passkey. `claimed` once the owner has a passkey;
    /// `wrong_code` for any code that is not the live one.
    pub async fn claim(
        &self,
        code: &str,
        configured: Option<&str>,
        name: &str,
    ) -> BridgeResult<(Account, String)> {
        self.claim_at(
            code,
            configured,
            name,
            crate::link::random::<16>(),
            crate::link::random::<32>(),
            now_secs(),
        )
        .await
    }

    pub(crate) async fn claim_at(
        &self,
        code: &str,
        configured: Option<&str>,
        name: &str,
        id_random: [u8; 16],
        link_random: [u8; 32],
        now: u64,
    ) -> BridgeResult<(Account, String)> {
        let id = configured
            .map(str::to_string)
            .unwrap_or_else(|| new_account_id(id_random));
        let name = check_name(&id, name)?;
        let kept = normalize(code).map(|code| hash(&code));
        let token = link_token(link_random);
        let owner = transaction!(&self.store, |connection| {
            if claimed_in(connection).await? {
                return Err(BridgeError::new("claimed", "this hub already has an owner").into());
            }
            let Some(kept) = &kept else {
                return Err(wrong_code().into());
            };
            let live = sqlx::query("SELECT expires_at FROM claim_codes WHERE hash = $1")
                .bind(kept)
                .fetch_optional(&mut *connection)
                .await?;
            let live = match live {
                Some(row) => unsecs(row.try_get::<i64, _>(0)?) > now,
                None => false,
            };
            if !live {
                return Err(wrong_code().into());
            }
            let owner = match owner_in(connection).await? {
                Some(owner) => {
                    if configured.is_some_and(|id| id != owner.id) {
                        return Err(BridgeError::new(
                            "owner_mismatch",
                            format!(
                                "this hub's owner account is {}, not the config's owner",
                                owner.id
                            ),
                        )
                        .into());
                    }
                    // Nobody signs in as the owner yet: the claimant names them.
                    sqlx::query("UPDATE accounts SET name = $1 WHERE id = $2")
                        .bind(name)
                        .bind(&owner.id)
                        .execute(&mut *connection)
                        .await?;
                    Account {
                        name: name.to_string(),
                        ..owner
                    }
                }
                None => {
                    create_in(connection, &id, name, true, now).await?;
                    Account {
                        id: id.clone(),
                        name: name.to_string(),
                        owner: true,
                        disabled_at: None,
                    }
                }
            };
            issue_in(
                connection,
                LinkPurpose::Owner,
                &owner.id,
                &token,
                now,
                false,
            )
            .await?;
            Ok(owner)
        })?;
        Ok((owner, token))
    }
}

#[cfg(test)]
mod tests;
