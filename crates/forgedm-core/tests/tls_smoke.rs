//! S0.1 smoke test: the pinned reqwest feature set builds a client with the
//! ForgeDM transfer configuration (HTTP/1 only, no transparent decompression,
//! rustls TLS backend).

#[test]
fn client_builds_with_configured_tls_provider() {
    let client = reqwest::Client::builder()
        .http1_only()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .tls_backend_rustls()
        .build();
    assert!(client.is_ok(), "client build failed: {:?}", client.err());
}
