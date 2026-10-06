use gpui_kit::component::{
    Theme, ThemeMode, h_flex,
    input::{InputEvent, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::*;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const VARIANT: &str = env!("IME_LAB_VARIANT");
const COMMIT: &str = env!("IME_LAB_COMMIT");
mod scenario;

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

struct LogState {
    file: File,
    recent: VecDeque<String>,
    seq: u64,
    display_revision: u64,
}

#[derive(Clone)]
struct EventLog(Arc<Mutex<LogState>>);

impl EventLog {
    fn new(path: &PathBuf) -> Self {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).expect("create log directory");
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open IME_LAB_LOG");
        Self(Arc::new(Mutex::new(LogState {
            file,
            recent: VecDeque::new(),
            seq: 0,
            display_revision: 0,
        })))
    }

    fn emit(&self, mut event: Value) {
        let mut state = self.0.lock().unwrap();
        state.seq += 1;
        event["schema"] = json!(1);
        event["ts_ms"] = json!(epoch_ms());
        event["seq"] = json!(state.seq);
        event["pid"] = json!(std::process::id());
        event["variant"] = json!(VARIANT);
        // Bounds queries caused by a paint must not cause another paint themselves.
        if event["event"] != "cursor_bounds" {
            state.display_revision += 1;
        }
        let line = serde_json::to_string(&event).unwrap();
        writeln!(state.file, "{line}").expect("write IME log");
        state.file.flush().expect("flush IME log");
        state.recent.push_back(line);
        while state.recent.len() > 50 {
            state.recent.pop_front();
        }
    }

    fn recent(&self) -> Vec<String> {
        self.0.lock().unwrap().recent.iter().cloned().collect()
    }
    fn revision(&self) -> u64 {
        self.0.lock().unwrap().display_revision
    }
}

struct Lab {
    input: Entity<TextareaState>,
    messages: Vec<String>,
    log: EventLog,
    guard: bool,
    rendered: bool,
    log_revision: u64,
    _subscriptions: Vec<Subscription>,
}

impl Lab {
    fn new(log: EventLog, guard: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("输入中文。Enter 发送，Shift+Enter 换行，Ctrl+Enter 仅记录。")
                .auto_grow(4, 9)
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(&input, window, |this: &mut Self, input, event, window, cx| {
            let state = input.read(cx).ime_lab_snapshot();
            match event {
                InputEvent::Change => this.log.emit(json!({"event":"input_change", "state":state,
                    "value":input.read(cx).value().to_string()})),
                InputEvent::Focus | InputEvent::Blur => this.log.emit(json!({"event":"focus",
                    "target":"input", "focused":matches!(event, InputEvent::Focus), "state":state})),
                InputEvent::PressEnter { secondary, shift } => {
                    this.log.emit(json!({"event":"press_enter", "secondary":secondary, "shift":shift, "state":state}));
                    if !shift && !secondary {
                        if this.guard && state["has_preedit"] == true {
                            this.log.emit(json!({"event":"send_blocked", "reason":"preedit", "state":state}));
                        } else {
                            let text = input.read(cx).value().to_string();
                            this.log.emit(json!({"event":"send", "text":text, "state":state, "guard":this.guard}));
                            this.messages.push(text);
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                }
            }
            cx.notify();
        });
        let activation = cx.observe_window_activation(window, |this: &mut Self, window, _| {
            this.log.emit(
                json!({"event":"focus", "target":"window", "focused":window.is_window_active()}),
            );
        });
        // GPUI resolves actions before element key listeners. Observe here so
        // even a consumed Shift+Enter records the pre-action composition state.
        let key_log = log.clone();
        let key_input = input.clone();
        let keys = cx.intercept_keystrokes(move |event, _, cx| {
            key_log.emit(
                json!({"event":"key_down", "source":"GPUI intercept_keystrokes",
                "phase":"before_action", "keystroke":event.keystroke.to_string(),
                "key":event.keystroke.key, "key_char":event.keystroke.key_char,
                "modifiers":event.keystroke.modifiers,
                "state":key_input.read(cx).ime_lab_snapshot()}),
            );
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        if std::env::var("IME_LAB_SCENARIO").as_deref() == Ok("check") {
            scenario::start(window, cx);
        }
        // Pull observer events into the panel without recursive entity updates.
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if weak
                    .update(cx, |this, cx| {
                        let revision = this.log.revision();
                        if revision != this.log_revision {
                            this.log_revision = revision;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            input,
            messages: Vec::new(),
            log,
            guard,
            rendered: false,
            log_revision: 0,
            _subscriptions: vec![subscription, activation, keys],
        }
    }
}

impl Render for Lab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.rendered {
            self.log.emit(json!({"event":"first_render"}));
            self.rendered = true;
        }
        h_flex().size_full().p_4().gap_4().bg(rgb(0x171c25)).text_color(rgb(0xe4e9f2))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.log.emit(json!({"event":"key_down_unhandled", "source":"GPUI capture after actions", "keystroke":event.keystroke.to_string(),
                    "key":event.keystroke.key, "key_char":event.keystroke.key_char,
                    "modifiers":event.keystroke.modifiers, "is_held":event.is_held,
                    "prefer_character_input":event.prefer_character_input,
                    "state":this.input.read(cx).ime_lab_snapshot()}));
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                this.log.emit(json!({"event":"key_up", "source":"GPUI capture", "keystroke":event.keystroke.to_string(),
                    "key":event.keystroke.key, "key_char":event.keystroke.key_char,
                    "modifiers":event.keystroke.modifiers, "state":this.input.read(cx).ime_lab_snapshot()}));
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.log.emit(json!({"event":"modifiers_changed", "modifiers":event.modifiers,
                    "state":this.input.read(cx).ime_lab_snapshot()}));
            }))
            .child(v_flex().flex_1().min_w_0().h_full().gap_3()
                .child(div().text_xl().child(format!("ime-lab {VARIANT}")))
                .child(format!("Kit Enter 默认路径 · 应用发送防护 {}", if self.guard { "ON" } else { "OFF" }))
                .child(div().id("messages").w_full().flex_1().min_h_0().overflow_y_scroll()
                    .child(v_flex().gap_2().children(self.messages.iter().enumerate().map(|(i, text)| {
                        div().p_3().rounded_md().bg(rgb(0x253344)).child(format!("{}  {}", i+1, text))
                    }))))
                .child("Enter 发送 · Shift+Enter 换行 · Ctrl+Enter 仅记录")
                .child(Textarea::new(&self.input).h(px(190.))))
            .child(v_flex().w(px(500.)).h_full().gap_2()
                .child("事件日志 · 最近 50 条（新事件在上）")
                .child(div().id("event-log").flex_1().min_h_0().overflow_y_scroll()
                    .child(v_flex().gap_1().children(self.log.recent().into_iter().rev().map(|line| {
                        div().text_xs().p_1().bg(rgb(0x212732)).child(line)
                    })))))
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = std::env::var_os("IME_LAB_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("IME_LAB_ROOT"))
                .join("logs")
                .join(format!("{VARIANT}-{}.jsonl", epoch_ms()))
        });
    let log = EventLog::new(&path);
    let guard = std::env::var("IME_LAB_GUARD").as_deref() == Ok("1");
    let observer = log.clone();
    gpui_kit::base::ime_lab_observer::install(move |event| observer.emit(event));
    log.emit(
        json!({"event":"start", "commit":COMMIT, "guard":guard, "log_path":path,
        "wayland_display":std::env::var("WAYLAND_DISPLAY").ok(),
        "xdg_runtime_dir":std::env::var("XDG_RUNTIME_DIR").ok(), "gpui_pre":"0.3.7",
        "scenario":std::env::var("IME_LAB_SCENARIO").unwrap_or_else(|_| "none".into())}),
    );
    eprintln!("ime-lab {VARIANT}: {}", path.display());
    let app_log = log.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1300.), px(800.)), cx)),
                app_id: Some(format!("ime-lab-{VARIANT}").into()),
                ..Default::default()
            };
            let window_log = app_log.clone();
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title(&format!("ime-lab {VARIANT}"));
                cx.new(|cx| Lab::new(window_log, guard, window, cx))
            })
            .expect("open ime-lab window");
            app_log.emit(json!({"event":"window_opened", "title":format!("ime-lab {VARIANT}")}));
            if let Ok(seconds) = std::env::var("IME_LAB_SECONDS") {
                let seconds: u64 = seconds.parse().expect("IME_LAB_SECONDS integer");
                let timer = cx.background_executor().timer(Duration::from_secs(seconds));
                cx.spawn(async move |cx| {
                    timer.await;
                    app_log.emit(json!({"event":"timed_close", "seconds":seconds}));
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
    log.emit(json!({"event":"exit"}));
}
