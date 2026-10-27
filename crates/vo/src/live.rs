//! `run_overlay`: drives an [`App`] on a platform host. Every event is applied at its host
//! timestamp, every frame is sampled at its predicted present time, and a frame is asked for
//! only while something changed or moves.

use vo_host::{
    CpuFrame, Frame, HostApp, HostCtl, HostError, HostEvent, InputMode, OverlaySpec, Placement, SurfaceChange,
    SurfaceId,
};
use vo_render::cpu::CpuRenderer;
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, InputRegion};

use crate::app::{App, Driver};
use crate::{Options, Tokens};

/// Channel history kept by a live overlay, in seconds. Seeking is an offline need.
const LIVE_RETENTION: f64 = 10.0;

/// The host side of an extra surface (`App::surfaces`); its scene is in the driver.
struct LiveExtra {
    key: String,
    id: SurfaceId,
    /// Its own renderer: renderer state such as particles and pooled textures belongs to one
    /// surface.
    renderer: Renderer,
    list: DisplayList,
    region: Option<InputRegion>,
    /// When a capped-rate animation on it wants its next frame.
    paced: Option<f64>,
    frames: u64,
}

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
    /// With diagnostics: the display list of the last frame before a settle, to tell a frame
    /// drawn while idle.
    settled_list: Option<DisplayList>,
    /// The host asked for the next frame (resize, GPU hand-over), so it may repeat the last.
    host_asked: bool,
    surface: SurfaceId,
    list: DisplayList,
    /// The last region sent to the host, for `InputMode::Regions` specs.
    region: Option<InputRegion>,
    /// The last compositor blur region sent, from the glass panels in the list (FX07).
    blur: Option<InputRegion>,
    /// When a capped-rate animation (glass at 30 fps, FX08) wants its next frame.
    paced: Option<f64>,
    frames: u64,
    extras: Vec<LiveExtra>,
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
        let paced = self.paced;
        let d = self.driver();
        // A capped-rate animation is not due at every event (each present is one), only at
        // its own wake time.
        let due = d.scene.next_wake(now).is_some_and(|w| w <= now) || paced.is_some_and(|w| w <= now);
        if d.dirty || due {
            host.request_frame(id);
        }
        let next = d.timers.next_at();
        let paced = paced.filter(|&w| w > now);
        let extra_paced = self.sync_extras(host, now);
        let wake = [next, paced, extra_paced].into_iter().flatten().min_by(f64::total_cmp);
        host.wake_at(wake);
        if self.driver().done(now) && !self.driver().dirty && !self.driver().extras.list.iter().any(|e| e.dirty) {
            self.finish(host);
        }
    }

    /// Opens, closes and asks frames for the extra surfaces at `now`. Returns the earliest
    /// capped-rate wake time among them that is still to come.
    fn sync_extras(&mut self, host: &mut dyn HostCtl, now: f64) -> Option<f64> {
        // Extra surfaces draw on the GPU only; they open once it is ready.
        self.renderer.as_ref()?;
        for key in self.driver().extras.take_closed(now) {
            if let Some(i) = self.extras.iter().position(|x| x.key == key) {
                let x = self.extras.remove(i);
                host.close(x.id);
                if let Some(d) = self.diag() {
                    d.surface_closed(&x.key, now, x.frames);
                }
            }
        }
        let mut wake: Option<f64> = None;
        let n = self.driver().extras.list.len();
        for di in 0..n {
            let (key, spec, dirty, next) = {
                let e = &self.driver().extras.list[di];
                (e.key.clone(), e.spec.clone(), e.dirty, e.scene.next_wake(now))
            };
            let li = match self.extras.iter().position(|x| x.key == key) {
                Some(li) => li,
                None => match self.open_extra(host, &key, &spec) {
                    Ok(li) => li,
                    Err(e) => {
                        eprintln!("vo: cannot open surface {key}: {e}");
                        continue;
                    }
                },
            };
            let x = &mut self.extras[li];
            let paced_due = x.paced.is_some_and(|w| w <= now);
            if dirty || paced_due || next.is_some_and(|w| w <= now) {
                host.request_frame(x.id);
            }
            if let Some(w) = x.paced.filter(|&w| w > now) {
                wake = Some(wake.map_or(w, |v: f64| v.min(w)));
            }
        }
        wake
    }

    fn open_extra(&mut self, host: &mut dyn HostCtl, key: &str, spec: &OverlaySpec) -> Result<usize, HostError> {
        let id = host.open(spec)?;
        let gpu = host.gpu()?;
        let options = self.options_rest.as_ref().expect("started");
        let mut renderer = Renderer::with_fonts(&gpu.device, &gpu.queue, options.fonts.clone());
        options.configure(&mut renderer);
        self.extras.push(LiveExtra {
            key: key.to_string(),
            id,
            renderer,
            list: DisplayList::default(),
            region: None,
            paced: None,
            frames: 0,
        });
        let t = host.now();
        if let Some(d) = self.diag() {
            d.surface_opened(key, t);
        }
        Ok(self.extras.len() - 1)
    }

    /// The driver's index of the extra surface with host id `id`.
    fn extra_index(&mut self, id: SurfaceId) -> Option<usize> {
        let key = &self.extras.iter().find(|x| x.id == id)?.key;
        let key = key.clone();
        self.driver().extras.find(&key)
    }

    /// Draws a frame of an extra surface.
    fn extra_frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
        let Some(li) = self.extras.iter().position(|x| x.id == frame.id) else { return };
        let Some(di) = self.extra_index(frame.id) else { return };
        let driver = self.driver.as_mut().expect("started");
        let e = &mut driver.extras.list[di];
        let x = &mut self.extras[li];
        e.scene.sample_into(frame.t, &mut x.list);
        if matches!(e.spec.input, InputMode::Regions(_)) {
            let region = x.list.input_region();
            if x.region.as_ref() != Some(&region) {
                let _ = host.update(frame.id, SurfaceChange::Input(InputMode::Regions(region.clone())));
                x.region = Some(region);
            }
        }
        let mut target = Target::from_texture(frame.texture.clone());
        x.renderer.render_at(&x.list, frame.scale, &mut target);
        e.dirty = false;
        x.frames += 1;
        x.paced = None;
        match e.scene.next_wake(frame.t) {
            Some(w) if w > frame.t => x.paced = Some((w - 1.0 / 120.0).max(frame.t)),
            Some(_) => host.request_frame(frame.id),
            None => {}
        }
        let (key, n) = (x.key.clone(), x.frames - 1);
        if let Some(d) = self.diag() {
            d.surface_frame(&key, n, frame.t);
        }
        self.after_change(host);
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
        // The first view sets the surface margin, so the scene comes before the surface.
        let mut scene = options.scene().with_retention(LIVE_RETENTION);
        let t = host.now();
        scene.set_viewport(t, self.spec.size, 1.0);
        let mut driver = Driver::with_surfaces(app, scene, self.tokens.clone(), t, options.scene_maker());
        if let Some(m) = options.margin.reach(&driver.scene, t) {
            let content = self.spec.size;
            let fit = vo_scene::surface_for(content, m);
            self.spec.size = fit.size;
            self.spec.placement = inset_placement(self.spec.placement, fit.origin.y);
            driver.scene.set_viewport(t, fit.size, 1.0);
            let (right, bottom) = (fit.size.w - content.w - fit.origin.x, fit.size.h - content.h - fit.origin.y);
            driver.set_margin(vo_scene::Insets::new(fit.origin.x, fit.origin.y, right, bottom), t);
        }
        if options.cpu_first_frame && host.gpu_in_background() {
            self.cpu = Some(CpuRenderer::new(options.fonts.clone()));
            self.surface = host.open(&self.spec)?;
        } else {
            self.surface = host.open(&self.spec)?;
            let gpu = host.gpu()?;
            let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, options.fonts.clone());
            options.configure(&mut r);
            self.renderer = Some(r);
        }
        if let Some(inbox) = &options.inbox {
            inbox.connect(host.waker());
        }
        self.driver = Some(driver);
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
        if host.caps().backdrop_blur {
            let blur = list.blur_region();
            if self.blur.as_ref() != Some(&blur) {
                let change = if blur.rects.is_empty() { None } else { Some(blur.clone()) };
                let _ = host.update(id, SurfaceChange::Blur(change));
                self.blur = Some(blur);
            }
        }
        self.list = list;
    }

    /// After a frame, CPU or GPU: report it, then ask for the next one or report the settle.
    fn after_frame(&mut self, host: &mut dyn HostCtl, id: SurfaceId, t: f64) {
        let n = self.frames;
        self.frames += 1;
        let host_asked = std::mem::take(&mut self.host_asked);
        if let Some(before) = self.settled_list.take() {
            if !host_asked && before == self.list {
                if let Some(d) = self.diag() {
                    d.idle_frame(n, t);
                }
            }
        }
        let host_diag = host.diag().clone();
        let moving = self.driver().moving(t);
        self.driver().dirty = false;
        if let Some(d) = self.diag() {
            d.frame(n, t, &host_diag);
        }
        self.paced = None;
        if moving {
            // Glass animates at a capped rate (FX08): a wake time after `t` is a timer, not
            // the next vblank.
            match self.driver().scene.next_wake(t) {
                Some(w) if w > t => {
                    // The frame comes at the first vblank after the request, so ask half a
                    // 60 Hz refresh early, or a 30 fps loop lands at 20 fps.
                    let w = (w - 1.0 / 120.0).max(t);
                    self.paced = Some(w);
                    let timer = self.driver().timers.next_at();
                    host.wake_at(Some(timer.map_or(w, |x| x.min(w))));
                }
                _ => host.request_frame(id),
            }
        } else {
            // The last frame until the next event: what follows is idle.
            if self.diag().is_some() {
                self.settled_list = Some(self.list.clone());
            }
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
                self.host_asked = true;
            }
            HostEvent::Pointer(ev) if ev.surface == self.surface => self.driver().pointer(&ev),
            HostEvent::Configured { id, t, size, scale } => {
                if let Some(di) = self.extra_index(id) {
                    let e = &mut self.driver().extras.list[di];
                    e.scene.set_viewport(t, size, scale);
                    e.dirty = true;
                }
            }
            HostEvent::Pointer(ev) => {
                if let Some(di) = self.extra_index(ev.surface) {
                    self.driver().pointer_on(Some(di), &ev);
                }
            }
            HostEvent::Closed { id, .. } if id != self.surface => {
                // The compositor closed it; it opens again if the app still returns its key.
                self.extras.retain(|x| x.id != id);
            }
            HostEvent::Timer { t } => self.driver().fire_timers(t),
            HostEvent::External { t } => {
                self.driver().fire_timers(t);
                self.drain_inbox(t);
            }
            HostEvent::GpuReady { .. } => match host.gpu() {
                Ok(gpu) => {
                    let fonts = self.options_rest.as_ref().map(|o| o.fonts.clone()).unwrap_or_default();
                    let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts);
                    if let Some(o) = &self.options_rest {
                        o.configure(&mut r);
                    }
                    self.renderer = Some(r);
                    // The GPU draws a frame even when the scene has settled: that frame
                    // replaces the CPU one.
                    self.driver().dirty = true;
                    self.host_asked = true;
                }
                Err(e) => {
                    self.error = Some(e);
                    host.exit();
                    return;
                }
            },
            HostEvent::Presented { id, frame, .. }
                if id == self.surface && !self.startup_logged && self.first_gpu_frame.is_some_and(|f| frame >= f) =>
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
        if frame.id != self.surface {
            self.extra_frame(host, frame);
            return;
        }
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
        settled_list: None,
        host_asked: false,
        surface: 0,
        list: DisplayList::default(),
        region: None,
        blur: None,
        paced: None,
        frames: 0,
        extras: Vec::new(),
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

/// The placement once the effect margin has moved the content `top` pixels down the surface:
/// `LineAt` names a line of the content, so it moves with it.
fn inset_placement(placement: Placement, top: f32) -> Placement {
    match placement {
        Placement::LineAt { fraction, y } => Placement::LineAt { fraction, y: y + top },
        other => other,
    }
}

#[cfg(test)]
mod inset_tests {
    use super::*;

    #[test]
    fn line_at_follows_the_top_inset() {
        let p = inset_placement(Placement::LineAt { fraction: 0.5, y: 120.0 }, 9.0);
        assert_eq!(p, Placement::LineAt { fraction: 0.5, y: 129.0 });
        assert_eq!(inset_placement(Placement::Centered, 9.0), Placement::Centered);
    }
}
