//! 只读取当前 Wayland seat 的 repeat_info；不创建 surface，不取得输入焦点。
use rustix::event::{PollFd, PollFlags, poll};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    protocol::{wl_keyboard, wl_registry, wl_seat},
};

#[derive(Clone, Copy)]
pub(crate) struct RepeatTiming {
    pub delay: Duration,
    pub interval: Duration,
}

struct Settings {
    timing: Mutex<Option<RepeatTiming>>,
    _wake: UnixStream,
}

#[derive(Clone)]
pub(crate) struct KeyboardRepeat(Arc<Settings>);

impl KeyboardRepeat {
    pub fn read() -> Self {
        let (wake, stop) = UnixStream::pair().expect("repeat_info wake socket");
        let settings = Self(Arc::new(Settings {
            timing: Mutex::new(None),
            _wake: wake,
        }));
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let output = Arc::downgrade(&settings.0);
            // GPUI 没有公开 repeat_info；独立只读连接不阻塞 UI，也不改变输入法接入。
            std::thread::spawn(move || {
                listen_repeat_info(output, stop);
            });
        }
        settings
    }

    pub fn timing(&self) -> Option<RepeatTiming> {
        *self.0.timing.lock().unwrap()
    }
}

#[derive(Default)]
struct Reader {
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    timing: Option<RepeatTiming>,
}

fn listen_repeat_info(output: std::sync::Weak<Settings>, stop: UnixStream) -> Option<()> {
    let connection = Connection::connect_to_env().ok()?;
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());
    let mut reader = Reader::default();
    // registry → seat capabilities → keyboard repeat_info。
    for _ in 0..3 {
        queue.roundtrip(&mut reader).ok()?;
    }
    loop {
        {
            let settings = output.upgrade()?;
            *settings.timing.lock().unwrap() = reader.timing;
        }
        queue.dispatch_pending(&mut reader).ok()?;
        let Some(read) = queue.prepare_read() else {
            continue;
        };
        connection.flush().ok()?;
        let mut fds = [
            PollFd::new(&connection, PollFlags::IN),
            PollFd::new(&stop, PollFlags::IN),
        ];
        poll(&mut fds, None).ok()?;
        // 最后一个 Composer 释放 Settings 时关闭 wake，立刻结束监听，无定时轮询。
        if !fds[1].revents().is_empty() {
            return Some(());
        }
        read.read().ok()?;
        queue.dispatch_pending(&mut reader).ok()?;
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Reader {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_seat"
            && version >= 4
            && state.seat.is_none()
        {
            state.seat = Some(registry.bind(name, version.min(7), qh, ()));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Reader {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Reader {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::RepeatInfo { rate, delay } = event {
            state.timing = (rate > 0 && delay >= 0).then(|| RepeatTiming {
                delay: Duration::from_millis(delay as u64),
                interval: Duration::from_secs(1) / rate as u32,
            });
        }
    }
}
