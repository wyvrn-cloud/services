//! What the web view keeps, beside the rest of the community's store: the
//! channels made public, members' pictures and lasting keys, sessions, and the
//! identity provider's websites, consents, codes, tokens and keys.
//!
//! Every secret handed out (a session, a code, a token, a website's secret) is
//! kept only as its hash ([`hashed`]).

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use wyvrn_service::db::Result;
use wyvrn_service::params;

use crate::store::{Member, Store};

/// A secret as it is kept: its SHA-256, base64url.
pub fn hashed(secret: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

/// A new secret: 256 random bits, as 64 hex digits.
pub fn secret() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// A website that may sign members in through the community.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: String,
    /// `registered` (by an admin), `dynamic` (it registered itself) or `indieauth`.
    pub kind: String,
    pub name: String,
    /// Its secret's hash, for one that has a secret.
    pub secret: Option<String>,
    pub redirect_uris: Vec<String>,
    pub created: i64,
    pub created_by: Option<String>,
}

impl Client {
    /// Added by an admin.
    pub fn verified(&self) -> bool {
        self.kind == "registered"
    }
}

/// A code handed back to a website, to be exchanged for tokens once.
#[derive(Debug, Clone)]
pub struct Code {
    pub mkey: String,
    pub client: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub nonce: Option<String>,
    pub challenge: String,
}

/// An access token's holder and what it may read.
#[derive(Debug, Clone)]
pub struct Token {
    pub mkey: String,
    pub client: String,
    pub scopes: String,
}

fn client_from(row: &wyvrn_service::db::Row) -> Result<Client> {
    let uris: String = row.get(4)?;
    Ok(Client {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        secret: row.get(3)?,
        redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
        created: row.get(5)?,
        created_by: row.get(6)?,
    })
}

const CLIENT_COLUMNS: &str = "id, kind, name, secret, redirect_uris, created, created_by";

impl Store {
    // --- Members, as the web knows them -----------------------------------------

    /// `did`'s lasting key, made the first time it is asked for.
    pub fn member_key(&self, did: &str) -> Result<String> {
        let mut db = self.db.lock();
        if let Some(key) = db.query_row("SELECT mkey FROM member_keys WHERE did = ?1", params![did], |row| row.get::<String>(0))? {
            return Ok(key);
        }
        let key = uuid::Uuid::new_v4().simple().to_string();
        db.execute("INSERT OR IGNORE INTO member_keys (did, mkey) VALUES (?1, ?2)", params![did, key.clone()])?;
        Ok(db.query_row("SELECT mkey FROM member_keys WHERE did = ?1", params![did], |row| row.get::<String>(0))?.unwrap_or(key))
    }

    /// The member a lasting key is, while they are one.
    pub fn member_by_key(&self, key: &str) -> Result<Option<Member>> {
        let did = self.db.lock().query_row("SELECT did FROM member_keys WHERE mkey = ?1", params![key], |row| row.get::<String>(0))?;
        match did {
            Some(did) => self.member(&did),
            None => Ok(None),
        }
    }

    /// The picture a member's profile carries, or none: `(media type, base64)`.
    pub fn set_member_picture(&self, did: &str, picture: Option<(&str, &str)>, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM member_pictures WHERE did = ?1", params![did])?;
        if let Some((media_type, data)) = picture {
            db.execute("INSERT INTO member_pictures (did, media_type, data, at) VALUES (?1, ?2, ?3, ?4)", params![did, media_type, data, now])?;
        }
        Ok(())
    }

    pub fn member_picture(&self, did: &str) -> Result<Option<(String, String, i64)>> {
        self.db.lock().query_row("SELECT media_type, data, at FROM member_pictures WHERE did = ?1", params![did], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    /// The token a member's picture is served at, for one audience (the public
    /// pages, or one website): made once, so the address stays the same.
    pub fn picture_token(&self, mkey: &str, audience: &str, secret: &str) -> Result<String> {
        let token = super::keyed(secret, &format!("picture\n{audience}\n{mkey}"))[..32].to_string();
        self.db.lock().execute("INSERT OR IGNORE INTO web_pictures (token, mkey) VALUES (?1, ?2)", params![token.clone(), mkey])?;
        Ok(token)
    }

    pub fn picture_holder(&self, token: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT mkey FROM web_pictures WHERE token = ?1", params![token], |row| row.get(0))
    }

    // --- Public channels ------------------------------------------------------------

    /// The channels made public, each with when from (0 for its whole history).
    pub fn web_channels(&self) -> Result<HashMap<String, i64>> {
        Ok(self.db.lock().query("SELECT channel, since FROM web_channels", params![], |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)))?.into_iter().collect())
    }

    /// Makes a channel public from `since`, or takes it off the web (`None`).
    pub fn set_web_channel(&self, channel: &str, since: Option<i64>) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM web_channels WHERE channel = ?1", params![channel])?;
        if let Some(since) = since {
            db.execute("INSERT INTO web_channels (channel, since) VALUES (?1, ?2)", params![channel, since])?;
        }
        Ok(())
    }

    // --- Sessions -----------------------------------------------------------------------

    pub fn add_session(&self, token: &str, mkey: &str, csrf: &str, now: i64, expires: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM web_sessions WHERE expires < ?1", params![now])?;
        db.execute("INSERT INTO web_sessions (token, mkey, csrf, created, expires) VALUES (?1, ?2, ?3, ?4, ?5)", params![hashed(token), mkey, csrf, now, expires])?;
        Ok(())
    }

    /// The member key and form token a session cookie holds, while it lasts.
    pub fn session(&self, token: &str, now: i64) -> Result<Option<(String, String)>> {
        self.db.lock().query_row("SELECT mkey, csrf FROM web_sessions WHERE token = ?1 AND expires > ?2", params![hashed(token), now], |row| Ok((row.get(0)?, row.get(1)?)))
    }

    pub fn end_session(&self, token: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM web_sessions WHERE token = ?1", params![hashed(token)])?;
        Ok(())
    }

    // --- The identity provider's websites ---------------------------------------------

    pub fn add_client(&self, client: &Client) -> Result<()> {
        self.db.lock().execute(
            "INSERT OR REPLACE INTO oidc_clients (id, kind, name, secret, redirect_uris, created, created_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![client.id.clone(), client.kind.clone(), client.name.clone(), client.secret.clone(), serde_json::to_string(&client.redirect_uris).unwrap_or_default(), client.created, client.created_by.clone()],
        )?;
        Ok(())
    }

    pub fn client(&self, id: &str) -> Result<Option<Client>> {
        self.db.lock().query_row(&format!("SELECT {CLIENT_COLUMNS} FROM oidc_clients WHERE id = ?1"), params![id], client_from)
    }

    pub fn clients(&self) -> Result<Vec<Client>> {
        self.db.lock().query(&format!("SELECT {CLIENT_COLUMNS} FROM oidc_clients ORDER BY created, id"), params![], client_from)
    }

    /// How many websites have registered themselves from `created_by` (an address)
    /// since `since`.
    pub fn clients_registered_since(&self, created_by: &str, since: i64) -> Result<i64> {
        self.db.lock().query_value("SELECT COUNT(*) FROM oidc_clients WHERE kind = 'dynamic' AND created_by = ?1 AND created >= ?2", params![created_by, since])
    }

    /// Takes a website away, and everything it was given.
    pub fn remove_client(&self, id: &str) -> Result<bool> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM oidc_tokens WHERE client = ?1", params![id])?;
        db.execute("DELETE FROM oidc_codes WHERE client = ?1", params![id])?;
        db.execute("DELETE FROM oidc_consents WHERE client = ?1", params![id])?;
        Ok(db.execute("DELETE FROM oidc_clients WHERE id = ?1", params![id])? > 0)
    }

    // --- Consent ----------------------------------------------------------------------------

    /// The scopes a member allowed a website, as they were last allowed.
    pub fn consent(&self, mkey: &str, client: &str) -> Result<Option<String>> {
        self.db.lock().query_row("SELECT scopes FROM oidc_consents WHERE mkey = ?1 AND client = ?2", params![mkey, client], |row| row.get(0))
    }

    pub fn set_consent(&self, mkey: &str, client: &str, scopes: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT OR REPLACE INTO oidc_consents (mkey, client, scopes, at) VALUES (?1, ?2, ?3, ?4)", params![mkey, client, scopes, now])?;
        Ok(())
    }

    /// Every website a member allowed: `(client, scopes, when)`.
    pub fn consents_of(&self, mkey: &str) -> Result<Vec<(String, String, i64)>> {
        self.db.lock().query("SELECT client, scopes, at FROM oidc_consents WHERE mkey = ?1 ORDER BY at DESC", params![mkey], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    /// Takes back what a member allowed a website, and its tokens for them.
    pub fn revoke_consent(&self, mkey: &str, client: &str) -> Result<bool> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM oidc_tokens WHERE mkey = ?1 AND client = ?2", params![mkey, client])?;
        db.execute("DELETE FROM oidc_codes WHERE mkey = ?1 AND client = ?2", params![mkey, client])?;
        Ok(db.execute("DELETE FROM oidc_consents WHERE mkey = ?1 AND client = ?2", params![mkey, client])? > 0)
    }

    // --- Codes and tokens -------------------------------------------------------------------

    pub fn add_code(&self, code: &str, held: &Code, expires: i64) -> Result<()> {
        self.db.lock().execute(
            "INSERT INTO oidc_codes (code, mkey, client, redirect_uri, scopes, nonce, challenge, expires, used) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![hashed(code), held.mkey.clone(), held.client.clone(), held.redirect_uri.clone(), held.scopes.clone(), held.nonce.clone(), held.challenge.clone(), expires],
        )?;
        Ok(())
    }

    /// The code, used now: `None` if there is none, it is too old, or it was used
    /// already, in which case the tokens it got are taken back (RFC 6749 4.1.2).
    pub fn take_code(&self, code: &str, now: i64) -> Result<Option<Code>> {
        let hash = hashed(code);
        self.db.lock().transaction(|tx| {
            let row = tx.query_row(
                "SELECT mkey, client, redirect_uri, scopes, nonce, challenge, expires, used FROM oidc_codes WHERE code = ?1",
                params![hash.clone()],
                |row| Ok((Code { mkey: row.get(0)?, client: row.get(1)?, redirect_uri: row.get(2)?, scopes: row.get(3)?, nonce: row.get(4)?, challenge: row.get(5)? }, row.get::<i64>(6)?, row.get::<i64>(7)?)),
            )?;
            let Some((held, expires, used)) = row else { return Ok(None) };
            if used != 0 {
                tx.execute("DELETE FROM oidc_tokens WHERE code = ?1", params![hash.clone()])?;
                return Ok(None);
            }
            tx.execute("UPDATE oidc_codes SET used = 1 WHERE code = ?1", params![hash.clone()])?;
            Ok((expires > now).then_some(held))
        })
    }

    pub fn add_token(&self, token: &str, held: &Token, code: &str, expires: i64) -> Result<()> {
        self.db.lock().execute(
            "INSERT INTO oidc_tokens (token, mkey, client, scopes, code, expires) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![hashed(token), held.mkey.clone(), held.client.clone(), held.scopes.clone(), hashed(code), expires],
        )?;
        Ok(())
    }

    pub fn token(&self, token: &str, now: i64) -> Result<Option<Token>> {
        self.db.lock().query_row(
            "SELECT mkey, client, scopes FROM oidc_tokens WHERE token = ?1 AND expires > ?2",
            params![hashed(token), now],
            |row| Ok(Token { mkey: row.get(0)?, client: row.get(1)?, scopes: row.get(2)? }),
        )
    }

    pub fn revoke_token(&self, token: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM oidc_tokens WHERE token = ?1", params![hashed(token)])?;
        Ok(())
    }

    // --- The id tokens' keys -----------------------------------------------------------------

    /// Every key, newest first: `(kid, private JWK, made)`.
    pub fn oidc_keys(&self) -> Result<Vec<(String, String, i64)>> {
        self.db.lock().query("SELECT kid, jwk, created FROM oidc_keys ORDER BY created DESC, kid", params![], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    }

    pub fn add_oidc_key(&self, kid: &str, jwk: &str, now: i64) -> Result<()> {
        self.db.lock().execute("INSERT INTO oidc_keys (kid, jwk, created) VALUES (?1, ?2, ?3)", params![kid, jwk, now])?;
        Ok(())
    }

    pub fn remove_oidc_key(&self, kid: &str) -> Result<()> {
        self.db.lock().execute("DELETE FROM oidc_keys WHERE kid = ?1", params![kid])?;
        Ok(())
    }

    /// Forgets sessions, codes and tokens that have run out.
    pub fn prune_web(&self, now: i64) -> Result<()> {
        let mut db = self.db.lock();
        db.execute("DELETE FROM web_sessions WHERE expires < ?1", params![now])?;
        db.execute("DELETE FROM oidc_codes WHERE expires < ?1", params![now - 3600])?;
        db.execute("DELETE FROM oidc_tokens WHERE expires < ?1", params![now])?;
        Ok(())
    }
}
