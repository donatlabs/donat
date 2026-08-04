//! The S3-compatible object store the file-attachment suites run against.
//!
//! It is a real MinIO from `docker-compose.conformance.yml`, not a double. The
//! engine has no local-disk backend, so "the upload worked" has to mean an
//! actual S3 implementation accepted our presigned URL — a hand-written stub
//! could only prove the signature is canonical by our own reading of the spec.
//!
//! Two buckets, matching how a deployment would separate them: one private, and
//! one served anonymously for attachments declared public.

/// Credentials and buckets created by the compose file's one-shot init step.
pub const ACCESS_KEY_ID: &str = "donatconformance";
pub const SECRET_ACCESS_KEY: &str = "donatconformancesecret";
pub const BUCKET: &str = "donat-test";
pub const PUBLIC_BUCKET: &str = "donat-public";
pub const REGION: &str = "us-east-1";

/// Where the store listens, from `S3_URL` (default matching the compose file).
pub fn endpoint() -> String {
    std::env::var("S3_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:19000".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// The anonymous origin a public attachment's URL is rooted at.
pub fn public_base_url() -> String {
    format!("{}/{PUBLIC_BUCKET}", endpoint())
}

/// Fail early and clearly when the store is not running: every file-attachment
/// case needs it, and a connection error inside a round trip reads like an
/// engine bug.
pub fn require_running() {
    let url = format!("{}/minio/health/live", endpoint());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("http client");
    match client.get(&url).send() {
        Ok(response) if response.status().is_success() => {}
        other => panic!(
            "the conformance object store is not reachable at {} ({other:?}).\n\
             Start it with: docker compose -f docker-compose.conformance.yml up -d minio minio-init",
            endpoint()
        ),
    }
}
