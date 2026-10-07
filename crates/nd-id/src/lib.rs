//! 不依赖协议或存储的内容地址。
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct BlobId(String);

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("内容地址须为 64 位小写十六进制 SHA-256")]
pub struct InvalidBlobId;
impl BlobId {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::str::FromStr for BlobId {
    type Err = InvalidBlobId;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidBlobId);
        }
        Ok(Self(value.into()))
    }
}
impl TryFrom<String> for BlobId {
    type Error = InvalidBlobId;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<BlobId> for String {
    fn from(value: BlobId) -> Self {
        value.0
    }
}
impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl AsRef<str> for BlobId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_address_preserves_wire_string_and_rejects_invalid_paths_and_case() {
        let id = BlobId::of(b"abc");
        assert_eq!(
            id.as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(serde_json::to_value(&id).unwrap(), id.as_str());
        assert_eq!(
            serde_json::from_value::<BlobId>(serde_json::json!(id.as_str())).unwrap(),
            id
        );
        for invalid in [
            "../state.sqlite".to_owned(),
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("+{}", "a".repeat(63)),
        ] {
            assert!(invalid.parse::<BlobId>().is_err());
            assert!(serde_json::from_value::<BlobId>(serde_json::json!(invalid)).is_err());
        }
    }
}
