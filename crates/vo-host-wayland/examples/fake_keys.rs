//! Injects key presses into a nested KWin through `org_kde_kwin_fake_input` (version 4 or
//! 5), for Wayland-side keyboard tests. KWin only allows this protocol to trusted clients;
//! start the nested KWin with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`. Never point this at the
//! real session. The keys go through KWin's own keymap and focus, as typed keys do; they are
//! not kernel evdev events, so voxtype's evdev listener never sees them.
//!
//!   fake_keys STEP...
//!
//! A step is a key name or an evdev code of two or more digits (`e`, `5`, `apostrophe`,
//! `rightalt`, `30`): `KEY` taps it,
//! `+KEY` presses it, `-KEY` releases it, and `wait:MS` pauses. Steps are 15 ms apart.
//!
//!   fake_keys +rightalt apostrophe -rightalt e      # AltGr+' then e: "é" on us(altgr-intl)

use std::time::Duration;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::{self, OrgKdeKwinFakeInput};

struct State;

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

impl Dispatch<OrgKdeKwinFakeInput, ()> for State {
    fn event(
        _: &mut State,
        _: &OrgKdeKwinFakeInput,
        _: org_kde_kwin_fake_input::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// evdev codes (linux/input-event-codes.h) by name.
fn code(name: &str) -> Option<u32> {
    // One digit is a digit key; a longer number is an evdev code.
    if let (true, Ok(n)) = (name.len() > 1, name.parse()) {
        return Some(n);
    }
    let letters = "qwertyuiop\0\0\0\0asdfghjkl\0\0\0\0\0zxcvbnm";
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_lowercase() {
            let i = letters.find(c)? as u32;
            return Some(16 + i);
        }
        if c.is_ascii_digit() {
            return Some(if c == '0' { 11 } else { 1 + c.to_digit(10)? });
        }
    }
    Some(match name {
        "esc" => 1,
        "minus" => 12,
        "equal" => 13,
        "backspace" => 14,
        "tab" => 15,
        "enter" => 28,
        "leftctrl" => 29,
        "semicolon" => 39,
        "apostrophe" => 40,
        "grave" => 41,
        "leftshift" => 42,
        "comma" => 51,
        "dot" => 52,
        "slash" => 53,
        "rightshift" => 54,
        "leftalt" => 56,
        "space" => 57,
        "rightctrl" => 97,
        "rightalt" => 100,
        "home" => 102,
        "up" => 103,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "delete" => 111,
        _ => return None,
    })
}

fn main() {
    let steps: Vec<String> = std::env::args().skip(1).collect();
    if steps.is_empty() {
        eprintln!("usage: fake_keys STEP...  (KEY, +KEY, -KEY, wait:MS)");
        std::process::exit(2);
    }
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 4..=5, ()).expect("org_kde_kwin_fake_input (version 4 or 5)");
    fake.authenticate("vo fake_keys".into(), "keyboard input test".into());
    let pause = || std::thread::sleep(Duration::from_millis(15));
    for step in &steps {
        if let Some(ms) = step.strip_prefix("wait:") {
            std::thread::sleep(Duration::from_millis(ms.parse().expect("wait:MS")));
            continue;
        }
        let (down, up, name) = match step.as_bytes()[0] {
            b'+' => (true, false, &step[1..]),
            b'-' => (false, true, &step[1..]),
            _ => (true, true, step.as_str()),
        };
        let key = code(name).unwrap_or_else(|| panic!("unknown key {name}"));
        if down {
            fake.keyboard_key(key, 1);
            queue.roundtrip(&mut State).expect("roundtrip");
            pause();
        }
        if up {
            fake.keyboard_key(key, 0);
            queue.roundtrip(&mut State).expect("roundtrip");
            pause();
        }
    }
    eprintln!("fake_keys: {} steps", steps.len());
}
