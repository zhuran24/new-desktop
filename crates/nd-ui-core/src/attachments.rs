use nd_wire::Attachment;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachmentSource {
    Path(PathBuf),
    Bytes { name: String, bytes: Vec<u8> },
}
impl AttachmentSource {
    pub(crate) async fn read(self) -> Result<(Attachment, Vec<u8>), String> {
        let (name, bytes) = match self {
            Self::Bytes { name, bytes } => (name, bytes),
            Self::Path(path) => {
                let file = tokio::fs::File::open(&path)
                    .await
                    .map_err(|e| format!("{}：{e}", path.display()))?;
                let metadata = file.metadata().await.map_err(|e| e.to_string())?;
                if !metadata.is_file() {
                    return Err("附件必须是普通文件".into());
                }
                if metadata.len() > nd_wire::MAX_ATTACHMENT_BYTES {
                    return Err("单个附件不能超过 5 MiB".into());
                }
                let mut bytes = vec![];
                file.take(nd_wire::MAX_ATTACHMENT_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                (
                    path.file_name()
                        .ok_or("附件没有文件名")?
                        .to_string_lossy()
                        .into_owned(),
                    bytes,
                )
            }
        };
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            "image/webp"
        } else if bytes.starts_with(b"%PDF-") {
            "application/pdf"
        } else if std::str::from_utf8(&bytes).is_ok_and(|s| {
            !s.chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        }) {
            "text/plain"
        } else {
            return Err("不支持此文件：请使用 PNG、JPEG、GIF、WebP、PDF 或 UTF-8 文本".into());
        };
        let attachment = Attachment {
            blob: nd_wire::BlobId::of(&bytes),
            name,
            media_type: media_type.into(),
            size: bytes.len() as u64,
        };
        attachment.validate()?;
        Ok((attachment, bytes))
    }
}
