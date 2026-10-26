//! Injects pointer input into a nested KWin through `org_kde_kwin_fake_input`, for testing
//! overlays without a person at the mouse. KWin only allows this protocol to trusted clients;
//! start the nested KWin with `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`. Never point this at the
//! real session.
//!
//!   fake_click move X Y        move the pointer to global logical (X, Y)
//!   fake_click click X Y       move there, then press and release the left button

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::{self, OrgKdeKwinFakeInput};

const BTN_LEFT: u32 = 0x110;

struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut State, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<OrgKdeKwinFakeInput, ()> for State {
    fn event(_: &mut State, _: &OrgKdeKwinFakeInput, _: org_kde_kwin_fake_input::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (verb, x, y) = match args.as_slice() {
        [v, x, y] => (v.as_str(), x.parse::<f64>().expect("X"), y.parse::<f64>().expect("Y")),
        _ => {
            eprintln!("usage: fake_click move|click X Y");
            std::process::exit(2);
        }
    };
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 3..=6, ()).expect("org_kde_kwin_fake_input (version 3 or later)");
    fake.authenticate("vo fake_click".into(), "overlay input test".into());
    fake.pointer_motion_absolute(x, y);
    if verb == "click" {
        queue.roundtrip(&mut State).expect("roundtrip");
        std::thread::sleep(std::time::Duration::from_millis(30));
        fake.button(BTN_LEFT, 1);
        queue.roundtrip(&mut State).expect("roundtrip");
        std::thread::sleep(std::time::Duration::from_millis(30));
        fake.button(BTN_LEFT, 0);
    } else if verb != "move" {
        eprintln!("unknown verb {verb}");
        std::process::exit(2);
    }
    queue.roundtrip(&mut State).expect("roundtrip");
    eprintln!("fake_click: {verb} at ({x}, {y})");
}
