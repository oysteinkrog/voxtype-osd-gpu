//! Keyboard focus and key events through the Wayland host's focus proxy (M1.2, M1.3). Run it
//! only inside a nested `kwin_wayland --virtual`, next to a window that has focus, and type
//! into it with `fake_keys`. Never point it at the real session.
//!
//!   key_probe [--hold-ms N] [--output NAME]
//!
//! Opens a focus proxy, prints every keyboard event the host sends while it holds focus,
//! closes the proxy after N ms (default 1500), waits 300 ms for stray events and exits. Each
//! line starts with CLOCK_MONOTONIC milliseconds. The last line is `RESULT typed=<text>
//! gained=<n> lost_requested=<n> lost_unrequested=<n> keys=<n>`.

use vo_host::{FocusProxySpec, Frame, Host, HostApp, HostCtl, HostEvent, KeyState, SurfaceId};
use vo_host_wayland::{monotonic, WaylandHost};

#[derive(Default)]
struct Probe {
    hold: f64,
    output: Option<String>,
    proxy: Option<SurfaceId>,
    closing: bool,
    typed: String,
    gained: u32,
    lost_requested: u32,
    lost_unrequested: u32,
    keys: u32,
}

fn log(line: &str) {
    println!("{:.3} {line}", monotonic() * 1000.0);
}

impl HostApp for Probe {
    fn start(&mut self, host: &mut dyn HostCtl) {
        log(&format!("caps keyboard={}", host.caps().keyboard));
        let spec = FocusProxySpec { app_id: "vo-key-probe".into(), output: self.output.clone() };
        match host.open_focus_proxy(&spec) {
            Ok(id) => {
                log(&format!("proxy opened id={id}"));
                self.proxy = Some(id);
            }
            Err(e) => {
                log(&format!("proxy failed: {e}"));
                host.exit();
                return;
            }
        }
        host.wake_at(Some(host.now() + self.hold));
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Key(k) => {
                self.keys += 1;
                if k.state == KeyState::Down {
                    if let Some(t) = &k.text {
                        self.typed.push_str(t);
                    }
                }
                log(&format!("key {:?} {:?} code={} text={:?} mods={:?}", k.state, k.key, k.code, k.text, k.mods));
            }
            HostEvent::KeyboardFocus { id, gained, requested, .. } => {
                match (gained, requested) {
                    (true, _) => self.gained += 1,
                    (false, true) => self.lost_requested += 1,
                    (false, false) => self.lost_unrequested += 1,
                }
                log(&format!("focus id={id} gained={gained} requested={requested}"));
            }
            HostEvent::Modifiers { mods, .. } => log(&format!("mods {mods:?}")),
            HostEvent::KeyRepeatInfo { info, .. } => log(&format!("repeat {info:?}")),
            HostEvent::Timer { .. } if !self.closing => {
                self.closing = true;
                if let Some(id) = self.proxy.take() {
                    log("closing proxy");
                    host.close_focus_proxy(id);
                }
                host.wake_at(Some(host.now() + 0.3));
            }
            HostEvent::Timer { .. } => {
                log(&format!(
                    "RESULT typed={} gained={} lost_requested={} lost_unrequested={} keys={}",
                    self.typed, self.gained, self.lost_requested, self.lost_unrequested, self.keys
                ));
                host.exit();
            }
            _ => {}
        }
    }

    fn frame(&mut self, _: &mut dyn HostCtl, _: &Frame<'_>) {}
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let hold = arg("--hold-ms").map_or(1500.0, |v| v.parse::<f64>().expect("--hold-ms N")) / 1000.0;
    let mut probe = Probe { hold, output: arg("--output"), ..Probe::default() };
    let host = WaylandHost::connect().unwrap_or_else(|e| panic!("key_probe: {e}"));
    host.run(&mut probe).unwrap_or_else(|e| panic!("key_probe: {e}"));
}
