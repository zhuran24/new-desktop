//! Stable 128-bit identifiers derived from a domain-prefixed key.
pub(crate) fn derive_id(domain: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{domain}{key}").as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
