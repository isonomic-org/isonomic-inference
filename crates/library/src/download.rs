// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Model download: checksum-verified fetch with progress reporting, cached
//! under a models directory chosen by the caller.
//!
//! The file a caller receives has always been hashed in full: a download is
//! written to a `.part` file and renamed only after its SHA-256 matches, and
//! a cached file is hashed again before it is reused.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

/// A downloadable model file with its integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// File name under the cache directory.
    pub file_name: String,
    /// Direct download URL.
    pub url: String,
    /// Expected SHA-256 of the file, lowercase hex.
    pub sha256_hex: String,
    /// Expected size in bytes (used for progress totals when the server omits Content-Length).
    pub size_bytes: u64,
}

/// Progress events reported during `ensure_model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadProgress {
    Started {
        total_bytes: u64,
    },
    Received {
        bytes: u64,
        total_bytes: u64,
    },
    Verifying,
    /// A cached, checksum-verified copy was reused; no download happened.
    Cached,
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("download failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },
}

/// Ensure `spec` is present and checksum-verified under `cache_dir`, downloading if absent.
/// Returns the path to the verified model file.
pub async fn ensure_model(
    spec: &ModelSpec,
    cache_dir: &Path,
    progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf, ModelError> {
    ensure_model_with(&reqwest::Client::new(), spec, cache_dir, progress).await
}

/// [`ensure_model`] with a caller-supplied HTTP client (proxy and timeout
/// policy belong to the caller).
pub async fn ensure_model_with(
    client: &reqwest::Client,
    spec: &ModelSpec,
    cache_dir: &Path,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf, ModelError> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let dest = cache_dir.join(&spec.file_name);
    if tokio::fs::try_exists(&dest).await? {
        progress(DownloadProgress::Verifying);
        if file_sha256(&dest).await? == spec.sha256_hex {
            progress(DownloadProgress::Cached);
            return Ok(dest);
        }
        // Corrupt or truncated cache entry: discard and re-download.
        tokio::fs::remove_file(&dest).await?;
    }

    let part = cache_dir.join(format!("{}.part", spec.file_name));
    let response = client.get(&spec.url).send().await?.error_for_status()?;
    let total_bytes = response.content_length().unwrap_or(spec.size_bytes);
    progress(DownloadProgress::Started { total_bytes });

    let mut hasher = Sha256::new();
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = response.bytes_stream();
    let mut received: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        received += chunk.len() as u64;
        progress(DownloadProgress::Received {
            bytes: received,
            total_bytes,
        });
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);

    progress(DownloadProgress::Verifying);
    let actual = hex::encode(hasher.finalize());
    if actual != spec.sha256_hex {
        tokio::fs::remove_file(&part).await?;
        return Err(ModelError::ChecksumMismatch {
            name: spec.file_name.clone(),
            expected: spec.sha256_hex.clone(),
            actual,
        });
    }
    tokio::fs::rename(&part, &dest).await?;
    progress(DownloadProgress::Done);
    Ok(dest)
}

async fn file_sha256(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::{ensure_model_with, DownloadProgress, ModelError, ModelSpec};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Tests must not be redirected by proxy variables in the environment.
    async fn ensure_model(
        spec: &ModelSpec,
        cache_dir: &Path,
        progress: impl FnMut(DownloadProgress),
    ) -> Result<PathBuf, ModelError> {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        ensure_model_with(&client, spec, cache_dir, progress).await
    }

    fn serve_body_once(
        listener: tokio::net::TcpListener,
        body: Vec<u8>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn downloads_verifies_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"fake-gguf-bytes".to_vec();
        let sha = hex::encode(Sha256::digest(&body));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve_body_once(listener, body.clone());

        let spec = ModelSpec {
            file_name: "test.gguf".to_string(),
            url: format!("http://{addr}/test.gguf"),
            sha256_hex: sha,
            size_bytes: body.len() as u64,
        };
        let mut events = Vec::new();
        let path = ensure_model(&spec, dir.path(), |p| events.push(p))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(events
            .iter()
            .any(|e| matches!(e, DownloadProgress::Started { .. })));
        assert!(events.iter().any(|e| matches!(e, DownloadProgress::Done)));
        server.await.unwrap();

        // Second call reuses the cache: no server is listening any more.
        let mut events2 = Vec::new();
        let path2 = ensure_model(&spec, dir.path(), |p| events2.push(p))
            .await
            .unwrap();
        assert_eq!(path, path2);
        assert!(events2
            .iter()
            .any(|e| matches!(e, DownloadProgress::Cached)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_a_checksum_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"tampered-bytes".to_vec();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = serve_body_once(listener, body);

        let spec = ModelSpec {
            file_name: "bad.gguf".to_string(),
            url: format!("http://{addr}/bad.gguf"),
            sha256_hex: "0".repeat(64),
            size_bytes: 14,
        };
        let err = ensure_model(&spec, dir.path(), |_| {}).await.unwrap_err();
        assert!(matches!(err, ModelError::ChecksumMismatch { .. }));
        assert!(!dir.path().join("bad.gguf").exists());
        assert!(!dir.path().join("bad.gguf.part").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_corrupt_cached_file_is_reverified_discarded_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"the-real-model".to_vec();
        let sha = hex::encode(Sha256::digest(&body));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve_body_once(listener, body.clone());
        let spec = ModelSpec {
            file_name: "cached.gguf".to_string(),
            url: format!("http://{addr}/cached.gguf"),
            sha256_hex: sha,
            size_bytes: body.len() as u64,
        };

        // A file with the right name and the wrong bytes is already cached.
        std::fs::write(dir.path().join("cached.gguf"), b"tampered").unwrap();

        let mut events = Vec::new();
        let path = ensure_model(&spec, dir.path(), |p| events.push(p))
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        // It was hashed before use, found wrong, and downloaded again.
        assert_eq!(events.first(), Some(&DownloadProgress::Verifying));
        assert!(!events.contains(&DownloadProgress::Cached));
        assert!(events.contains(&DownloadProgress::Done));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_corrupt_cached_file_is_never_handed_back_when_the_download_fails() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens here, so the replacement download cannot succeed.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = dead.local_addr().unwrap();
        drop(dead);
        let spec = ModelSpec {
            file_name: "cached.gguf".to_string(),
            url: format!("http://{addr}/cached.gguf"),
            sha256_hex: "0".repeat(64),
            size_bytes: 8,
        };
        std::fs::write(dir.path().join("cached.gguf"), b"tampered").unwrap();

        let err = ensure_model(&spec, dir.path(), |_| {}).await.unwrap_err();
        assert!(matches!(err, ModelError::Http(_)));
        assert!(!dir.path().join("cached.gguf").exists());
        assert!(!dir.path().join("cached.gguf.part").exists());
    }
}
