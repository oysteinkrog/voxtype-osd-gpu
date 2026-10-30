//! M0.1 KWin focus probe: can the popup take keyboard focus on demand and give it back?
//!
//! Run only inside a nested `kwin_wayland --virtual` (start it with
//! `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` so `--keys` and `--pointer` may use
//! `org_kde_kwin_fake_input`). Never point this at the real session.
//!
//!   focus_probe target [--title T] [--life-ms N]
//!       A plain xdg_toplevel that logs its own keyboard enter, leave and keys. It stands in
//!       for the app that dictated text goes to. It exits after N ms (default: never).
//!   focus_probe edit [--mode proxy|single] [--interactivity on_demand|exclusive] [--output NAME]
//!                    [--hold-ms N] [--keys] [--pointer X,Y [--click]] [--proxy-size N] [--after-ms N]
//!       Maps a popup like voxtype's (Overlay layer, keyboard_interactivity none, empty input
//!       region), then asks for focus:
//!         proxy   maps a second small, buffered Overlay layer surface with the given
//!                 interactivity, then destroys it to hand focus back.
//!         single  switches the popup's own interactivity at runtime, then back to none.
//!       --keys injects A, B, C through fake input while focused and counts wl_keyboard.key.
//!       --pointer moves the pointer to (X, Y) while focused (focus-follows-mouse test), and
//!       --click then clicks there (focus taken by another window mid-edit).
//!
//! Every line on stdout starts with CLOCK_MONOTONIC milliseconds, so lines from the target and
//! the editor can be merged by time. The editor ends with one `RESULT` line.
//!
//! Results on nested KWin 6.8.2, 2026-10-30 (`--reattach` sends a new buffer with the switch):
//! - proxy, on_demand or exclusive, 1 px or 8 px: `enter` 0.3 to 1.5 ms after the buffered
//!   commit, keymap, repeat_info and all keys arrive. Destroying it gives focus back to the
//!   previous window within about 1 ms, 10 runs out of 10, when both are on the same output.
//! - proxy on another output than the previous window: focus goes nowhere after the destroy.
//!   An explicit activation (`kdotool windowactivate`) fixes it.
//! - single surface: switching the mapped popup to on_demand or exclusive takes focus within
//!   1 ms. Switching back to none (with or without a new buffer) does not let go of it; only
//!   an explicit activation of the previous window or destroying the surface does.
//! - focus-follows-mouse: the pointer entering another window takes focus from the proxy
//!   after KWin's delay (about 300 ms). A click on another window does the same at once.
//! - previous window closed mid-edit: the proxy keeps focus; the destroy then activates the
//!   next window in the focus chain (another app), or nothing.
//! - locked session: neither the proxy nor any window gets `enter`.

use std::os::fd::AsRawFd;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window, registry_handlers,
};
use wayland_client::globals::{registry_queue_init, GlobalList};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::{self, OrgKdeKwinFakeInput};

const KEY_A: u32 = 30;
const KEY_B: u32 = 48;
const KEY_C: u32 = 46;
const BTN_LEFT: u32 = 0x110;

fn now_ms() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 * 1e3 + ts.tv_nsec as f64 / 1e6
}

fn log(role: &str, msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{:.3} {role} {msg}", now_ms());
    let _ = out.flush();
}

struct App {
    role: &'static str,
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    popup: Option<LayerSurface>,
    proxy: Option<LayerSurface>,
    window: Option<Window>,
    /// Surfaces that have a buffer attached, with the time of that commit.
    mapped: Vec<(WlSurface, f64)>,
    keyboard: Option<WlKeyboard>,
    focus: Option<WlSurface>,
    enters: Vec<(f64, WlSurface)>,
    leaves: Vec<(f64, WlSurface)>,
    keymap: bool,
    repeat: Option<(i32, i32)>,
    keys: Vec<(f64, u32, bool)>,
    closed: bool,
}

impl App {
    fn name_of(&self, s: &WlSurface) -> &'static str {
        if self.popup.as_ref().is_some_and(|l| l.wl_surface() == s) {
            "popup"
        } else if self.proxy.as_ref().is_some_and(|l| l.wl_surface() == s) {
            "proxy"
        } else if self.window.as_ref().is_some_and(|w| w.wl_surface() == s) {
            "window"
        } else {
            "gone"
        }
    }

    fn draw(&mut self, surface: &WlSurface, w: u32, h: u32, argb: u32) {
        let (w, h) = (w.max(1), h.max(1));
        let (buffer, canvas) =
            self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, Format::Argb8888).expect("buffer");
        for px in canvas.as_chunks_mut::<4>().0 {
            *px = argb.to_le_bytes();
        }
        surface.damage_buffer(0, 0, w as i32, h as i32);
        buffer.attach_to(surface).expect("attach");
        surface.commit();
        if !self.mapped.iter().any(|(s, _)| s == surface) {
            let t = now_ms();
            self.mapped.push((surface.clone(), t));
            log(self.role, &format!("mapped {}", self.name_of(surface)));
        }
    }

    fn mapped_at(&self, s: &WlSurface) -> Option<f64> {
        self.mapped.iter().find(|(m, _)| m == s).map(|(_, t)| *t)
    }

    fn focused(&self, s: &WlSurface) -> bool {
        self.focus.as_ref() == Some(s)
    }
}

/// Dispatches events for up to `ms`, returning early once `stop` holds.
fn pump(q: &mut EventQueue<App>, app: &mut App, ms: f64, stop: impl Fn(&App) -> bool) -> bool {
    let deadline = now_ms() + ms;
    loop {
        q.dispatch_pending(app).expect("dispatch");
        if stop(app) {
            return true;
        }
        let left = deadline - now_ms();
        if left <= 0.0 {
            return false;
        }
        q.flush().expect("flush");
        if let Some(guard) = q.prepare_read() {
            let mut pfd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut pfd, 1, left.ceil() as i32) } > 0 {
                let _ = guard.read();
            }
        }
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn arg_ms(args: &[String], name: &str, default: f64) -> f64 {
    arg(args, name).map(|v| v.parse().expect("number")).unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let role = match args.first().map(String::as_str) {
        Some("target") => "target",
        Some("edit") => "edit",
        _ => {
            eprintln!("usage: focus_probe target|edit [options]  (see the file header)");
            std::process::exit(2);
        }
    };
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut q) = registry_queue_init::<App>(&conn).expect("registry");
    let qh = q.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let pool = SlotPool::new(256 * 256 * 4, &shm).expect("pool");
    let _seat: WlSeat = globals.bind(&qh, 1..=7, ()).expect("wl_seat");
    let mut app = App {
        role,
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        popup: None,
        proxy: None,
        window: None,
        mapped: Vec::new(),
        keyboard: None,
        focus: None,
        enters: Vec::new(),
        leaves: Vec::new(),
        keymap: false,
        repeat: None,
        keys: Vec::new(),
        closed: false,
    };
    q.roundtrip(&mut app).expect("roundtrip");
    q.roundtrip(&mut app).expect("roundtrip");
    if role == "target" {
        run_target(&args, &globals, &compositor, &mut q, &mut app);
    } else {
        run_edit(&args, &globals, &compositor, &mut q, &mut app);
    }
}

fn run_target(
    args: &[String],
    globals: &GlobalList,
    compositor: &CompositorState,
    q: &mut EventQueue<App>,
    app: &mut App,
) {
    let qh = q.handle();
    let xdg = XdgShell::bind(globals, &qh).expect("xdg_wm_base");
    let title = arg(args, "--title").unwrap_or("focus-target").to_string();
    let window = xdg.create_window(compositor.create_surface(&qh), WindowDecorations::RequestServer, &qh);
    window.set_title(title.clone());
    window.set_app_id(title.clone());
    window.set_min_size(Some((400, 300)));
    window.commit();
    app.window = Some(window);
    log("target", &format!("start title={title} pid={}", std::process::id()));
    let life = arg_ms(args, "--life-ms", f64::INFINITY);
    let end = now_ms() + life;
    while !app.closed && now_ms() < end {
        pump(q, app, (end - now_ms()).min(1000.0), |a| a.closed);
    }
    log("target", "exit");
}

fn run_edit(
    args: &[String],
    globals: &GlobalList,
    compositor: &CompositorState,
    q: &mut EventQueue<App>,
    app: &mut App,
) {
    let qh = q.handle();
    let shell = LayerShell::bind(globals, &qh).expect("zwlr_layer_shell_v1");
    let mode = arg(args, "--mode").unwrap_or("proxy").to_string();
    let wanted = match arg(args, "--interactivity").unwrap_or("on_demand") {
        "on_demand" => KeyboardInteractivity::OnDemand,
        "exclusive" => KeyboardInteractivity::Exclusive,
        other => panic!("unknown interactivity {other}"),
    };
    let hold = arg_ms(args, "--hold-ms", 800.0);
    let after = arg_ms(args, "--after-ms", 600.0);
    let proxy_size = arg_ms(args, "--proxy-size", 1.0) as u32;
    let keys = args.iter().any(|a| a == "--keys");
    let click = args.iter().any(|a| a == "--click");
    let pointer = arg(args, "--pointer").map(|v| {
        let (x, y) = v.split_once(',').expect("--pointer X,Y");
        (x.parse::<f64>().expect("X"), y.parse::<f64>().expect("Y"))
    });

    let output: Option<WlOutput> = arg(args, "--output").map(|name| {
        app.outputs
            .outputs()
            .find(|o| app.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no output named {name}"))
    });
    let names: Vec<String> = app.outputs.outputs().filter_map(|o| app.outputs.info(&o).and_then(|i| i.name)).collect();
    log("edit", &format!("outputs {names:?} mode={mode} interactivity={wanted:?}"));

    // The popup as voxtype maps it today: never focused, click-through.
    let surface = compositor.create_surface(&qh);
    let popup = shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("voxtype-osd"), output.as_ref());
    popup.set_anchor(Anchor::BOTTOM);
    popup.set_size(600, 120);
    popup.set_margin(0, 0, 80, 0);
    popup.set_keyboard_interactivity(KeyboardInteractivity::None);
    let empty = Region::new(compositor).expect("region");
    popup.set_input_region(Some(empty.wl_region()));
    popup.commit();
    let popup_surface = popup.wl_surface().clone();
    app.popup = Some(popup);
    pump(q, app, 2000.0, |a| a.mapped_at(&popup_surface).is_some());
    pump(q, app, 400.0, |_| false);
    let popup_focused_early = app.focused(&popup_surface);
    log("edit", &format!("popup settled, focused={popup_focused_early}"));

    // Ask for focus.
    let (focus_surface, t_ask) = if mode == "proxy" {
        let surface = compositor.create_surface(&qh);
        let proxy =
            shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("voxtype-edit-focus"), output.as_ref());
        proxy.set_anchor(Anchor::TOP | Anchor::LEFT);
        proxy.set_size(proxy_size, proxy_size);
        proxy.set_keyboard_interactivity(wanted);
        proxy.commit();
        let s = proxy.wl_surface().clone();
        let t = now_ms();
        log("edit", "proxy created");
        app.proxy = Some(proxy);
        (s, t)
    } else {
        let popup = app.popup.as_ref().unwrap();
        popup.set_keyboard_interactivity(wanted);
        if args.iter().any(|a| a == "--reattach") {
            app.draw(&popup_surface, 600, 120, 0xc0202840);
        } else {
            popup.commit();
        }
        let t = now_ms();
        log("edit", "popup interactivity switched");
        (popup_surface.clone(), t)
    };
    let got = pump(q, app, 1500.0, |a| a.focused(&focus_surface));
    let t_enter = app.enters.iter().rev().find(|(_, s)| *s == focus_surface).map(|(t, _)| *t);
    let mapped = app.mapped_at(&focus_surface).unwrap_or(t_ask);
    let enter_ms = t_enter.map(|t| t - if mode == "proxy" { mapped } else { t_ask });
    log("edit", &format!("focus {} after {:?} ms", if got { "taken" } else { "NOT taken" }, enter_ms));

    // Hold the focus like an edit session would.
    let key_base = app.keys.len();
    if got && (keys || pointer.is_some()) {
        let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 4..=5, ()).expect("org_kde_kwin_fake_input v4+");
        fake.authenticate("vo focus_probe".into(), "keyboard focus test".into());
        if let Some((x, y)) = pointer {
            fake.pointer_motion_absolute(x, y);
            log("edit", &format!("pointer moved to {x},{y}"));
            pump(q, app, 150.0, |_| false);
            fake.pointer_motion_absolute(x + 5.0, y + 5.0);
            if click {
                fake.button(BTN_LEFT, 1);
                pump(q, app, 30.0, |_| false);
                fake.button(BTN_LEFT, 0);
                log("edit", "clicked");
            }
        }
        if keys {
            for k in [KEY_A, KEY_B, KEY_C] {
                fake.keyboard_key(k, 1);
                pump(q, app, 20.0, |_| false);
                fake.keyboard_key(k, 0);
                pump(q, app, 20.0, |_| false);
            }
        }
    }
    pump(q, app, hold, |_| false);
    let still = app.focused(&focus_surface);
    let lost_early = app.leaves.iter().any(|(t, s)| *s == focus_surface && t_enter.is_some_and(|e| *t > e));
    let keys_seen = app.keys.len() - key_base;

    // Hand focus back.
    let t_release = now_ms();
    if mode == "proxy" {
        app.proxy = None;
        log("edit", "proxy destroyed");
    } else {
        let popup = app.popup.as_ref().unwrap();
        popup.set_keyboard_interactivity(KeyboardInteractivity::None);
        if args.iter().any(|a| a == "--reattach") {
            app.draw(&popup_surface, 600, 120, 0xc0203040);
        } else {
            popup.commit();
        }
        log("edit", "popup interactivity back to none");
    }
    let released = pump(q, app, 1000.0, |a| !a.focused(&focus_surface));
    let t_leave = app.leaves.iter().find(|(t, _)| *t >= t_release).map(|(t, _)| *t);
    pump(q, app, after, |_| false);
    log(
        "edit",
        &format!(
            "RESULT mode={mode} interactivity={wanted:?} popup_focused_before={popup_focused_early} enter_ms={} ask_to_enter_ms={} \
             held_to_end={still} lost_during_hold={lost_early} keymap={} repeat={:?} keys={keys_seen} \
             t_release={t_release:.3} leave_ms={} released={released}",
            enter_ms.map_or("none".into(), |v| format!("{v:.1}")),
            t_enter.map_or("none".into(), |t| format!("{:.1}", t - t_ask)),
            app.keymap,
            app.repeat,
            t_leave.map_or("none".into(), |t| format!("{:.1}", t - t_release)),
        ),
    );
}

impl Dispatch<WlSeat, ()> for App {
    fn event(app: &mut App, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<App>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) && app.keyboard.is_none() {
                app.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(app: &mut App, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
        let t = now_ms();
        match event {
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                app.keymap = true;
                log(app.role, &format!("keymap size={size}"));
                drop(fd);
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                log(app.role, &format!("enter {}", app.name_of(&surface)));
                app.enters.push((t, surface.clone()));
                app.focus = Some(surface);
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                log(app.role, &format!("leave {}", app.name_of(&surface)));
                app.leaves.push((t, surface));
                // One seat: a leave always ends the current focus, even when its surface is already
                // destroyed and no longer compares equal.
                app.focus = None;
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let down = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                log(app.role, &format!("key {key} {}", if down { "down" } else { "up" }));
                app.keys.push((t, key, down));
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                log(app.role, &format!("repeat rate={rate} delay={delay}"));
                app.repeat = Some((rate, delay));
            }
            _ => {}
        }
    }
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for App {
    fn event(
        _: &mut App,
        _: &OrgKdeKwinFakeInput,
        _: org_kde_kwin_fake_input::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        log(self.role, &format!("layer closed {}", self.name_of(layer.wl_surface())));
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        let s = layer.wl_surface().clone();
        let (w, h) = c.new_size;
        let argb = if self.name_of(&s) == "proxy" { 0x00000000 } else { 0xc0203040 };
        self.draw(&s, w, h, argb);
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.closed = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, c: WindowConfigure, _: u32) {
        let w = c.new_size.0.map_or(400, |v| v.get());
        let h = c.new_size.1.map_or(300, |v| v.get());
        let s = window.wl_surface().clone();
        self.draw(&s, w, h, 0xff406080);
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);
