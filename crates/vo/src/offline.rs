//! `render_offline`: the same app and scene, sampled at exact frame times and written as PNGs.
//! Scenario inputs and timers are applied at their own times, so the frames match a live run
//! that got the same inputs at the same times.

use std::path::Path;

use vo_core::time::FrameTime;
use vo_host::{PointerEvent, PointerKind, Point, Size};
use vo_render::offscreen::{write_png, Frame};
use vo_render::Offscreen;
use vo_scene::Scene;

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

fn headless_gpu() -> Result<(wgpu::Device, wgpu::Queue), OfflineError> {
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
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("vo-offline"), ..Default::default() }))
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
    mut options: Options<A::Msg>,
) -> Result<u64, OfflineError> {
    std::fs::create_dir_all(out_dir)?;
    let (device, queue) = headless_gpu()?;
    let mut off = Offscreen::new(&device, &queue);
    for font in options.fonts.drain(..) {
        off.renderer.add_font(font).map_err(|e| OfflineError::Gpu(format!("font: {e}")))?;
    }
    let mut scene = Scene::new();
    if let Some(text) = options.text.take() {
        scene = scene.with_text(text);
    }
    scene.set_viewport(0.0, scenario.size, scenario.scale);
    let mut driver = Driver::new(app, scene, options.tokens.clone(), 0.0);
    let px = [(scenario.size.w * scenario.scale).round() as u32, (scenario.size.h * scenario.scale).round() as u32];

    let mut written = 0u64;
    let mut write = |frame: Frame| -> Result<(), OfflineError> {
        write_png(&out_dir.join(format!("frame_{written:05}.png")), frame.width, frame.height, &frame.rgba)?;
        written += 1;
        Ok(())
    };
    let mut inputs = scenario.inputs.iter().peekable();
    let mut list = Default::default();
    for n in 0.. {
        let ft = FrameTime::at_rate(n, fps);
        if ft.t > scenario.duration {
            break;
        }
        // Inputs and timers up to this frame, in time order.
        loop {
            let input_t = inputs.peek().map(|(t, _)| *t).filter(|t| *t <= ft.t);
            let timer_t = driver.timers.next_at().filter(|t| *t <= ft.t);
            match (input_t, timer_t) {
                (Some(i), Some(tm)) if tm <= i => driver.fire_timers(tm),
                (Some(_), _) => {
                    let (t, input) = inputs.next().unwrap();
                    match input {
                        Input::Msg(m) => driver.dispatch(m.clone(), *t),
                        Input::Pointer { pos, kind } => {
                            driver.pointer(&PointerEvent { surface: 1, t: *t, pos: *pos, kind: *kind })
                        }
                    }
                }
                (None, Some(tm)) => driver.fire_timers(tm),
                (None, None) => break,
            }
        }
        driver.scene.sample_into(ft.t, &mut list);
        if let Some(frame) = off.submit(&list, px, scenario.scale) {
            write(frame)?;
        }
        if driver.done(ft.t) {
            break;
        }
    }
    for frame in off.finish() {
        write(frame)?;
    }
    Ok(written)
}
