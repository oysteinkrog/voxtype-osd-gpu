//! `render_offline`: the same app and scene, sampled at exact frame times and written as PNGs.
//! Scenario inputs and timers are applied at their own times, so the frames match a live run
//! that got the same inputs at the same times.

use std::path::Path;

use vo_core::time::FrameTime;
use vo_host::{Point, PointerEvent, PointerKind, Size};
use vo_render::offscreen::{write_png, Frame};
use vo_render::Offscreen;

use crate::app::{App, Driver};
use crate::Options;

/// One scripted input.
#[derive(Clone, Debug)]
pub enum Input<M> {
    Msg(M),
    Pointer { pos: Point, kind: PointerKind },
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
    pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { label: Some("vo-offline"), ..Default::default() }),
    )
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
/// `Options::diagnostics` are not used offline.
pub fn render_offline_with<A: App>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    out_dir: &Path,
    options: Options<A::Msg>,
) -> Result<u64, OfflineError> {
    std::fs::create_dir_all(out_dir)?;
    let mut written = 0u64;
    let mut extra_written: std::collections::HashMap<String, u64> = Default::default();
    let frames = Frames::UntilDone(scenario.duration);
    drive_surfaces(
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
    Ok(written)
}

/// The driver and scene a scenario starts with: the view applied at t = 0 on a viewport of
/// the scenario's size.
pub(crate) fn start<A: App>(app: A, scenario: &Scenario<A::Msg>, options: &mut Options<A::Msg>) -> Driver<A> {
    let mut scene = options.scene();
    scene.set_viewport(0.0, scenario.size, scenario.scale);
    Driver::with_surfaces(app, scene, options.tokens.clone(), 0.0, options.scene_maker())
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
        let timer_t = driver.timers.next_at().filter(|tm| *tm <= t);
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
                }
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
    drive_surfaces(app, scenario, fps, frames, options, sink, &mut |_, _| Ok(()))
}

/// [`drive`], and each extra surface's frames to `extra` with the surface's key, drawn at the
/// size its spec asks for at the scenario's scale.
pub(crate) fn drive_surfaces<A: App, E: From<OfflineError>>(
    app: A,
    scenario: &Scenario<A::Msg>,
    fps: f64,
    frames: Frames,
    mut options: Options<A::Msg>,
    sink: &mut dyn FnMut(Frame) -> Result<(), E>,
    extra: &mut dyn FnMut(&str, Frame) -> Result<(), E>,
) -> Result<u64, E> {
    let (device, queue) = headless_gpu()?;
    let mut off = Offscreen::with_fonts(&device, &queue, options.fonts.clone());
    options.configure(&mut off.renderer);
    let mut driver = start(app, scenario, &mut options);
    // One offscreen renderer per extra surface, by key: renderer state such as particles
    // belongs to one surface.
    let mut extra_off: Vec<(String, Offscreen)> = Vec::new();
    let mut extra_list = Default::default();
    let px = [(scenario.size.w * scenario.scale).round() as u32, (scenario.size.h * scenario.scale).round() as u32];

    let mut rendered = 0u64;
    let mut next_input = 0;
    let mut list = Default::default();
    for n in 0.. {
        let ft = FrameTime::at_rate(n, fps);
        match frames {
            Frames::UntilDone(end) if ft.t > end => break,
            Frames::Count(c) if n >= c => break,
            _ => {}
        }
        advance(&mut driver, &scenario.inputs, &mut next_input, ft.t);
        driver.scene.sample_into(ft.t, &mut list);
        rendered += 1;
        if let Some(frame) = off.submit(&list, px, scenario.scale) {
            sink(frame)?;
        }
        for e in &driver.extras.list {
            let i = match extra_off.iter().position(|(k, _)| *k == e.key) {
                Some(i) => i,
                None => {
                    let mut o = Offscreen::with_fonts(&device, &queue, options.fonts.clone());
                    options.configure(&mut o.renderer);
                    extra_off.push((e.key.clone(), o));
                    extra_off.len() - 1
                }
            };
            e.scene.sample_into(ft.t, &mut extra_list);
            let size =
                [(e.spec.size.w * scenario.scale).round() as u32, (e.spec.size.h * scenario.scale).round() as u32];
            if let Some(frame) = extra_off[i].1.submit(&extra_list, size, scenario.scale) {
                extra(&e.key, frame)?;
            }
        }
        for key in driver.extras.take_closed(ft.t) {
            if let Some(i) = extra_off.iter().position(|(k, _)| *k == key) {
                let (key, mut o) = extra_off.remove(i);
                for frame in o.finish() {
                    extra(&key, frame)?;
                }
            }
        }
        if matches!(frames, Frames::UntilDone(_)) && driver.done(ft.t) {
            break;
        }
    }
    for frame in off.finish() {
        sink(frame)?;
    }
    for (key, mut o) in extra_off {
        for frame in o.finish() {
            extra(&key, frame)?;
        }
    }
    Ok(rendered)
}
