//! Injects pointer input into a nested KWin through `org_kde_kwin_fake_input`, for testing
//! overlays without a person at the mouse. KWin only allows this protocol to trusted clients;
//! start the nested KWin with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`. Never point this at the
//! real session.
//!
//!   fake_click move X Y        move the pointer to global logical (X, Y)
//!   fake_click click X Y [N]   move there, then press and release the left button (N times,
//!                              60 ms apart, for a double or triple click)
//!   fake_click drag X Y X2 Y2  press at (X, Y), move to (X2, Y2) in steps, release there
//!   fake_click rdrag X Y DX DY press at (X, Y), move by (DX, DY) in relative steps, as a
//!                              mouse does, release there
//!
//! KWin removes the fake pointer when this exits, and clients see the pointer capability go.
//! Set `FAKE_CLICK_HOLD_MS=N` to keep it N ms longer, for a check that needs the pointer to
//! stay (a cursor image set on it, for example).

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::{self, OrgKdeKwinFakeInput};

const BTN_LEFT: u32 = 0x110;

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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |s: &String| s.parse::<f64>().expect("a number");
    let (verb, x, y, to) = match args.as_slice() {
        [v, x, y] => (v.as_str(), num(x), num(y), None),
        [v, x, y, n] if v == "click" => (v.as_str(), num(x), num(y), Some((num(n), 0.0))),
        [v, x, y, x2, y2] if v == "drag" || v == "rdrag" => (v.as_str(), num(x), num(y), Some((num(x2), num(y2)))),
        _ => {
            eprintln!("usage: fake_click move|click X Y, click X Y N, drag X Y X2 Y2, or rdrag X Y DX DY");
            std::process::exit(2);
        }
    };
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 3..=5, ()).expect("org_kde_kwin_fake_input (version 3 to 5)");
    fake.authenticate("vo fake_click".into(), "overlay input test".into());
    fake.pointer_motion_absolute(x, y);
    let pause = |queue: &mut wayland_client::EventQueue<State>| {
        queue.roundtrip(&mut State).expect("roundtrip");
        std::thread::sleep(std::time::Duration::from_millis(30));
    };
    match (verb, to) {
        ("move", _) => {}
        ("click", n) => {
            for _ in 0..n.map_or(1, |(n, _)| n as u32) {
                pause(&mut queue);
                fake.button(BTN_LEFT, 1);
                pause(&mut queue);
                fake.button(BTN_LEFT, 0);
            }
        }
        ("drag", Some((x2, y2))) => {
            pause(&mut queue);
            fake.button(BTN_LEFT, 1);
            for i in 1..=8 {
                pause(&mut queue);
                let f = f64::from(i) / 8.0;
                fake.pointer_motion_absolute(x + (x2 - x) * f, y + (y2 - y) * f);
            }
            pause(&mut queue);
            fake.button(BTN_LEFT, 0);
        }
        ("rdrag", Some((dx, dy))) => {
            pause(&mut queue);
            fake.button(BTN_LEFT, 1);
            for _ in 1..=8 {
                pause(&mut queue);
                fake.pointer_motion(dx / 8.0, dy / 8.0);
            }
            pause(&mut queue);
            fake.button(BTN_LEFT, 0);
        }
        _ => {
            eprintln!("unknown verb {verb}");
            std::process::exit(2);
        }
    }
    queue.roundtrip(&mut State).expect("roundtrip");
    eprintln!("fake_click: {verb} at ({x}, {y})");
    if let Some(ms) = std::env::var("FAKE_CLICK_HOLD_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}
