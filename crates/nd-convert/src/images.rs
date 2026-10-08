use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};

/// 字节由适配器在 Export 时读取；转换器永远不访问路径或 URL。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenImage {
    pub media_type: String,
    pub data: String,
    pub sha256: nd_id::BlobId,
}

impl FrozenImage {
    pub fn from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            media_type: media_type.into(),
            data: STANDARD.encode(bytes),
            sha256: nd_id::BlobId::of(bytes),
        }
    }
}

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

pub(crate) fn unavailable(label: &str) -> Part {
    Part::Notice {
        reason: "image_unavailable".into(),
        value: json!(label),
    }
}

pub(crate) fn inline(media: &str, data: &str) -> Part {
    if !matches!(
        media,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    ) || data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
    {
        return unavailable("unsupported media or image exceeds 5 MiB");
    }
    match STANDARD.decode(data) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES => Part::Image {
            media_type: media.into(),
            data: STANDARD.encode(bytes),
        },
        _ => unavailable("invalid or oversized image bytes"),
    }
}

pub(crate) fn reference(input: &FrozenInput, reference: &str) -> Result<Part, ConvertError> {
    if let Some((media, data)) = reference
        .strip_prefix("data:")
        .and_then(|s| s.split_once(";base64,"))
    {
        return Ok(inline(media, data));
    }
    let Some(image) = input.images.get(reference) else {
        return Ok(unavailable(reference));
    };
    let bytes = STANDARD
        .decode(&image.data)
        .map_err(|_| ConvertError::Invalid("invalid frozen image base64".into()))?;
    if nd_id::BlobId::of(&bytes) != image.sha256 {
        return Err(ConvertError::Invalid("frozen image hash mismatch".into()));
    }
    Ok(inline(&image.media_type, &image.data))
}

/// One reference vocabulary for decoding and synchronization fingerprints.
pub(crate) fn reference_key(block: &Value) -> Option<&str> {
    match block["type"].as_str() {
        Some("localImage") => block["path"].as_str(),
        Some("input_image") => block["image_url"]
            .as_str()
            .or_else(|| block["file_id"].as_str()),
        Some("image") => block["url"]
            .as_str()
            .or_else(|| block["source"]["url"].as_str())
            .or_else(|| block["fileId"].as_str())
            .or_else(|| block["source"]["file_id"].as_str()),
        _ => None,
    }
}

pub(crate) fn decode_reference(input: &FrozenInput, block: &Value) -> Result<Part, ConvertError> {
    match reference_key(block) {
        Some(key) => reference(input, key),
        None => Ok(unavailable(&block.to_string())),
    }
}

pub(crate) fn prefix_hash(input: &FrozenInput, count: usize) -> String {
    fn walk<'a>(v: &'a Value, refs: &mut BTreeSet<&'a str>) {
        if let Some(s) = reference_key(v) {
            refs.insert(s);
        }
        match v {
            Value::Array(a) => {
                for child in a {
                    walk(child, refs);
                }
            }
            Value::Object(m) => {
                for child in m.values() {
                    walk(child, refs);
                }
            }
            _ => {}
        }
    }
    let mut refs = BTreeSet::new();
    for item in &input.items[..count] {
        walk(&item.payload, &mut refs);
    }
    let images: Vec<_> = refs
        .into_iter()
        .map(|key| (key, input.images.get(key)))
        .collect();
    hash(&(&input.items[..count], images))
}
