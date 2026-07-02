//! Bluesky (atproto) OAuth for the admin portal.
//!
//! OAuth is used purely for identity: the flow proves control of a DID, we
//! check that DID against the curator roster, mint our own session cookie,
//! and immediately discard the atproto tokens — this app never acts on
//! anyone's PDS.
//!
//! Two client modes, chosen from the environment:
//! - **Confidential** (hosted): PCG_PUBLIC_URL + PCG_OAUTH_PRIVATE_KEY set.
//!   client_id is the public /oauth/client-metadata.json URL, token auth is
//!   private_key_jwt (ES256), and the public JWK is served at
//!   /oauth/jwks.json. Generate a key with `paperclips-gallery gen-oauth-key`.
//! - **Loopback** (dev): neither set. client_id derived from http://localhost;
//!   authorization servers accept it only for 127.0.0.1 redirect URIs.

use std::sync::Arc;

use anyhow::Context as _;
use atrium_api::types::string::Did;
use atrium_identity::{
    did::{CommonDidResolver, CommonDidResolverConfig},
    handle::{AppViewHandleResolver, AppViewHandleResolverConfig},
};
use atrium_oauth::{
    AtprotoClientMetadata, AtprotoLocalhostClientMetadata, AuthMethod, GrantType, KnownScope,
    OAuthClient, OAuthClientConfig, OAuthResolverConfig, Scope,
    store::{
        session::{Session, SessionStore},
        state::{InternalStateData, StateStore},
    },
};
use axum::{
    extract::{FromRequestParts, OptionalFromRequestParts},
    http::request::Parts,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use sqlx::PgPool;

pub const SESSION_COOKIE: &str = "pcg_admin";
const SESSION_TTL_DAYS: i64 = 30;

pub type AtriumOAuthClient = OAuthClient<
    PgStateStore,
    PgSessionStore,
    CommonDidResolver<HttpClient>,
    AppViewHandleResolver<HttpClient>,
    HttpClient,
>;

/// Thin reqwest adapter for atrium's HttpClient trait.
#[derive(Clone, Default)]
pub struct HttpClient {
    client: reqwest::Client,
}

impl atrium_xrpc::HttpClient for HttpClient {
    async fn send_http(
        &self,
        request: atrium_xrpc::http::Request<Vec<u8>>,
    ) -> Result<
        atrium_xrpc::http::Response<Vec<u8>>,
        Box<dyn std::error::Error + Send + Sync + 'static>,
    > {
        let response = self.client.execute(request.try_into()?).await?;
        let mut builder = atrium_xrpc::http::Response::builder().status(response.status());
        for (k, v) in response.headers() {
            builder = builder.header(k, v);
        }
        builder
            .body(response.bytes().await?.to_vec())
            .map_err(Into::into)
    }
}

#[derive(Debug)]
pub enum StoreError {
    Db(sqlx::Error),
    Serde(serde_json::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Db(e) => write!(f, "db error: {e}"),
            StoreError::Serde(e) => write!(f, "serde error: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
    }
}

/// Postgres-backed store for in-flight OAuth authorization state.
#[derive(Clone)]
pub struct PgStateStore {
    pool: PgPool,
}

impl atrium_common::store::Store<String, InternalStateData> for PgStateStore {
    type Error = StoreError;

    async fn get(&self, key: &String) -> Result<Option<InternalStateData>, Self::Error> {
        let row = sqlx::query!("SELECT data FROM oauth_states WHERE key = $1", key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| serde_json::from_value(r.data)).transpose()?)
    }

    async fn set(&self, key: String, value: InternalStateData) -> Result<(), Self::Error> {
        let data = serde_json::to_value(&value)?;
        sqlx::query!(
            "INSERT INTO oauth_states (key, data) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET data = EXCLUDED.data",
            key,
            data,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn del(&self, key: &String) -> Result<(), Self::Error> {
        sqlx::query!("DELETE FROM oauth_states WHERE key = $1", key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        sqlx::query!("DELETE FROM oauth_states")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl StateStore for PgStateStore {}

/// Postgres-backed store for atproto OAuth sessions. Rows are short-lived:
/// the callback handler deletes the session as soon as the DID is extracted.
#[derive(Clone)]
pub struct PgSessionStore {
    pool: PgPool,
}

impl atrium_common::store::Store<Did, Session> for PgSessionStore {
    type Error = StoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        let row = sqlx::query!(
            "SELECT data FROM oauth_sessions WHERE did = $1",
            key.as_str()
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| serde_json::from_value(r.data)).transpose()?)
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        let data = serde_json::to_value(&value)?;
        sqlx::query!(
            "INSERT INTO oauth_sessions (did, data) VALUES ($1, $2)
             ON CONFLICT (did) DO UPDATE SET data = EXCLUDED.data, updated_at = now()",
            key.as_str(),
            data,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        sqlx::query!("DELETE FROM oauth_sessions WHERE did = $1", key.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        sqlx::query!("DELETE FROM oauth_sessions")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl SessionStore for PgSessionStore {}

/// How this deployment identifies itself to authorization servers.
pub enum OauthMode {
    /// Dev: loopback client_id; browser must reach us via 127.0.0.1.
    Loopback { callback_url: String },
    /// Hosted: public client-metadata document + private_key_jwt (ES256).
    Confidential {
        public_url: String,
        private_key: p256::SecretKey,
    },
}

impl OauthMode {
    /// Confidential when PCG_PUBLIC_URL and PCG_OAUTH_PRIVATE_KEY are both
    /// set; loopback otherwise.
    pub fn from_env(port: u16) -> anyhow::Result<Self> {
        let public_url = std::env::var("PCG_PUBLIC_URL").ok();
        let key_b64 = std::env::var("PCG_OAUTH_PRIVATE_KEY").ok();
        match (public_url, key_b64) {
            (Some(public_url), Some(key_b64)) => Ok(Self::Confidential {
                public_url: public_url.trim_end_matches('/').to_string(),
                private_key: parse_private_key(&key_b64)?,
            }),
            (Some(_), None) => anyhow::bail!(
                "PCG_PUBLIC_URL is set but PCG_OAUTH_PRIVATE_KEY is not — \
                 generate one with `paperclips-gallery gen-oauth-key`"
            ),
            _ => Ok(Self::Loopback {
                callback_url: std::env::var("PCG_OAUTH_CALLBACK_URL")
                    .unwrap_or_else(|_| format!("http://127.0.0.1:{port}/admin/oauth/callback")),
            }),
        }
    }

    pub fn is_confidential(&self) -> bool {
        matches!(self, Self::Confidential { .. })
    }

    pub fn client_id(public_url: &str) -> String {
        format!("{public_url}/oauth/client-metadata.json")
    }

    pub fn redirect_uri(public_url: &str) -> String {
        format!("{public_url}/admin/oauth/callback")
    }

    pub fn jwks_uri(public_url: &str) -> String {
        format!("{public_url}/oauth/jwks.json")
    }

    /// The public JWK set served at /oauth/jwks.json (confidential only).
    pub fn jwks(&self) -> Option<serde_json::Value> {
        match self {
            Self::Confidential { private_key, .. } => {
                let jwk = public_jwk(private_key).ok()?;
                Some(serde_json::json!({ "keys": [jwk] }))
            }
            Self::Loopback { .. } => None,
        }
    }

    /// The client-metadata document served at /oauth/client-metadata.json
    /// (confidential only).
    pub fn client_metadata_doc(&self) -> Option<serde_json::Value> {
        match self {
            Self::Confidential { public_url, .. } => Some(serde_json::json!({
                "client_id": Self::client_id(public_url),
                "client_name": "Fluoddity — a field guide",
                "client_uri": public_url,
                "application_type": "web",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "scope": "atproto",
                "redirect_uris": [Self::redirect_uri(public_url)],
                "dpop_bound_access_tokens": true,
                "token_endpoint_auth_method": "private_key_jwt",
                "token_endpoint_auth_signing_alg": "ES256",
                "jwks_uri": Self::jwks_uri(public_url),
            })),
            Self::Loopback { .. } => None,
        }
    }
}

/// Decode a base64-wrapped SEC1 PEM EC private key (P-256).
pub fn parse_private_key(key_b64: &str) -> anyhow::Result<p256::SecretKey> {
    use base64::Engine as _;
    let pem = base64::engine::general_purpose::STANDARD
        .decode(key_b64.trim())
        .context("PCG_OAUTH_PRIVATE_KEY is not valid base64")?;
    let pem = String::from_utf8(pem).context("decoded private key is not UTF-8 PEM")?;
    p256::SecretKey::from_sec1_pem(&pem).map_err(|e| anyhow::anyhow!("parsing EC private key: {e}"))
}

/// A fresh base64-wrapped SEC1 PEM key for PCG_OAUTH_PRIVATE_KEY.
pub fn generate_private_key() -> anyhow::Result<String> {
    use base64::Engine as _;
    let key = p256::SecretKey::random(&mut rand_core::OsRng);
    let pem = key
        .to_sec1_pem(elliptic_curve::pkcs8::LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("encoding key: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(pem.as_bytes()))
}

/// kid = base64url(SHA-256(x || y)), matching across private and public JWKs.
fn key_id(point: &p256::EncodedPoint) -> anyhow::Result<String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    let (Some(x), Some(y)) = (point.x(), point.y()) else {
        anyhow::bail!("EC point missing coordinates");
    };
    let mut hasher = sha2::Sha256::new();
    hasher.update(x);
    hasher.update(y);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

/// Private signing JWK for atrium's private_key_jwt + DPoP machinery.
fn private_jwk(key: &p256::SecretKey) -> anyhow::Result<jose_jwk::Jwk> {
    let jwk_ec = key.to_jwk();
    let raw = serde_json::to_string(&jwk_ec).context("serializing JwkEcKey")?;
    let mut jwk: jose_jwk::Jwk = serde_json::from_str(&raw).context("converting to jose Jwk")?;
    jwk.prm.kid = Some(key_id(&key.public_key().into())?);
    jwk.prm.alg = Some(jose_jwk::jose_jwa::Algorithm::Signing(
        jose_jwk::jose_jwa::Signing::Es256,
    ));
    jwk.prm.cls = Some(jose_jwk::Class::Signing);
    jwk.prm.ops = Some(std::collections::BTreeSet::from([
        jose_jwk::Operations::Sign,
    ]));
    Ok(jwk)
}

/// Public half of the signing key, for the published JWK set.
fn public_jwk(key: &p256::SecretKey) -> anyhow::Result<jose_jwk::Jwk> {
    let mut jwk = private_jwk(key)?;
    if let jose_jwk::Key::Ec(ec) = &mut jwk.key {
        ec.d = None;
    }
    jwk.prm.ops = Some(std::collections::BTreeSet::from([
        jose_jwk::Operations::Verify,
    ]));
    Ok(jwk)
}

/// Build the OAuth client for the configured mode.
pub fn build_oauth_client(pool: PgPool, mode: &OauthMode) -> anyhow::Result<AtriumOAuthClient> {
    let http_client = HttpClient::default();
    let arced = Arc::new(http_client.clone());
    let resolver = OAuthResolverConfig {
        did_resolver: CommonDidResolver::new(CommonDidResolverConfig {
            plc_directory_url: "https://plc.directory".to_string(),
            http_client: arced.clone(),
        }),
        handle_resolver: AppViewHandleResolver::new(AppViewHandleResolverConfig {
            service_url: "https://public.api.bsky.app".to_string(),
            http_client: arced,
        }),
        authorization_server_metadata: Default::default(),
        protected_resource_metadata: Default::default(),
    };
    let state_store = PgStateStore { pool: pool.clone() };
    let session_store = PgSessionStore { pool };

    match mode {
        OauthMode::Loopback { callback_url } => {
            let config = OAuthClientConfig {
                client_metadata: AtprotoLocalhostClientMetadata {
                    redirect_uris: Some(vec![callback_url.clone()]),
                    scopes: Some(vec![Scope::Known(KnownScope::Atproto)]),
                },
                keys: None,
                resolver,
                state_store,
                session_store,
                http_client,
            };
            OAuthClient::new(config)
                .map_err(|e| anyhow::anyhow!("failed to build loopback oauth client: {e}"))
        }
        OauthMode::Confidential {
            public_url,
            private_key,
        } => {
            let config = OAuthClientConfig {
                client_metadata: AtprotoClientMetadata {
                    client_id: OauthMode::client_id(public_url),
                    client_uri: Some(public_url.clone()),
                    redirect_uris: vec![OauthMode::redirect_uri(public_url)],
                    scopes: vec![Scope::Known(KnownScope::Atproto)],
                    token_endpoint_auth_method: AuthMethod::PrivateKeyJwt,
                    grant_types: vec![GrantType::AuthorizationCode, GrantType::RefreshToken],
                    token_endpoint_auth_signing_alg: Some("ES256".to_string()),
                    jwks_uri: Some(OauthMode::jwks_uri(public_url)),
                },
                keys: Some(vec![private_jwk(private_key)?]),
                resolver,
                state_store,
                session_store,
                http_client,
            };
            OAuthClient::new(config)
                .map_err(|e| anyhow::anyhow!("failed to build confidential oauth client: {e}"))
        }
    }
}

/// A logged-in member of the curation roster.
#[derive(Debug, Clone)]
pub struct Curator {
    pub did: String,
    pub handle: String,
    pub role: String,
}

/// Look up the curator for a session token, if the session is still live.
pub async fn curator_for_token(pool: &PgPool, token: &str) -> anyhow::Result<Option<Curator>> {
    Ok(sqlx::query!(
        "SELECT c.did, c.handle, c.role
         FROM admin_sessions s
         JOIN curators c ON c.did = s.did
         WHERE s.token = $1 AND s.expires_at > now()",
        token,
    )
    .fetch_optional(pool)
    .await?
    .map(|row| Curator {
        did: row.did,
        handle: row.handle,
        role: row.role,
    }))
}

/// Mint an admin session for a DID already confirmed to be on the roster.
pub async fn create_admin_session(pool: &PgPool, did: &str) -> anyhow::Result<String> {
    let token = uuid::Uuid::new_v4().to_string();
    sqlx::query!(
        "INSERT INTO admin_sessions (token, did, expires_at)
         VALUES ($1, $2, now() + make_interval(days => $3::int))",
        token,
        did,
        SESSION_TTL_DAYS as i32,
    )
    .execute(pool)
    .await?;
    Ok(token)
}

/// Seed the roster from PCG_ADMIN_DIDS (comma-separated `did[=handle]`
/// entries). The artist DID from gallery_meta is always on the roster.
pub async fn seed_curators(pool: &PgPool) -> anyhow::Result<()> {
    if let Some(meta) = sqlx::query!("SELECT artist_did, artist_handle FROM gallery_meta")
        .fetch_optional(pool)
        .await?
    {
        sqlx::query!(
            "INSERT INTO curators (did, handle, role) VALUES ($1, $2, 'artist')
             ON CONFLICT (did) DO UPDATE SET role = 'artist', handle = EXCLUDED.handle",
            meta.artist_did,
            meta.artist_handle,
        )
        .execute(pool)
        .await?;
    }
    let Ok(admin_dids) = std::env::var("PCG_ADMIN_DIDS") else {
        return Ok(());
    };
    for entry in admin_dids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let (did, handle) = entry.split_once('=').unwrap_or((entry, ""));
        sqlx::query!(
            "INSERT INTO curators (did, handle) VALUES ($1, $2)
             ON CONFLICT (did) DO NOTHING",
            did,
            handle,
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Extractor: requires a live admin session; otherwise redirects to /admin/login.
impl FromRequestParts<crate::SharedState> for Curator {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::SharedState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
            return Err(Redirect::to("/admin/login").into_response());
        };
        match curator_for_token(&state.pool, &token).await {
            Ok(Some(curator)) => Ok(curator),
            Ok(None) => Err(Redirect::to("/admin/login").into_response()),
            Err(err) => {
                tracing::error!(?err, "curator lookup failed");
                Err(Redirect::to("/admin/login").into_response())
            }
        }
    }
}

/// Optional variant: public pages can light up curator controls when a
/// valid session cookie is present, without requiring one.
impl OptionalFromRequestParts<crate::SharedState> for Curator {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::SharedState,
    ) -> Result<Option<Self>, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
            return Ok(None);
        };
        Ok(curator_for_token(&state.pool, &token)
            .await
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_and_derive_jwks() {
        let b64 = generate_private_key().unwrap();
        let key = parse_private_key(&b64).unwrap();

        let private = private_jwk(&key).unwrap();
        let public = public_jwk(&key).unwrap();
        assert_eq!(
            private.prm.kid, public.prm.kid,
            "kid must match across halves"
        );

        let private_json = serde_json::to_value(&private).unwrap();
        let public_json = serde_json::to_value(&public).unwrap();
        assert!(private_json.get("d").is_some(), "private jwk keeps d");
        assert!(public_json.get("d").is_none(), "public jwk must not leak d");
        assert_eq!(public_json["kty"], "EC");
        assert_eq!(public_json["crv"], "P-256");
    }

    #[test]
    fn confidential_mode_publishes_consistent_documents() {
        let key = p256::SecretKey::random(&mut rand_core::OsRng);
        let mode = OauthMode::Confidential {
            public_url: "https://fluoddity.example".to_string(),
            private_key: key,
        };
        let doc = mode.client_metadata_doc().unwrap();
        assert_eq!(
            doc["client_id"],
            "https://fluoddity.example/oauth/client-metadata.json"
        );
        assert_eq!(
            doc["redirect_uris"][0],
            "https://fluoddity.example/admin/oauth/callback"
        );
        assert_eq!(doc["token_endpoint_auth_method"], "private_key_jwt");
        assert_eq!(doc["jwks_uri"], "https://fluoddity.example/oauth/jwks.json");

        let jwks = mode.jwks().unwrap();
        assert_eq!(jwks["keys"].as_array().unwrap().len(), 1);
        assert!(jwks["keys"][0].get("d").is_none());
    }

    #[test]
    fn loopback_mode_publishes_nothing() {
        let mode = OauthMode::Loopback {
            callback_url: "http://127.0.0.1:4601/admin/oauth/callback".to_string(),
        };
        assert!(mode.client_metadata_doc().is_none());
        assert!(mode.jwks().is_none());
        assert!(!mode.is_confidential());
    }
}
