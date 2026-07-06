//! The vault: Bunny Storage holds the museum's own copies of the archive —
//! PDS-pulled originals under `pds/`, artist-uploaded masters under
//! `masters/`. A pull zone in front serves them (PCG_MEDIA_BASE_URL); the
//! Bluesky CDN remains the fallback for anything the vault doesn't hold.

use anyhow::Context;

/// Bunny Edge Storage credentials. Present wherever uploads happen: the
/// pull-media host (syncing PDS originals) and the web app (masters).
pub struct BunnyConfig {
    endpoint: String,
    zone: String,
    access_key: String,
}

impl BunnyConfig {
    /// Reads PCG_BUNNY_STORAGE_ZONE + PCG_BUNNY_STORAGE_KEY (the storage
    /// zone's read-write password), and optionally
    /// PCG_BUNNY_STORAGE_ENDPOINT for a non-default storage region.
    /// `None` disables the vault entirely — everything falls back.
    pub fn from_env() -> Option<Self> {
        let zone = std::env::var("PCG_BUNNY_STORAGE_ZONE").ok()?;
        let access_key = std::env::var("PCG_BUNNY_STORAGE_KEY").ok()?;
        let endpoint = std::env::var("PCG_BUNNY_STORAGE_ENDPOINT")
            .unwrap_or_else(|_| "https://storage.bunnycdn.com".to_string());
        Some(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            zone,
            access_key,
        })
    }

    fn object_url(&self, key: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.zone, key)
    }

    /// Upload one local file to the vault. Bunny types objects by file
    /// extension on the CDN side, so keys must keep their real extensions.
    pub async fn put_file(
        &self,
        client: &reqwest::Client,
        key: &str,
        path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let file = tokio::fs::File::open(path)
            .await
            .with_context(|| format!("opening {} for vault upload", path.display()))?;
        let len = file.metadata().await?.len();
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
        let response = client
            .put(self.object_url(key))
            .header("AccessKey", &self.access_key)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "bunny PUT {key} failed: {}",
            response.status()
        );
        Ok(())
    }
}

/// Public base URL of the pull zone in front of the vault
/// (e.g. https://media.fluoddity.observer). `None` means the site keeps
/// serving from the Bluesky CDN even if keys are recorded.
pub fn media_base_from_env() -> Option<String> {
    std::env::var("PCG_MEDIA_BASE_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
}

/// Vault key for a PDS-pulled original, mirroring the archive filename
/// (which already carries rkey + full cid + real extension).
pub fn pds_key(archive_file: &str) -> String {
    let name = archive_file.rsplit('/').next().unwrap_or(archive_file);
    format!("pds/{name}")
}

/// Vault key for an artist-uploaded master. One slot per specimen — a
/// re-upload replaces the previous master.
pub fn master_key(rkey: &str, ext: &str) -> String {
    format!("masters/{rkey}.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pds_keys_mirror_the_archive_filename() {
        assert_eq!(
            pds_key("videos/3mabc_bafkreiverylongcid.mp4"),
            "pds/3mabc_bafkreiverylongcid.mp4"
        );
        assert_eq!(pds_key("bare.mp4"), "pds/bare.mp4");
    }

    #[test]
    fn master_keys_are_one_slot_per_specimen() {
        assert_eq!(master_key("3mabc", "mp4"), "masters/3mabc.mp4");
    }
}
