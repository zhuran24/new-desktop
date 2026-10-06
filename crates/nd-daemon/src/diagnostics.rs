//! 可选诊断组件；持久备注用于标注本机诊断状态。
use super::NamespaceProvider;
use nd_kernel::Lifecycle;
use nd_wire::{Fallback, Item};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub(super) fn migrate(tx: &mut nd_store::Tx<'_>) -> nd_store::Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS diagnostic_note(id INTEGER PRIMARY KEY CHECK(id=1), text TEXT NOT NULL, revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO diagnostic_note VALUES(1,'',0);")?;
    Ok(())
}
pub(super) struct Diagnostics {
    pub(super) store: Arc<nd_store::Store>,
}
impl Diagnostics {
    fn note(&self) -> nd_store::Result<Value> {
        Ok(self.store.read()?.query_row(
            "SELECT text,revision FROM diagnostic_note WHERE id=1",
            [],
            |r| Ok(json!({"text":r.get::<_,String>(0)?,"revision":r.get::<_,u64>(1)?})),
        )?)
    }
}
impl Lifecycle for Diagnostics {}
impl NamespaceProvider for Diagnostics {
    fn names(&self) -> BTreeMap<String, u32> {
        BTreeMap::from([("diagnostics".into(), 1)])
    }
    fn snapshot(&self) -> Vec<Item> {
        vec![Item {
            id: "diagnostics".into(),
            namespace: "diagnostics".into(),
            kind: "status".into(),
            data: json!({"commands":["diagnostics.inspect", "diagnostics.set_note"],"note":self.note().unwrap_or(Value::Null)}),
            fallback: Fallback {
                title: "诊断".into(),
                text: "可选诊断组件已启用".into(),
            },
        }]
    }
    fn command(&self, name: &str) -> std::result::Result<Value, String> {
        if name == "diagnostics.inspect" {
            Ok(json!({"status":"ready"}))
        } else {
            Err("not_found".into())
        }
    }
    fn execute(
        &self,
        tx: &mut nd_store::Tx<'_>,
        command: &nd_wire::Command,
    ) -> nd_store::Result<nd_wire::Receipt> {
        if command.name != "diagnostics.set_note" {
            return Ok(nd_wire::Receipt::Rejected {
                code: "not_found".into(),
                now: Value::Null,
            });
        }
        if command.args.as_object().is_none_or(|v| v.len() != 1)
            || command.expect.as_object().is_none_or(|v| v.len() != 1)
            || command.expect["revision"].as_u64().is_none()
            || command.args["text"]
                .as_str()
                .is_none_or(|s| s.len() > 65536)
        {
            return Ok(nd_wire::Receipt::Rejected {
                code: "invalid".into(),
                now: Value::Null,
            });
        }
        let revision: u64 =
            tx.query_row("SELECT revision FROM diagnostic_note WHERE id=1", [], |r| {
                r.get(0)
            })?;
        if command.expect["revision"].as_u64() != Some(revision) {
            return Ok(nd_wire::Receipt::Rejected {
                code: "precondition".into(),
                now: json!({"revision":revision}),
            });
        }
        let Some(text) = command.args["text"].as_str() else {
            return Ok(nd_wire::Receipt::Rejected {
                code: "invalid".into(),
                now: Value::Null,
            });
        };
        tx.execute(
            "UPDATE diagnostic_note SET text=?1,revision=revision+1 WHERE id=1",
            [text],
        )?;
        Ok(nd_wire::Receipt::Done {
            value: json!({"revision":revision+1}),
        })
    }
}
