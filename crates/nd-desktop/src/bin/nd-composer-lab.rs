//! 离线真机验收入口：使用产品 Composer，提交只显示在本窗口内。
use gpui_kit::*;
use nd_composer::ComposerAction;
use nd_desktop::{
    apply_theme,
    composer::{Composer, ComposerEvent},
};
use nd_view_model::{Theme, ThemeMode};
use std::time::Duration;

struct Lab {
    composer: Entity<Composer>,
    theme: Theme,
    messages: Vec<String>,
    escapes: usize,
    _events: Subscription,
    first_render: bool,
}
impl Lab {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Theme::builtin(ThemeMode::Dark);
        apply_theme(&theme, cx);
        let composer = cx.new(|cx| {
            let mut input = Composer::new(theme.clone(), window, cx);
            input.set_send_enabled(true, cx);
            input
                .editor()
                .update(cx, |input, cx| input.focus(window, cx));
            input
        });
        let events = cx.subscribe_in(
            &composer,
            window,
            |this: &mut Self, composer, event, window, cx| {
                match event {
                    ComposerEvent::Action(ComposerAction::Submit { text }) => {
                        this.messages.push(text.clone());
                        // 本地接收在同一回调完成；不表示任何 nd-wire 交付。
                        composer
                            .read(cx)
                            .editor()
                            .clone()
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    ComposerEvent::Action(ComposerAction::Escape) => this.escapes += 1,
                    _ => return,
                }
                cx.notify();
            },
        );
        Self {
            composer,
            theme,
            messages: vec![],
            escapes: 0,
            _events: events,
            first_render: true,
        }
    }
}
impl Render for Lab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.first_render {
            println!("{}", serde_json::json!({"composer_lab_ready": true}));
            self.first_render = false;
        }
        let t = &self.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(px(t.spacing.large))
            .gap(px(t.spacing.medium))
            .bg(rgba(t.colors.background))
            .text_color(rgba(t.colors.foreground))
            .font_family(t.typography.family.clone())
            .text_size(px(t.typography.body))
            .child(
                div()
                    .text_size(px(t.typography.title))
                    .child("输入框离线验收"),
            )
            .child("提交仅显示在此窗口，不连接守护进程或模型；关窗后清空。")
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(format!(
                        "本地提交：{}　非组词 Esc：{}",
                        self.messages.len(),
                        self.escapes
                    ))
                    .child(
                        div()
                            .id("theme")
                            .cursor_pointer()
                            .text_color(rgba(t.colors.accent))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.theme =
                                    Theme::builtin(if this.theme.mode == ThemeMode::Dark {
                                        ThemeMode::Light
                                    } else {
                                        ThemeMode::Dark
                                    });
                                apply_theme(&this.theme, cx);
                                this.composer
                                    .update(cx, |c, cx| c.set_theme(this.theme.clone(), cx));
                                cx.notify();
                            }))
                            .child("切换明暗"),
                    ),
            )
            .child(
                div()
                    .id("submissions")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(self.messages.iter().enumerate().map(|(i, text)| {
                        div()
                            .p(px(t.spacing.small))
                            .child(format!("{}：{}", i + 1, text))
                    })),
            )
            .child(self.composer.clone())
    }
}
fn main() {
    let mut args = std::env::args().skip(1);
    let mut seconds = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quit-after" => {
                seconds = Some(
                    args.next()
                        .expect("seconds")
                        .parse::<u64>()
                        .expect("seconds"),
                )
            }
            _ => panic!("nd-composer-lab [--quit-after SECONDS]"),
        }
    }
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
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(size(px(1000.), px(700.)), cx)),
                    app_id: Some("new-desktop-composer-lab".into()),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_window_title("New Desktop 输入框离线验收");
                    cx.new(|cx| Lab::new(window, cx))
                },
            )
            .expect("open composer lab");
            if let Some(seconds) = seconds {
                let timer = cx.background_executor().timer(Duration::from_secs(seconds));
                cx.spawn(async move |cx| {
                    timer.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
        });
}
