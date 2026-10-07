//! 命令账本：事务由调用方拥有，处理器只在首次受理时运行。
use nd_store::{OptionalExtension, Tx, params};
use nd_wire::{Command, CommandReply, Receipt, ReceiptLookup};

pub fn migrate(tx: &mut Tx<'_>) -> nd_store::Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS command_receipts (
        id TEXT PRIMARY KEY, digest TEXT NOT NULL, receipt TEXT, expires_ms INTEGER NOT NULL
    ); CREATE INDEX IF NOT EXISTS receipt_expiry ON command_receipts(expires_ms) WHERE receipt IS NOT NULL;")?;
    Ok(())
}

/// 保留 id 和散列，正文清理之后永久阻止重复执行。
pub fn expire(tx: &mut Tx<'_>) -> nd_store::Result<()> {
    tx.execute(
        "UPDATE command_receipts SET receipt=NULL WHERE receipt IS NOT NULL AND expires_ms<=?1",
        [now_ms()],
    )?;
    Ok(())
}

/// 效果、拒绝或受理收据使用同一个事务。闭包禁止外部 I/O；出错由外层回滚。
/// 延迟动作须由其主人同事务保存意图，收到结果后才签发 Delivery 收据（见 [`begin`]、[`record`]）。
pub fn execute(
    tx: &mut Tx<'_>,
    command: &Command,
    keep_ms: u64,
    apply: impl FnOnce(&mut Tx<'_>) -> nd_store::Result<Receipt>,
) -> nd_store::Result<CommandReply> {
    let receipt = match begin(tx, command)? {
        Begin::Prior(reply) => return Ok(reply),
        Begin::Invalid => invalid(),
        Begin::New => apply(tx)?,
    };
    record(tx, &command.id, &command.content_hash(), &receipt, keep_ms)?;
    Ok(CommandReply::Receipt { receipt })
}

/// 一条命令此刻在账本里的样子。
pub enum Begin {
    /// 已有收据（同内容回原收据）、内容冲突或已过期：不再执行。
    Prior(CommandReply),
    /// 信封不合法：调用方记下 [`invalid`] 收据。
    Invalid,
    /// 第一次见到：调用方执行。
    New,
}

/// 查账本，不写。收据等动作有结果的命令（Delivery）先用它判重，效果的主人同事务保存意图，
/// 等结果到了再在那个事务里 [`record`]。
pub fn begin(tx: &Tx<'_>, command: &Command) -> nd_store::Result<Begin> {
    let digest = command.content_hash();
    let prior: Option<(String, Option<String>, u64)> = tx
        .query_row(
            "SELECT digest,receipt,expires_ms FROM command_receipts WHERE id=?1",
            [&command.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((old_digest, prior, expires)) = prior {
        if old_digest != digest {
            return Ok(Begin::Prior(CommandReply::Conflict));
        }
        return Ok(Begin::Prior(match prior.filter(|_| expires > now_ms()) {
            Some(prior) => CommandReply::Receipt {
                receipt: decode(&prior)?,
            },
            None => CommandReply::Expired,
        }));
    }
    if command.id.is_empty()
        || command.id.len() > 256
        || command.device.is_empty()
        || command.device.len() > 256
        || command.name.is_empty()
        || command.name.len() > 256
    {
        return Ok(Begin::Invalid);
    }
    Ok(Begin::New)
}

pub fn invalid() -> Receipt {
    Receipt::Rejected {
        code: "invalid".into(),
        now: serde_json::Value::Null,
    }
}

/// 落下一条命令的永久收据；`digest` 是命令的内容散列。只在调用方的事务里写一次。
pub fn record(
    tx: &Tx<'_>,
    id: &str,
    digest: &str,
    receipt: &Receipt,
    keep_ms: u64,
) -> nd_store::Result<()> {
    tx.execute(
        "INSERT INTO command_receipts(id,digest,receipt,expires_ms) VALUES(?1,?2,?3,?4)",
        params![
            id,
            digest,
            serde_json::to_string(receipt).unwrap(),
            now_ms().saturating_add(keep_ms)
        ],
    )?;
    Ok(())
}

/// 查询使用只读 WAL 连接，不为读开启写事务，也不延长保留期。
pub fn lookup(
    store: &nd_store::Store,
    id: &str,
    content_hash: Option<&str>,
) -> nd_store::Result<ReceiptLookup> {
    let prior: Option<(Option<String>, u64, String)> = store
        .read()?
        .query_row(
            "SELECT receipt,expires_ms,digest FROM command_receipts WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match prior {
        Some((_, _, digest)) if content_hash.is_some_and(|hash| hash != digest) => {
            ReceiptLookup::Conflict
        }
        Some((Some(prior), expires, _)) if expires > now_ms() => ReceiptLookup::Found {
            receipt: decode(&prior)?,
        },
        Some(_) => ReceiptLookup::Expired,
        None => ReceiptLookup::Missing,
    })
}

fn decode(text: &str) -> nd_store::Result<Receipt> {
    serde_json::from_str(text).map_err(|_| nd_store::Error::Corrupt)
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
