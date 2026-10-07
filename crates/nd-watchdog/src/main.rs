mod journal;
use journal::Journal;
use nd_watchdog_proto::*;
use std::{os::unix::fs::OpenOptionsExt, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    sync::{Mutex, mpsc, oneshot},
};
struct State {
    hello: Hello,
    journal: Journal,
}
impl State {
    fn hello(&self) -> Hello {
        let mut h = self.hello.clone();
        h.high = self.journal.high;
        h
    }
}
type Input = (Request, oneshot::Sender<Response>);
#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 || args[1] != "--spec" {
        return Err("usage: nd-watchdog --spec PATH".into());
    }
    let spec: WatchSpec = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(spec.directory.join("watchdog.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    // Never replay a backend launch when the watchdog itself dies.
    let _once = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(spec.directory.join("started"))?;
    let prepare = (|| -> Result<_> {
        Ok((
            Journal::new(&spec.directory, spec.launch.limits.clone())?,
            UnixListener::bind(spec.directory.join("watchdog.sock"))?,
        ))
    })();
    let (journal, listener) = match prepare {
        Ok(v) => v,
        Err(e) => {
            private_json(&spec.directory.join("failed.json"), &e.to_string())?;
            return Err(e);
        }
    };
    let child = tokio::process::Command::new(spec.launch.argv.first().ok_or("empty argv")?)
        .args(&spec.launch.argv[1..])
        .env_clear()
        .envs(&spec.launch.env)
        .env_remove("BUN_OPTIONS")
        .current_dir(&spec.launch.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            private_json(&spec.directory.join("failed.json"), &e.to_string())?;
            return Err(e.into());
        }
    };
    let hello = Hello {
        version: VERSION,
        run: spec.run,
        identity: Identity::read(child.id().ok_or("no child pid")?)?,
        watchdog: Identity::read(std::process::id())?,
        high: 0,
        written: 0,
        accepted: 0,
        exit: None,
    };
    private_json(&spec.directory.join("hello.json"), &hello)?;
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin = Some(child.stdin.take().unwrap());
    let state = Arc::new(Mutex::new(State { hello, journal }));
    let out_state = state.clone();
    let mut out = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = vec![];
            let n = (&mut reader)
                .take((MAX_FRAME / 4) as u64)
                .read_until(b'\n', &mut bytes)
                .await?;
            if n == 0 {
                break;
            }
            let event = if !bytes.ends_with(b"\n") && n == MAX_FRAME / 4 {
                loop {
                    let mut skip = vec![];
                    let n = (&mut reader)
                        .take(16384)
                        .read_until(b'\n', &mut skip)
                        .await?;
                    if n == 0 || skip.ends_with(b"\n") {
                        break;
                    }
                }
                Event::Gap {
                    reason: GapReason::LostLines,
                }
            } else {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                match String::from_utf8(bytes) {
                    Ok(line) => Event::Out { line },
                    Err(_) => Event::Gap {
                        reason: GapReason::LostLines,
                    },
                }
            };
            let _ = out_state.lock().await.journal.append(event);
        }
        Ok::<_, std::io::Error>(())
    });
    let err_state = state.clone();
    let mut err = tokio::spawn(async move {
        let mut bytes = [0; 4096];
        loop {
            let n = stderr.read(&mut bytes).await?;
            if n == 0 {
                break;
            }
            let _ = err_state.lock().await.journal.append(Event::Err {
                chunk: String::from_utf8_lossy(&bytes[..n]).into(),
            });
        }
        Ok::<_, std::io::Error>(())
    });
    // This writer outlives a control connection: disconnect cannot cancel a partially written line.
    let (inputs, mut input_rx) = mpsc::channel::<Input>(8);
    let input_state = state.clone();
    let writer = tokio::spawn(async move {
        let mut failed_input = None;
        while let Some((request, reply)) = input_rx.recv().await {
            let result = async {
                match request {
                    Request::Write { in_seq, line } => {
                        if failed_input.is_some_and(|failed| in_seq >= failed) {
                            return Err("previous input write incomplete; delivery unknown".into());
                        }
                        if in_seq > input_state.lock().await.hello.written {
                            input_state.lock().await.journal.append(Event::In {
                                in_seq,
                                line: line.clone(),
                            })?;
                            // A partial pipe failure is never acknowledged or automatically resent.
                            failed_input = Some(in_seq);
                            stdin
                                .as_mut()
                                .ok_or("stdin closed")?
                                .write_all(format!("{line}\n").as_bytes())
                                .await?;
                            input_state.lock().await.hello.written = in_seq;
                            failed_input = None;
                        }
                        Ok(Response::Written { in_seq })
                    }
                    _ => Err("invalid input request".into()),
                }
            }
            .await;
            let _ = reply.send(result.unwrap_or_else(
                |e: Box<dyn std::error::Error + Send + Sync>| Response::Error {
                    message: e.to_string(),
                },
            ));
        }
    });
    let input_abort = writer.abort_handle();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        let mut controller: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            let (mut socket, _) = listener.accept().await?;
            if socket.peer_cred()?.uid() != rustix::process::geteuid().as_raw() {
                continue;
            }
            if let Some(old) = controller.take() {
                old.abort();
            }
            let state = server_state.clone();
            let inputs = inputs.clone();
            let input_abort = input_abort.clone();
            controller = Some(tokio::spawn(async move {
                let _ = async {
                    send(
                        &mut socket,
                        &Response::Hello {
                            hello: state.lock().await.hello(),
                        },
                    )
                    .await?;
                    loop {
                        let request: Request = recv(&mut socket).await?;
                        let result = match request {
                            Request::Attach { after, limit } => state
                                .lock()
                                .await
                                .journal
                                .read(after, limit)
                                .map(|records| Response::Records { records }),
                            Request::Ack { seq } => state
                                .lock()
                                .await
                                .journal
                                .ack(seq)
                                .map(|_| Response::Acked { seq }),
                            Request::Stats => Ok(Response::Stats {
                                stats: state.lock().await.journal.stats.clone(),
                            }),
                            Request::Finish { action } => {
                                if matches!(action, Finish::CloseStdin) {
                                    input_abort.abort();
                                    Ok(Response::Finished)
                                } else {
                                    let identity = state.lock().await.hello.identity.clone();
                                    if !identity.alive() {
                                        Err("IdentityMismatch".into())
                                    } else {
                                        let signal = if matches!(action, Finish::Kill) {
                                            rustix::process::Signal::KILL
                                        } else {
                                            rustix::process::Signal::TERM
                                        };
                                        rustix::process::kill_process(
                                            rustix::process::Pid::from_raw(identity.pid as i32)
                                                .ok_or("invalid pid")?,
                                            signal,
                                        )?;
                                        Ok(Response::Finished)
                                    }
                                }
                            }
                            Request::Write { in_seq, ref line } => {
                                if line.contains('\n') || line.len() > MAX_FRAME / 4 || in_seq == 0
                                {
                                    return Err(
                                        "input must be one bounded line with nonzero sequence"
                                            .into(),
                                    );
                                }
                                {
                                    let mut state = state.lock().await;
                                    state.hello.accepted = state.hello.accepted.max(in_seq);
                                }
                                let (tx, rx) = oneshot::channel();
                                inputs.send((request, tx)).await?;
                                Ok(rx.await?)
                            }
                            Request::Release => break,
                        };
                        let response = result.unwrap_or_else(|e| Response::Error {
                            message: e.to_string(),
                        });
                        send(&mut socket, &response).await?;
                    }
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                }
                .await;
            }));
        }
        #[allow(unreachable_code)]
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    });
    let checkpoint_state = state.clone();
    let checkpoint_dir = spec.directory.clone();
    let checkpoint = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let state = checkpoint_state.lock().await;
            let _ = private_json(&checkpoint_dir.join("hello.json"), &state.hello());
            let _ = private_json(&checkpoint_dir.join("stats.json"), &state.journal.stats);
        }
    });
    let status = child.wait().await?;
    let drained = matches!(
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            tokio::join!(&mut out, &mut err)
        })
        .await,
        Ok((Ok(Ok(())), Ok(Ok(()))))
    );
    // Any unread or unobservable tail is explicit; never silently declare it reconstructable.
    out.abort();
    err.abort();
    let mut state = state.lock().await;
    if !drained {
        let _ = state.journal.append(Event::Gap {
            reason: GapReason::LostLines,
        });
    }
    let code = status.code().unwrap_or(-1);
    let _ = state.journal.append(Event::Exit { code });
    state.hello.exit = Some(code);
    private_json(&spec.directory.join("hello.json"), &state.hello())?;
    private_json(&spec.directory.join("stats.json"), &state.journal.stats)?;
    checkpoint.abort();
    server.abort();
    writer.abort();
    Ok(())
}
