#[tokio::main]
async fn main() -> nd_daemon::Result<()> {
    let mut args = std::env::args().skip(1);
    let paths = match (args.next().as_deref(), args.next()) {
        (Some("--root"), Some(root)) if args.next().is_none() => {
            nd_daemon::Paths::isolated(std::path::Path::new(&root))
        }
        (None, None) => nd_daemon::Paths::from_env()?,
        _ => return Err("usage: nd-daemon [--root PATH]".into()),
    };
    nd_daemon::run_at(paths).await
}
