//! ndctl：和界面用同一个同步副本连守护进程的命令行，手动复现和自动测试走同一条路。
use nd_ui_core::SyncReplica;
use nd_wire::{Command, CommandReply, Item, Receipt, Snapshot};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, time::Duration};

const USAGE: &str = "usage: ndctl [--socket PATH] get|watch|page [resource]
       ndctl [--socket PATH] command JSON | receipt COMMAND_ID
       ndctl [--socket PATH] new [--cwd DIR] [--model M] [--permission-mode MODE] [--follow] TEXT
       ndctl [--socket PATH] send SESSION [--intent fold|after_turn|interrupting] [--follow] TEXT";

#[tokio::main]
async fn main() -> nd_ui_core::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let socket = if args.first().map(String::as_str) == Some("--socket") {
        if args.len() < 2 {
            return Err(USAGE.into());
        }
        let socket = std::path::PathBuf::from(args.remove(1));
        args.remove(0);
        socket
    } else {
        std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?).join("new-desktop/nd.sock")
    };
    let operation = if args.is_empty() {
        "get".to_owned()
    } else {
        args.remove(0)
    };
    match operation.as_str() {
        "new" | "send" => return converse(socket, &operation, args).await,
        "get" | "watch" | "page" | "command" | "receipt" if args.len() <= 1 => {}
        _ => return Err(USAGE.into()),
    }
    let argument = args.pop();
    let mut replica = SyncReplica::connect(socket).await?;
    if operation == "command" {
        let command = serde_json::from_str::<Command>(&argument.ok_or("missing command JSON")?)?;
        println!(
            "{}",
            serde_json::to_string(&replica.command(&command).await?)?
        );
        return Ok(());
    }
    if operation == "receipt" {
        println!(
            "{}",
            serde_json::to_string(
                &replica
                    .receipt(&argument.ok_or("missing command id")?)
                    .await?
            )?
        );
        return Ok(());
    }
    let stream = argument.unwrap_or("global".into());
    if operation == "page" {
        println!(
            "{}",
            serde_json::to_string(&replica.get(&stream, nd_wire::PageReq::default()).await?)?
        );
        return Ok(());
    }
    let first = replica.subscribe(&stream).await?;
    println!("{}", serde_json::to_string(&first)?);
    std::io::stdout().flush()?;
    if operation == "watch" {
        loop {
            println!("{}", serde_json::to_string(&replica.next().await?)?);
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}

/// 新建会话或发一条消息；`--follow` 时接着把会话流里变了的条目逐行打出来（含流式文字），
/// 直到这一轮安静下来：没有没结论的消息、回合结束，或会话撤掉、部分完成。
async fn converse(
    socket: std::path::PathBuf,
    operation: &str,
    mut args: Vec<String>,
) -> nd_ui_core::Result<()> {
    let mut options = BTreeMap::new();
    let mut follow = false;
    let mut positional = vec![];
    while !args.is_empty() {
        let arg = args.remove(0);
        match arg.as_str() {
            "--follow" => follow = true,
            "--cwd" | "--model" | "--permission-mode" | "--intent" | "--timeout" => {
                if args.is_empty() {
                    return Err(USAGE.into());
                }
                options.insert(arg.trim_start_matches("--").to_owned(), args.remove(0));
            }
            _ => positional.push(arg),
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (name, session, payload) = match (operation, positional.as_slice()) {
        ("new", [text]) => {
            let cwd = match options.get("cwd") {
                Some(cwd) => cwd.clone(),
                None => std::env::current_dir()?.to_string_lossy().into_owned(),
            };
            let mut args = json!({"backend":"claude","cwd":cwd,"text":text});
            for key in ["model", "permission-mode"] {
                if let Some(value) = options.get(key) {
                    args[key.replace('-', "_")] = json!(value);
                }
            }
            ("session.create", None, args)
        }
        ("send", [session, text]) => (
            "session.send",
            Some(session.clone()),
            json!({"session":session,"text":text,"intent":options.get("intent").map_or("fold", String::as_str)}),
        ),
        _ => return Err(USAGE.into()),
    };
    let mut replica = SyncReplica::connect(&socket).await?;
    let reply = replica
        .command(&Command {
            id: id.clone(),
            device: "ndctl".into(),
            name: name.into(),
            args: payload,
            expect: json!({}),
        })
        .await?;
    let session = match (&reply, session) {
        (_, Some(session)) => Some(session),
        (
            CommandReply::Receipt {
                receipt:
                    Receipt::Accepted {
                        stream: Some(stream),
                        ..
                    },
            },
            None,
        ) => stream.strip_prefix("session/").map(str::to_owned),
        _ => None,
    };
    println!(
        "{}",
        json!({"command": id, "session": session, "reply": reply})
    );
    std::io::stdout().flush()?;
    let (true, Some(session)) = (follow, session) else {
        return Ok(());
    };
    let timeout = options
        .get("timeout")
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(120);
    let stream = format!("session/{session}");
    let follow = async {
        let mut seen: BTreeMap<String, Item> = BTreeMap::new();
        let mut snapshot = replica.subscribe(&stream).await?;
        loop {
            for item in &snapshot.items {
                if seen.get(&item.id) != Some(item) {
                    println!("{}", json!({"item": item}));
                    seen.insert(item.id.clone(), item.clone());
                }
            }
            std::io::stdout().flush()?;
            if quiet(&snapshot) {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
            }
            snapshot = replica.next().await?;
        }
    };
    tokio::time::timeout(Duration::from_secs(timeout), follow)
        .await
        .map_err(|_| "timed out following the session")??;
    Ok(())
}

fn quiet(snapshot: &Snapshot) -> bool {
    let header = snapshot
        .items
        .iter()
        .find(|i| i.id == "header")
        .map(|i| &i.data);
    let Some(header) = header else {
        return false;
    };
    match header["status"].as_str() {
        Some("withdrawn" | "partial") => return true,
        Some("active") => {}
        _ => return false,
    }
    if header["process"]["turn_running"] == true || !header["op"].is_null() {
        return false;
    }
    let seq = |i: &Item| i.data["seq"].as_u64().unwrap_or(0);
    let prompts: Vec<&Item> = snapshot
        .items
        .iter()
        .filter(|i| i.kind == "prompt")
        .collect();
    if prompts.iter().any(|p| {
        !matches!(
            p.data["state"].as_str(),
            Some("landed" | "failed" | "unknown")
        )
    }) {
        return false;
    }
    let last_landed = prompts
        .iter()
        .filter(|p| p.data["state"] == "landed")
        .map(|p| seq(p))
        .max();
    match last_landed {
        None => true,
        Some(at) => snapshot
            .items
            .iter()
            .any(|i| i.kind == "turn" && seq(i) > at && i.data != Value::Null),
    }
}
