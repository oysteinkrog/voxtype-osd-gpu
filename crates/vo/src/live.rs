//! `run_overlay`: drives an [`App`] on a platform host. Every event is applied at its host
//! timestamp, every frame is sampled at its predicted present time, and a frame is asked for
//! only while something changed or moves.

use vo_host::{
    CpuFrame, Frame, HostApp, HostCtl, HostError, HostEvent, InputMode, OverlaySpec, SurfaceChange, SurfaceId,
};
use vo_render::cpu::CpuRenderer;
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, InputRegion};

use crate::app::{App, Driver};
use crate::{Options, Tokens};

/// Channel history kept by a live overlay, in seconds. Seeking is an offline need.
const LIVE_RETENTION: f64 = 10.0;

struct Live<A: App> {
    pending: Option<(A, Options<A::Msg>)>,
    spec: OverlaySpec,
    tokens: Tokens,
    driver: Option<Driver<A>>,
    options_rest: Option<Options<A::Msg>>,
    renderer: Option<Renderer>,
    /// Draws the first frames while the GPU device is made; dropped once the GPU draws.
    cpu: Option<CpuRenderer>,
    /// The host's number of the first GPU frame, for the startup report.
    first_gpu_frame: Option<u64>,
    surface: SurfaceId,
    list: DisplayList,
    /// The last region sent to the host, for `InputMode::Regions` specs.
    region: Option<InputRegion>,
    frames: u64,
    startup_logged: bool,
    finished: bool,
    error: Option<HostError>,
}

impl<A: App> Live<A> {
    fn driver(&mut self) -> &mut Driver<A> {
        self.driver.as_mut().expect("started")
    }

    /// After any change: ask for a frame if needed, arm the next timer, exit when done.
    fn after_change(&mut self, host: &mut dyn HostCtl) {
        let now = host.now();
        let id = self.surface;
        let d = self.driver();
        if d.dirty || d.moving(now) {
            host.request_frame(id);
        }
        let next = self.driver().timers.next_at();
        host.wake_at(next);
        if self.driver().done(now) && !self.driver().dirty {
            self.finish(host);
        }
    }

    fn finish(&mut self, host: &mut dyn HostCtl) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let host_diag = host.diag().clone();
        if let Some(d) = self.diag() {
            d.exit(&host_diag);
        }
        host.exit();
    }

    fn start_inner(&mut self, host: &mut dyn HostCtl) -> Result<(), HostError> {
        let (app, mut options) = self.pending.take().expect("start runs once");
        if options.cpu_first_frame && host.gpu_in_background() {
            self.cpu = Some(CpuRenderer::new(options.fonts.clone()));
            self.surface = host.open(&self.spec)?;
        } else {
            self.surface = host.open(&self.spec)?;
            let gpu = host.gpu()?;
            self.renderer = Some(Renderer::with_fonts(&gpu.device, &gpu.queue, options.fonts.clone()));
        }
        if let Some(inbox) = &options.inbox {
            inbox.connect(host.waker());
        }
        let mut scene = options.scene().with_retention(LIVE_RETENTION);
        let t = host.now();
        scene.set_viewport(t, self.spec.size, 1.0);
        self.driver = Some(Driver::new(app, scene, self.tokens.clone(), t));
        self.options_rest = Some(options);
        self.drain_inbox(t);
        self.after_change(host);
        Ok(())
    }

    fn drain_inbox(&mut self, t: f64) {
        let msgs: Vec<A::Msg> =
            self.options_rest.as_ref().and_then(|o| o.inbox.as_ref()).map(|i| i.drain().collect()).unwrap_or_default();
        for m in msgs {
            self.driver().dispatch(m, t);
        }
    }

    fn diag(&mut self) -> Option<&mut Box<dyn crate::Diagnostics>> {
        self.options_rest.as_mut().and_then(|o| o.diagnostics.as_mut())
    }

    /// Samples the scene at `t` into `self.list` and sends a changed input region.
    fn sample(&mut self, host: &mut dyn HostCtl, id: SurfaceId, t: f64) {
        let mut list = std::mem::take(&mut self.list);
        self.driver().scene.sample_into(t, &mut list);
        if matches!(self.spec.input, InputMode::Regions(_)) {
            let region = list.input_region();
            if self.region.as_ref() != Some(&region) {
                let _ = host.update(id, SurfaceChange::Input(InputMode::Regions(region.clone())));
                self.region = Some(region);
            }
        }
        self.list = list;
    }

    /// After a frame, CPU or GPU: report it, then ask for the next one or report the settle.
    fn after_frame(&mut self, host: &mut dyn HostCtl, id: SurfaceId, t: f64) {
        let n = self.frames;
        self.frames += 1;
        let host_diag = host.diag().clone();
        let moving = self.driver().moving(t);
        self.driver().dirty = false;
        if let Some(d) = self.diag() {
            d.frame(n, t, &host_diag);
        }
        if moving {
            host.request_frame(id);
        } else {
            // The last frame until the next event: what follows is idle.
            if let Some(d) = self.diag() {
                d.settled(t, &host_diag);
            }
            if self.driver().done(t) {
                self.finish(host);
            }
        }
    }
}

impl<A: App> HostApp for Live<A> {
    fn start(&mut self, host: &mut dyn HostCtl) {
        if let Err(e) = self.start_inner(host) {
            self.error = Some(e);
            host.exit();
        }
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Configured { id, t, size, scale } if id == self.surface => {
                self.driver().scene.set_viewport(t, size, scale);
                self.driver().dirty = true;
            }
            HostEvent::Pointer(ev) if ev.surface == self.surface => self.driver().pointer(&ev),
            HostEvent::Timer { t } => self.driver().fire_timers(t),
            HostEvent::External { t } => {
                self.driver().fire_timers(t);
                self.drain_inbox(t);
            }
            HostEvent::GpuReady { .. } => match host.gpu() {
                Ok(gpu) => {
                    let fonts = self.options_rest.as_ref().map(|o| o.fonts.clone()).unwrap_or_default();
                    self.renderer = Some(Renderer::with_fonts(&gpu.device, &gpu.queue, fonts));
                    // The GPU draws a frame even when the scene has settled: that frame
                    // replaces the CPU one.
                    self.driver().dirty = true;
                }
                Err(e) => {
                    self.error = Some(e);
                    host.exit();
                    return;
                }
            },
            HostEvent::Presented { id, frame, .. }
                if id == self.surface && !self.startup_logged && self.first_gpu_frame == Some(frame) =>
            {
                self.startup_logged = true;
                let host_diag = host.diag().clone();
                if let Some(d) = self.diag() {
                    d.startup(&host_diag);
                }
            }
            HostEvent::Closed { id, .. } if id == self.surface => {
                self.finish(host);
                return;
            }
            _ => {}
        }
        self.after_change(host);
    }

    fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
        self.cpu = None;
        self.first_gpu_frame.get_or_insert(frame.frame);
        self.sample(host, frame.id, frame.t);
        let mut target = Target::from_texture(frame.texture.clone());
        self.renderer.as_mut().expect("GPU ready").render_at(&self.list, frame.scale, &mut target);
        if self.first_gpu_frame == Some(frame.frame) && !host.caps().present_feedback && !self.startup_logged {
            self.startup_logged = true;
            let host_diag = host.diag().clone();
            if let Some(d) = self.diag() {
                d.startup(&host_diag);
            }
        }
        self.after_frame(host, frame.id, frame.t);
    }

    fn cpu_frame(&mut self, host: &mut dyn HostCtl, frame: &mut CpuFrame<'_>) -> bool {
        if self.cpu.is_none() {
            return false;
        }
        self.sample(host, frame.id, frame.t);
        let [w, h] = frame.size;
        self.cpu.as_mut().unwrap().draw_bgra(&self.list, frame.scale, w, h, frame.stride, frame.pixels);
        self.after_frame(host, frame.id, frame.t);
        true
    }
}

/// Runs `app` as a live overlay on the platform host until the app exits or the compositor
/// closes the surface.
#[cfg(feature = "wayland")]
pub fn run_overlay_with<A: App>(app: A, spec: OverlaySpec, options: Options<A::Msg>) -> Result<(), HostError> {
    use vo_host::Host;
    let host = vo_host_wayland::WaylandHost::connect()?;
    let tokens = options.tokens.clone();
    let mut live = Live {
        pending: Some((app, options)),
        spec,
        tokens,
        driver: None,
        options_rest: None,
        renderer: None,
        cpu: None,
        first_gpu_frame: None,
        surface: 0,
        list: DisplayList::default(),
        region: None,
        frames: 0,
        startup_logged: false,
        finished: false,
        error: None,
    };
    let result = host.run(&mut live);
    match live.error.take() {
        Some(e) => Err(e),
        None => result,
    }
}
