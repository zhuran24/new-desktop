use nd_ui_core::SyncReplica;
use std::io::Write;
#[tokio::main]
async fn main() -> nd_ui_core::Result<()> {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    let (socket, operation) = if first.as_deref() == Some("--socket") {
        (
            std::path::PathBuf::from(args.next().ok_or("missing socket")?),
            args.next(),
        )
    } else {
        (
            std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR")?).join("new-desktop/nd.sock"),
            first,
        )
    };
    let stream = args.next().unwrap_or("global".into());
    let operation = operation.as_deref().unwrap_or("get");
    if !matches!(operation, "get" | "watch" | "page") || args.next().is_some() {
        return Err("usage: ndctl [--socket PATH] get|watch|page [global|resource]".into());
    }
    let mut replica = SyncReplica::connect(socket).await?;
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
