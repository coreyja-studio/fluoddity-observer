//! Bluesky (atproto) OAuth for the admin portal.
//!
//! OAuth is used purely for identity: the flow proves control of a DID, we
//! check that DID against the curator roster, mint our own session cookie,
//! and immediately discard the atproto tokens — this app never acts on
//! anyone's PDS.
//!
//! Currently configured as an atproto *loopback* client (client_id derived
//! from `http://localhost`), which authorization servers accept only for
//! 127.0.0.1 redirect URIs — right for dev and tailnet use via a local port
//! forward. The hosted, confidential-client metadata (public client_id URL +
//! JWKS) lands with the hosting setup, since it requires the public domain.

use std::sync::Arc;

use atrium_api::types::string::Did;
use atrium_identity::{
    did::{CommonDidResolver, CommonDidResolverConfig},
    handle::{AppViewHandleResolver, AppViewHandleResolverConfig},
};
use atrium_oauth::{
    AtprotoLocalhostClientMetadata, KnownScope, OAuthClient, OAuthClientConfig,
    OAuthResolverConfig, Scope,
    store::{
        session::{Session, SessionStore},
        state::{InternalStateData, StateStore},
    },
};
use axum::{
    extract::FromRequestParts,
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

/// Build the loopback-mode OAuth client. `callback_url` must use a loopback
/// host (e.g. `http://127.0.0.1:4601/admin/oauth/callback`).
pub fn build_oauth_client(pool: PgPool, callback_url: String) -> anyhow::Result<AtriumOAuthClient> {
    let http_client = HttpClient::default();
    let arced = Arc::new(http_client.clone());
    let config = OAuthClientConfig {
        client_metadata: AtprotoLocalhostClientMetadata {
            redirect_uris: Some(vec![callback_url]),
            scopes: Some(vec![Scope::Known(KnownScope::Atproto)]),
        },
        keys: None,
        resolver: OAuthResolverConfig {
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
        },
        state_store: PgStateStore { pool: pool.clone() },
        session_store: PgSessionStore { pool },
        http_client,
    };
    OAuthClient::new(config).map_err(|e| anyhow::anyhow!("failed to build oauth client: {e}"))
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
