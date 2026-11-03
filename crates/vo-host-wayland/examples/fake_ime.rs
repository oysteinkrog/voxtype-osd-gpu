//! A test input method for a nested KWin, through `zwp_input_method_v1` (plan F7). KWin
//! starts it itself: `kwin_wayland --inputmethod <path to fake_ime> ...`. Never point it at
//! the real session.
//!
//! Each time a text input is enabled (an `activate`), it plays a script once: the preedits
//! in `FAKE_IME_PREEDITS` (comma separated, default `pl,plu,plum`), 150 ms apart, then a
//! commit of `FAKE_IME_COMMIT` (default `plum`). Each step and every `surrounding_text` it
//! gets is logged to stderr, and to `FAKE_IME_LOG` when set, so a test can check what the
//! app told the input method.

use std::io::Write;
use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::protocols::wp::input_method::zv1::client::zwp_input_method_context_v1::{
    self, ZwpInputMethodContextV1,
};
use smithay_client_toolkit::reexports::protocols::wp::input_method::zv1::client::zwp_input_method_v1::{
    self, ZwpInputMethodV1,
};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};

#[derive(Default)]
struct State {
    /// The active context, the serial of its last `commit_state`, and where its script is.
    context: Option<(ZwpInputMethodContextV1, u32, Instant, usize)>,
    log: Option<std::fs::File>,
}

impl State {
    fn note(&mut self, line: &str) {
        eprintln!("fake_ime: {line}");
        if let Some(f) = &mut self.log {
            let _ = writeln!(f, "{line}");
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ZwpInputMethodV1,
        event: zwp_input_method_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            zwp_input_method_v1::Event::Activate { id } => {
                state.context = Some((id, 0, Instant::now(), 0));
                state.note("activate");
            }
            zwp_input_method_v1::Event::Deactivate { context } => {
                if state.context.as_ref().is_some_and(|c| c.0 == context) {
                    state.context = None;
                }
                context.destroy();
                state.note("deactivate");
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ZwpInputMethodV1, [
        zwp_input_method_v1::EVT_ACTIVATE_OPCODE => (ZwpInputMethodContextV1, ()),
    ]);
}

impl Dispatch<ZwpInputMethodContextV1, ()> for State {
    fn event(
        state: &mut State,
        ctx: &ZwpInputMethodContextV1,
        event: zwp_input_method_context_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            zwp_input_method_context_v1::Event::SurroundingText { text, cursor, anchor } => {
                state.note(&format!("surrounding {cursor} {anchor} {text:?}"));
            }
            zwp_input_method_context_v1::Event::CommitState { serial } => {
                if let Some(c) = state.context.as_mut().filter(|c| &c.0 == ctx) {
                    c.1 = serial;
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let preedits: Vec<String> = std::env::var("FAKE_IME_PREEDITS")
        .unwrap_or_else(|_| "pl,plu,plum".into())
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let commit = std::env::var("FAKE_IME_COMMIT").unwrap_or_else(|_| "plum".into());
    let conn = Connection::connect_to_env().expect("no Wayland connection (KWin passes WAYLAND_SOCKET)");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let _im: ZwpInputMethodV1 = globals.bind(&qh, 1..=1, ()).expect("zwp_input_method_v1");
    let mut state = State {
        log: std::env::var_os("FAKE_IME_LOG")
            .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()),
        ..State::default()
    };
    state.note("ready");
    let step = Duration::from_millis(150);
    loop {
        queue.flush().expect("flush");
        if let Some(guard) = queue.prepare_read() {
            let fd = guard.connection_fd();
            let mut pfd = libc::pollfd { fd: std::os::fd::AsRawFd::as_raw_fd(&fd), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd for the duration of the call.
            let n = unsafe { libc::poll(&mut pfd, 1, 20) };
            if n > 0 {
                let _ = guard.read();
            }
        }
        queue.dispatch_pending(&mut state).expect("dispatch");
        let Some((ctx, serial, since, at)) = state.context.clone() else { continue };
        if since.elapsed() < step * (at as u32 + 1) || at > preedits.len() {
            continue;
        }
        let line = match preedits.get(at) {
            Some(p) => {
                ctx.preedit_cursor(p.len() as i32);
                ctx.preedit_string(serial, p.clone(), commit.clone());
                format!("preedit {p:?}")
            }
            None => {
                ctx.commit_string(serial, commit.clone());
                format!("commit {commit:?}")
            }
        };
        if let Some(c) = state.context.as_mut() {
            c.3 = at + 1;
        }
        state.note(&line);
    }
}
