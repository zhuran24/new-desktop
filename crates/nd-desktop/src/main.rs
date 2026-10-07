use gpui_kit::*;
use nd_desktop::Desktop;
use nd_view_model::ViewStateFile;
use std::{path::PathBuf, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut socket = None;
    let mut state_path = None;
    let mut seconds = None::<u64>;
    let mut themes_path = None;
    #[cfg(feature = "scenarios")]
    let mut scenario_create = None::<serde_json::Value>;
    #[cfg(feature = "scenarios")]
    let mut scenario_settings = None::<serde_json::Value>;
    #[cfg(feature = "scenarios")]
    let mut scenario_draft = None::<serde_json::Value>;
    #[cfg(feature = "scenarios")]
    let mut scenario_theme_controls = None::<PathBuf>;
    #[cfg(feature = "scenarios")]
    let mut scenario_history = None::<serde_json::Value>;
    #[cfg(feature = "scenarios")]
    let mut scenario_controls = None::<PathBuf>;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            #[cfg(feature = "scenarios")]
            "--scenario-theme-controls" => {
                scenario_theme_controls = Some(PathBuf::from(
                    args.next().ok_or("missing theme controls path")?,
                ))
            }
            #[cfg(feature = "scenarios")]
            "--scenario-controls" => {
                scenario_controls = Some(args.next().ok_or("missing controls")?.into())
            }
            #[cfg(feature = "scenarios")]
            "--scenario-settings" => {
                scenario_settings = Some(serde_json::from_str(
                    &args.next().ok_or("missing settings plan")?,
                )?);
            }
            #[cfg(feature = "scenarios")]
            "--scenario-history" => {
                scenario_history = Some(serde_json::from_str(
                    &args.next().ok_or("missing history plan")?,
                )?)
            }
            #[cfg(feature = "scenarios")]
            "--scenario-draft" => {
                scenario_draft = Some(serde_json::from_str(
                    &args.next().ok_or("missing draft plan")?,
                )?)
            }
            #[cfg(feature = "scenarios")]
            "--scenario-create" => {
                scenario_create = Some(serde_json::from_str(
                    &args.next().ok_or("missing scenario")?,
                )?)
            }
            "--socket" => socket = Some(PathBuf::from(args.next().ok_or("--socket needs a path")?)),
            "--themes" => {
                themes_path = Some(PathBuf::from(args.next().ok_or("--themes needs a path")?))
            }
            "--state" => {
                state_path = Some(PathBuf::from(args.next().ok_or("--state needs a path")?))
            }
            "--quit-after" => {
                seconds = Some(args.next().ok_or("--quit-after needs seconds")?.parse()?)
            }
            "--help" => {
                println!(
                    "nd-desktop [--socket PATH] [--state PATH] [--themes DIRECTORY] [--quit-after SECONDS]"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let socket = match socket {
        Some(path) => path,
        None => PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR missing; use --socket")?,
        )
        .join("new-desktop/nd.sock"),
    };
    let state_path = match state_path {
        Some(path) => path,
        None => std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .ok_or("HOME missing; use --state")?
            .join("new-desktop/ui.json"),
    };
    let file = ViewStateFile::open(&state_path)?;
    let themes_path = match themes_path {
        Some(path) => path,
        None => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or("HOME missing; use --themes")?
            .join("new-desktop/themes"),
    };
    let themes_path = std::path::absolute(themes_path)?;
    let (state, warning) = match file.load() {
        Ok(state) => (state, None),
        Err(e) => (
            Default::default(),
            Some(format!("视图状态读取失败，已用默认值：{e}")),
        ),
    };
    let (save, mut saves) = tokio::sync::watch::channel(state.clone());
    // 唯一后台写入者，最新偏好可合并；退出后排空并释放文件锁。
    let writer = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("state runtime");
        runtime.block_on(async move {
            while saves.changed().await.is_ok() {
                let state = saves.borrow_and_update().clone();
                if let Err(e) = file.save(&state) {
                    eprintln!("保存视图状态失败：{e}");
                }
            }
        });
    });
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(
                    size(px(state.window.width), px(state.window.height)),
                    cx,
                )),
                app_id: Some("new-desktop".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title("New Desktop");
                cx.new(|cx| {
                    let desktop =
                        Desktop::new(socket, state, save, warning, themes_path, window, cx)
                            .expect("start nd-wire worker");
                    #[cfg(feature = "scenarios")]
                    if let Some(path) = scenario_theme_controls {
                        Desktop::scenario_theme_controls(path, window, cx);
                    }
                    #[cfg(feature = "scenarios")]
                    if let Some(plan) = scenario_settings {
                        Desktop::scenario_settings(plan, window, cx);
                    }
                    #[cfg(feature = "scenarios")]
                    if let Some(plan) = scenario_create {
                        Desktop::scenario_create(plan, window, cx);
                    }
                    #[cfg(feature = "scenarios")]
                    if let Some(plan) = scenario_draft {
                        Desktop::scenario_draft(plan, window, cx);
                    }
                    #[cfg(feature = "scenarios")]
                    if let Some(plan) = scenario_history {
                        Desktop::scenario_history(plan, window, cx);
                    }
                    #[cfg(feature = "scenarios")]
                    if let Some(path) = scenario_controls {
                        Desktop::scenario_controls(path, window, cx);
                    }
                    desktop
                })
            })
            .expect("open New Desktop window");
            if let Some(seconds) = seconds {
                let timer = cx.background_executor().timer(Duration::from_secs(seconds));
                cx.spawn(async move |cx| {
                    timer.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
    writer.join().map_err(|_| "state writer panicked")?;
    Ok(())
}
