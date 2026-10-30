//! `render_offline`: the same app and scene, sampled at exact frame times and written as PNGs.
//! Scenario inputs and timers are applied at their own times, so the frames match a live run
//! that got the same inputs at the same times.

use std::path::Path;

use vo_core::time::FrameTime;
use vo_host::{
    KeyEvent, KeyState, LogicalKey, Mods, NamedKey, Placement, Point, PointerEvent, PointerKind, RepeatInfo, Size,
};
use vo_render::offscreen::{write_png, Frame};
use vo_render::Offscreen;

use crate::app::{App, Driver, EXIT_MOTION_MAX};
use crate::Options;

/// One scripted input.
#[derive(Clone, Debug)]
pub enum Input<M> {
    Msg(M),
    Pointer {
        pos: Point,
        kind: PointerKind,
    },
    /// A key, as the host sends it (`Down` and `Up`; the driver makes the repeats). Its `t`
    /// is replaced by the input's time.
    Key(KeyEvent),
    /// Keyboard focus came or went (`App::keyboard_focus`). Offline, focus comes only from
    /// these inputs: `Cx::take_keyboard_focus` opens nothing.
    KeyboardFocus {
        gained: bool,
        requested: bool,
    },
    /// The modifier state changed.
    Modifiers(Mods),
    /// The compositor's key repeat rate and delay. Without it nothing repeats.
    RepeatInfo(RepeatInfo),
}

/// Seconds between the press and the release of a scripted key, and between scripted keys.
pub const KEY_STEP: f64 = 0.03;

/// The evdev code scripted text uses for a character: the same for its press and release,
/// and outside the codes of a real keyboard.
fn char_code(c: char) -> u32 {
    0x1_0000 + c as u32
}

/// What an offline render feeds the app.
#[derive(Clone, Debug)]
pub struct Scenario<M> {
    /// Logical size of the surface.
    pub size: Size,
    /// Physical pixels per logical pixel.
    pub scale: f32,
    /// Seconds. Rendering stops after this, or earlier once the app exits and settles.
    pub duration: f64,
    /// `(t, input)` in time order.
    pub inputs: Vec<(f64, Input<M>)>,
}

impl<M> Scenario<M> {
    /// Adds `input` at `t`, after any input already at or before `t`, so the inputs stay in
    /// time order.
    pub fn push(&mut self, t: f64, input: Input<M>) {
        let at = self.inputs.partition_point(|(it, _)| *it <= t);
        self.inputs.insert(at, (t, input));
    }

    /// A press and release of `key` with `mods` held, at `t`. Returns when it ends.
    pub fn key(&mut self, t: f64, key: LogicalKey, text: Option<&str>, mods: Mods) -> f64 {
        let code = match key {
            LogicalKey::Char(c) => char_code(c),
            LogicalKey::Named(n) => 0x2_0000 + n as u32,
            LogicalKey::Other(sym) => 0x3_0000 + sym,
        };
        let ev = KeyEvent { surface: 1, t, state: KeyState::Down, key, code, text: text.map(str::to_owned), mods };
        self.push(t, Input::Key(ev.clone()));
        self.push(t + KEY_STEP, Input::Key(KeyEvent { state: KeyState::Up, text: None, ..ev }));
        t + KEY_STEP
    }

    /// Types `text` from `t`, one key every `2 * KEY_STEP` seconds: each character a press
    /// and release with that text, upper-case letters with Shift, and a line break as
    /// Shift+Enter (the text box's newline). Returns when the last key is released.
    pub fn type_text(&mut self, t: f64, text: &str) -> f64 {
        let mut at = t;
        for c in text.chars() {
            at = if c == '\n' {
                self.chord(at, Mods::SHIFT, LogicalKey::Named(NamedKey::Enter))
            } else {
                let mods = if c.is_uppercase() { Mods::SHIFT } else { Mods::NONE };
                self.key(at, LogicalKey::Char(c), Some(c.encode_utf8(&mut [0; 4])), mods)
            } + KEY_STEP;
        }
        at - KEY_STEP
    }

    /// `key` pressed with `mods` held, at `t`: the modifier state changes first, then the
    /// key is pressed and released with no text, then the modifiers are let go. For Ctrl+Z,
    /// Ctrl+Left, Shift+Enter. Returns when the modifiers are let go.
    pub fn chord(&mut self, t: f64, mods: Mods, key: LogicalKey) -> f64 {
        self.push(t, Input::Modifiers(mods));
        let up = self.key(t + KEY_STEP, key, None, mods);
        self.push(up + KEY_STEP, Input::Modifiers(Mods::NONE));
        up + KEY_STEP
    }
}

/// How an offline run ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ending {
    /// The app asked to exit at `exit_at` and nothing moved any more at `at`, the first frame
    /// time with no motion.
    Settled { exit_at: f64, at: f64 },
    /// The app asked to exit at `exit_at` and something still moved at `at`, when the exit
    /// motion cap (`EXIT_MOTION_MAX` seconds) ended the run. Live, this is the
    /// "vo: still moving ... closing anyway" case.
    Capped { exit_at: f64, at: f64 },
    /// The run stopped first: the scenario ran out or the frame count was reached. `exit_at`
    /// is when the app asked to exit, if it did.
    Unfinished { exit_at: Option<f64> },
}

impl Ending {
    /// Seconds from the exit to the end of the run, for an app that exited and ended.
    pub fn after_exit(&self) -> Option<f64> {
        match *self {
            Ending::Settled { exit_at, at } | Ending::Capped { exit_at, at } => Some(at - exit_at),
            Ending::Unfinished { .. } => None,
        }
    }

    /// The exit motion cap ended the run.
    pub fn capped(&self) -> bool {
        matches!(self, Ending::Capped { .. })
    }

    /// The seconds an exit motion may run before the cap ends it.
    pub const CAP: f64 = EXIT_MOTION_MAX;
}

/// What an offline run did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OfflineReport {
    /// Frames rendered (or stepped, for [`exit_check`]).
    pub frames: u64,
    pub ending: Ending,
}

/// How the run stands at `t`, the frame time it stopped at.
fn ending_at<A: App>(driver: &Driver<A>, t: f64) -> Ending {
    match driver.exit_at() {
        Some(exit_at) if driver.done(t) => {
            if driver.overdue(t) && (driver.moving(t) || driver.extras.moving(t)) {
                Ending::Capped { exit_at, at: t }
            } else {
                Ending::Settled { exit_at, at: t }
            }
        }
        exit_at => Ending::Unfinished { exit_at },
    }
}

/// The live overlay's line for a run the cap ended, on stderr.
fn warn_capped(ending: &Ending) {
    if ending.capped() {
        eprintln!("vo: still moving {EXIT_MOTION_MAX} s after the exit, closing anyway");
    }
}

#[derive(Debug)]
pub enum OfflineError {
    Gpu(String),
    Io(std::io::Error),
}

impl std::fmt::Display for OfflineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OfflineError::Gpu(s) => write!(f, "GPU: {s}"),
            OfflineError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for OfflineError {}

impl From<std::io::Error> for OfflineError {
    fn from(e: std::io::Error) -> OfflineError {
        OfflineError::Io(e)
    }
}

pub(crate) fn headless_gpu() -> Result<(wgpu::Device, wgpu::Queue), OfflineError> {
    headless_gpu_for(false)
}

/// [`headless_gpu`], with GPU timestamps when `timestamps` and the adapter has them.
pub(crate) fn headless_gpu_for(timestamps: bool) -> Result<(wgpu::Device, wgpu::Queue), OfflineError> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| OfflineError::Gpu(e.to_string()))?;
    let required_features = if timestamps {
        adapter.features() & vo_render::profile::timestamp_features()
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vo-offline"),
        required_features,
        ..Default::default()
    }))
    .map_err(|e| OfflineError::Gpu(e.to_string()))
}

/// Renders `app` through `scenario` at `fps` into `out_dir/frame_00000.png` and onwards.
/// Returns the number of frames written.
pub fn render_offline<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    out_dir: &Path,
) -> Result<u64, OfflineError> {
    render_offline_with(app, scenario, fps, out_dir, Options::default())
}

/// [`render_offline`] with fonts, a text measurer and tokens. `Options::inbox` and
/// `Options::diagnostics` are not used offline; `Options::frame_timings` gets each frame's GPU
/// time. When the exit motion cap ends the run, prints the live overlay's
/// "vo: still moving ..." line on stderr.
pub fn render_offline_with<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    out_dir: &Path,
    options: Options<A::Msg>,
) -> Result<u64, OfflineError> {
    let report = render_offline_report(app, scenario, fps, out_dir, options)?;
    warn_capped(&report.ending);
    Ok(report.frames)
}

/// [`render_offline_with`], returning how the run ended as well, with no stderr line.
pub fn render_offline_report<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    out_dir: &Path,
    options: Options<A::Msg>,
) -> Result<OfflineReport, OfflineError> {
    std::fs::create_dir_all(out_dir)?;
    let mut written = 0u64;
    let mut extra_written: std::collections::HashMap<String, u64> = Default::default();
    let frames = Frames::UntilDone(scenario.duration);
    let (frames, ending) = drive_surfaces(
        app,
        scenario,
        fps,
        frames,
        options,
        &mut |frame: Frame| -> Result<(), OfflineError> {
            write_png(&out_dir.join(format!("frame_{written:05}.png")), frame.width, frame.height, &frame.rgba)?;
            written += 1;
            Ok(())
        },
        &mut |key: &str, frame: Frame| -> Result<(), OfflineError> {
            let dir = out_dir.join(key);
            let n = extra_written.entry(key.to_string()).or_default();
            if *n == 0 {
                std::fs::create_dir_all(&dir)?;
            }
            write_png(&dir.join(format!("frame_{n:05}.png")), frame.width, frame.height, &frame.rgba)?;
            *n += 1;
            Ok(())
        },
    )?;
    Ok(OfflineReport { frames, ending })
}

/// Runs `app` through `scenario` at `fps` on the CPU, drawing nothing, and reports how its
/// exit ended: settled, or cut by the exit motion cap. Unlike a render it does not stop at
/// `scenario.duration` once the app has asked to exit, so a slow exit is never cut short; it
/// stops there only when the app has not asked to exit by then.
pub fn exit_check<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    mut options: Options<A::Msg>,
) -> OfflineReport {
    let mut driver = start(app, scenario, &mut options);
    let mut next_input = 0;
    let mut size = scenario.size;
    for n in 0.. {
        let ft = FrameTime::at_rate(n, fps);
        if ft.t > scenario.duration && driver.exit_at().is_none() {
            return OfflineReport { frames: n, ending: Ending::Unfinished { exit_at: None } };
        }
        advance(&mut driver, &scenario.inputs, &mut next_input, ft.t);
        size = follow_main_surface(&mut driver, size, scenario.scale, ft.t);
        driver.extras.take_closed(ft.t);
        if driver.done(ft.t) {
            return OfflineReport { frames: n + 1, ending: ending_at(&driver, ft.t) };
        }
    }
    unreachable!("an app that asked to exit is done by the exit motion cap")
}

/// The driver and scene a scenario starts with: the view applied at t = 0 on a viewport of
/// the scenario's size.
pub(crate) fn start<A: App>(app: A, scenario: &Scenario<A::Msg>, options: &mut Options<A::Msg>) -> Driver<A> {
    let mut scene = options.scene();
    scene.set_viewport(0.0, scenario.size, scenario.scale);
    let mut driver = Driver::with_surfaces(app, scene, options.tokens.clone(), 0.0, options.scene_maker());
    // Under reduced motion no fly-out opens (EG-08). A fly-out puts the panel where a popup
    // sits: centred across, its middle 85% of the way down.
    driver.extras.reduced = options.reduced_motion;
    let popup = Placement::LineAt { fraction: 0.85, y: 0.5 * scenario.size.h };
    driver.extras.anchor = crate::surfaces::PanelAnchor::of(popup, scenario.size, Default::default());
    driver
}

/// Lays the scene out at the size the app asks for after its updates (`App::main_surface`),
/// as a live run resizes the open surface. `size` is the surface's size so far; returns its
/// size from now on.
pub(crate) fn follow_main_surface<A: App>(driver: &mut Driver<A>, size: Size, scale: f32, t: f64) -> Size {
    match driver.app.main_surface() {
        Some((asked, _)) if asked != size => {
            driver.scene.set_viewport(t, asked, scale);
            asked
        }
        _ => size,
    }
}

/// Applies the scenario inputs from `inputs[*next..]` and the app's timers up to and including
/// `t`, in time order. Returns how many events it applied and the time of the last one.
pub(crate) fn advance<A: App>(
    driver: &mut Driver<A>,
    inputs: &[(f64, Input<A::Msg>)],
    next: &mut usize,
    t: f64,
) -> (usize, Option<f64>) {
    let (mut applied, mut last) = (0, None);
    loop {
        let input_t = inputs.get(*next).map(|(t, _)| *t).filter(|it| *it <= t);
        let timer_t = driver.next_timer_at().filter(|tm| *tm <= t);
        let at = match (input_t, timer_t) {
            (Some(i), Some(tm)) if tm <= i => {
                driver.fire_timer(tm);
                tm
            }
            (Some(_), _) => {
                let (at, input) = &inputs[*next];
                *next += 1;
                match input {
                    Input::Msg(m) => driver.dispatch(m.clone(), *at),
                    Input::Pointer { pos, kind } => {
                        driver.pointer(&PointerEvent { surface: 1, t: *at, pos: *pos, kind: *kind })
                    }
                    Input::Key(ev) => driver.key(&KeyEvent { t: *at, ..ev.clone() }),
                    Input::KeyboardFocus { gained, requested } => driver.keyboard_focus(*at, *gained, *requested),
                    Input::Modifiers(mods) => driver.modifiers(*at, *mods),
                    Input::RepeatInfo(info) => driver.repeat_info(*info),
                }
                // Offline, focus comes from the inputs; a request opens nothing.
                driver.focus = None;
                *at
            }
            (None, Some(tm)) => {
                driver.fire_timer(tm);
                tm
            }
            (None, None) => return (applied, last),
        };
        applied += 1;
        last = Some(at);
    }
}

/// Which frames [`drive`] renders.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Frames {
    /// Every frame up to and including this time, or fewer once the app exits and settles.
    UntilDone(f64),
    /// Exactly this many frames, whatever the app does.
    #[cfg_attr(not(feature = "export"), allow(dead_code))]
    Count(u64),
}

/// Runs `app` through `scenario` and hands each frame to `sink`, in order. Returns the number
/// of frames rendered. Extra surfaces (`App::surfaces`) are not drawn.
#[cfg_attr(not(feature = "export"), allow(dead_code))]
pub(crate) fn drive<A: App, E: From<OfflineError>>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    frames: Frames,
    options: Options<A::Msg>,
    sink: &mut dyn FnMut(Frame) -> Result<(), E>,
) -> Result<u64, E> {
    drive_surfaces(app, scenario, fps, frames, options, sink, &mut |_, _| Ok(())).map(|(n, _)| n)
}

/// [`drive`], and each extra surface's frames to `extra` with the surface's key, drawn at the
/// size its spec asks for at the scenario's scale. Returns the frame count and how the run
/// ended.
pub(crate) fn drive_surfaces<A: App, E: From<OfflineError>>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    frames: Frames,
    mut options: Options<A::Msg>,
    sink: &mut dyn FnMut(Frame) -> Result<(), E>,
    extra: &mut dyn FnMut(&str, Frame) -> Result<(), E>,
) -> Result<(u64, Ending), E> {
    let timings = options.frame_timings.clone();
    let (device, queue) = headless_gpu_for(timings.is_some())?;
    let profile = |r: &mut vo_render::Renderer| {
        if timings.is_some() {
            r.set_profiling(true);
        }
    };
    // Hands the frame just encoded on `r` to the timings sink.
    let report = |r: &mut vo_render::Renderer, surface: Option<&str>, frame: u64, t: f64, size: [u32; 2]| {
        if let Some(sink) = &timings {
            if let Some(tm) = r.last_timings() {
                sink(&crate::FrameTiming { surface, frame, t, size, timings: &tm });
            }
        }
    };
    let mut off = Offscreen::with_fonts(&device, &queue, options.fonts.clone());
    options.configure(&mut off.renderer);
    profile(&mut off.renderer);
    let mut driver = start(app, scenario, &mut options);
    // One offscreen renderer per extra surface, by key: renderer state such as particles
    // belongs to one surface.
    let mut extra_off: Vec<(String, Offscreen, u64)> = Vec::new();
    let mut extra_list = Default::default();
    let mut size = scenario.size;

    let mut rendered = 0u64;
    let mut next_input = 0;
    let mut list = Default::default();
    let mut ending = Ending::Unfinished { exit_at: None };
    for n in 0.. {
        let ft = FrameTime::at_rate(n, fps);
        match frames {
            Frames::UntilDone(end) if ft.t > end => {
                ending = Ending::Unfinished { exit_at: driver.exit_at() };
                break;
            }
            Frames::Count(c) if n >= c => {
                ending = Ending::Unfinished { exit_at: driver.exit_at() };
                break;
            }
            _ => {}
        }
        advance(&mut driver, &scenario.inputs, &mut next_input, ft.t);
        size = follow_main_surface(&mut driver, size, scenario.scale, ft.t);
        let px = [(size.w * scenario.scale).round() as u32, (size.h * scenario.scale).round() as u32];
        driver.scene.sample_into(ft.t, &mut list);
        rendered += 1;
        let ready = off.submit(&list, px, scenario.scale);
        report(&mut off.renderer, None, n, ft.t, px);
        if let Some(frame) = ready {
            sink(frame)?;
        }
        for e in &driver.extras.list {
            let i = match extra_off.iter().position(|(k, _, _)| *k == e.key) {
                Some(i) => i,
                None => {
                    let mut o = Offscreen::with_fonts(&device, &queue, options.fonts.clone());
                    options.configure(&mut o.renderer);
                    profile(&mut o.renderer);
                    extra_off.push((e.key.clone(), o, 0));
                    extra_off.len() - 1
                }
            };
            e.scene.sample_into(ft.t, &mut extra_list);
            let size =
                [(e.spec.size.w * scenario.scale).round() as u32, (e.spec.size.h * scenario.scale).round() as u32];
            let (_, o, drawn) = &mut extra_off[i];
            let ready = o.submit(&extra_list, size, scenario.scale);
            report(&mut o.renderer, Some(&e.key), *drawn, ft.t, size);
            *drawn += 1;
            if let Some(frame) = ready {
                extra(&e.key, frame)?;
            }
        }
        for key in driver.extras.take_closed(ft.t) {
            if let Some(i) = extra_off.iter().position(|(k, _, _)| *k == key) {
                let (key, mut o, _) = extra_off.remove(i);
                for frame in o.finish() {
                    extra(&key, frame)?;
                }
            }
        }
        if matches!(frames, Frames::UntilDone(_)) && driver.done(ft.t) {
            ending = ending_at(&driver, ft.t);
            break;
        }
    }
    for frame in off.finish() {
        sink(frame)?;
    }
    for (key, mut o, _) in extra_off {
        for frame in o.finish() {
            extra(&key, frame)?;
        }
    }
    Ok((rendered, ending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Cx, Tokens};
    use vo_core::edit::{EditAction, EditBuffer};
    use vo_scene::Element;

    /// A tiny text box: typed text goes in, Backspace deletes, Ctrl+Z undoes, Escape exits.
    struct TextBox {
        buf: EditBuffer,
        focused: bool,
        repeats: u32,
    }

    impl App for TextBox {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<()>) {}
        fn view(&self, _: &Tokens) -> Element<()> {
            Element::label("text", self.buf.text())
        }
        fn key(&mut self, ev: &KeyEvent, cx: &mut Cx<()>) -> bool {
            if !self.focused || ev.state == KeyState::Up {
                return false;
            }
            self.repeats += u32::from(ev.state == KeyState::Repeat);
            let action = match (ev.key, &ev.text) {
                (LogicalKey::Char('z'), _) if ev.mods.ctrl => EditAction::Undo,
                (LogicalKey::Named(NamedKey::Enter), _) if ev.mods.shift => EditAction::Insert("\n".into()),
                (LogicalKey::Named(NamedKey::Backspace), _) => {
                    EditAction::Delete(vo_core::edit::Dir::Back, vo_core::edit::Unit::Grapheme)
                }
                (LogicalKey::Named(NamedKey::Escape), _) => {
                    cx.exit();
                    return true;
                }
                (_, Some(t)) => EditAction::Insert(t.clone()),
                _ => return false,
            };
            self.buf.apply_plain(action, ev.t);
            true
        }
        fn keyboard_focus(&mut self, gained: bool, _: bool, _: &mut Cx<()>) {
            self.focused = gained;
        }
    }

    /// Runs a scenario through the driver with no GPU, as `exit_check` does.
    fn run_keys(scenario: &Scenario<()>) -> Driver<TextBox> {
        let app = TextBox { buf: EditBuffer::new(""), focused: false, repeats: 0 };
        let mut driver = start(app, scenario, &mut Options::default());
        let mut next = 0;
        advance(&mut driver, &scenario.inputs, &mut next, scenario.duration);
        driver
    }

    #[test]
    fn type_text_and_chord_run_headless() {
        let mut s = Scenario { size: Size::new(300.0, 80.0), scale: 1.0, duration: 10.0, inputs: Vec::new() };
        s.push(0.5, Input::KeyboardFocus { gained: true, requested: false });
        let t = s.type_text(1.0, "Hei på\ndeg");
        // A pause, so the next word is its own undo step.
        let t = s.type_text(t + 2.0, " du");
        let t = s.chord(t + 0.5, Mods::CTRL, LogicalKey::Char('z'));
        s.key(t + 0.5, LogicalKey::Named(NamedKey::Escape), None, Mods::NONE);
        assert!(s.inputs.windows(2).all(|w| w[0].0 <= w[1].0), "inputs stay in time order");
        let d = run_keys(&s);
        assert_eq!(d.app.buf.text(), "Hei på\ndeg");
        assert!(d.exit_at().is_some());
    }

    #[test]
    fn a_held_key_repeats_offline() {
        let mut s = Scenario { size: Size::new(300.0, 80.0), scale: 1.0, duration: 3.0, inputs: Vec::new() };
        s.push(0.0, Input::RepeatInfo(RepeatInfo { rate_hz: 20.0, delay_s: 0.5 }));
        s.push(0.1, Input::KeyboardFocus { gained: true, requested: false });
        s.type_text(0.2, "abc");
        let bs = KeyEvent {
            surface: 1,
            t: 0.0,
            state: KeyState::Down,
            key: LogicalKey::Named(NamedKey::Backspace),
            code: 14,
            text: None,
            mods: Mods::NONE,
        };
        // Held from 1.0 to 1.62: the press, then repeats at 1.5, 1.55 and 1.6.
        s.push(1.0, Input::Key(bs.clone()));
        s.push(1.62, Input::Key(KeyEvent { state: KeyState::Up, ..bs }));
        let d = run_keys(&s);
        assert_eq!(d.app.repeats, 3);
        assert_eq!(d.app.buf.text(), "");
    }

    /// An app that fades out when told to exit: its fade settles.
    struct FadeOut {
        leaving: bool,
    }

    impl App for FadeOut {
        type Msg = ();

        fn update(&mut self, _: (), cx: &mut Cx<()>) {
            self.leaving = true;
            cx.exit();
        }

        fn view(&self, _: &Tokens) -> Element<()> {
            Element::panel("root").size(100.0, 40.0).opacity(if self.leaving { 0.0 } else { 1.0 })
        }
    }

    /// An app with a dial that spins for ever, through its exit too.
    struct Spinner;

    impl App for Spinner {
        type Msg = ();

        fn update(&mut self, _: (), cx: &mut Cx<()>) {
            cx.exit();
        }

        fn view(&self, _: &Tokens) -> Element<()> {
            let dial = Element::shape("dial", vo_scene::display::ShapeKind::Circle)
                .size(40.0, 40.0)
                .ambient(vo_scene::LoopProp::Rotation, vo_scene::Loop::spin(1.0));
            Element::column("root").size(100.0, 100.0).child(dial)
        }
    }

    fn exit_at_one(duration: f64) -> Scenario<()> {
        Scenario { size: Size::new(200.0, 100.0), scale: 1.0, duration, inputs: vec![(1.0, Input::Msg(()))] }
    }

    /// An empty view that asks for a wider surface once its message comes.
    struct Grows {
        wide: bool,
    }

    impl App for Grows {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<()>) {
            self.wide = true;
        }
        fn view(&self, _: &Tokens) -> Element<()> {
            Element::column("root")
        }
        fn main_surface(&self) -> Option<(Size, Placement)> {
            Some((Size::new(if self.wide { 400.0 } else { 200.0 }, 100.0), Placement::Centered))
        }
    }

    /// Offline, as live, the frames take the size the app asks for after its updates; they
    /// kept the scenario's size, so a look switch to a wider look was cut off (t0xg).
    #[test]
    fn offline_frames_follow_the_main_surface() {
        let s =
            Scenario { size: Size::new(200.0, 100.0), scale: 1.0, duration: 2.0, inputs: vec![(1.0, Input::Msg(()))] };
        let mut widths = Vec::new();
        crate::envelope::sample_scenario(Grows { wide: false }, &s, 10.0, Options::default(), &mut |_, t, l| {
            widths.push((t, l.size.w));
        });
        assert!(widths.iter().all(|&(t, w)| w == if t < 1.0 { 200.0 } else { 400.0 }), "{widths:?}");
    }

    #[test]
    fn an_exit_that_settles_reports_when() {
        let r = exit_check(FadeOut { leaving: false }, &exit_at_one(10.0), 60.0, Options::default());
        let Ending::Settled { exit_at, at } = r.ending else { panic!("settles: {r:?}") };
        assert_eq!(exit_at, 1.0);
        assert!(at > 1.0 && at < 1.0 + EXIT_MOTION_MAX, "the fade plays, then settles: {at}");
        assert_eq!(r.ending.after_exit(), Some(at - 1.0));
    }

    #[test]
    fn a_loop_that_never_stops_reports_the_cap() {
        // The scenario ends before the cap: a check still runs to it.
        let r = exit_check(Spinner, &exit_at_one(2.0), 60.0, Options::default());
        assert!(r.ending.capped(), "{r:?}");
        let after = r.ending.after_exit().unwrap();
        assert!((after - EXIT_MOTION_MAX).abs() < 1.0 / 60.0 + 1e-9, "cut at the cap: {after}");
    }

    #[test]
    fn an_app_that_never_exits_is_unfinished() {
        let s = Scenario { inputs: Vec::new(), ..exit_at_one(2.0) };
        let r = exit_check(Spinner, &s, 10.0, Options::default());
        assert_eq!(r.ending, Ending::Unfinished { exit_at: None });
    }
}
