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
    /// A renderer kept from an earlier session on the same device (`Resident`), taken by the
    /// first GPU frame.
    warm: Option<Renderer>,
    /// Where a warm-up still running puts its renderer (`Resident::warm_up`).
    warming: Option<WarmSlot>,
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
    /// The main surface's size and place as the app last asked for them, before the margin
    /// (`App::main_surface`).
    asked: (vo_scene::geom::Size, Placement),
    /// The transparent margin round the view, from the first view (`Options::margin`).
    insets: vo_scene::Insets,
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
        self.follow_main_surface(host);
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

    /// Resizes and moves the main surface when the app asks for another size or place. The
    /// host draws nothing until the compositor confirms the size, so the next frame is laid
    /// out for it (the viewport follows `HostEvent::Configured`).
    fn follow_main_surface(&mut self, host: &mut dyn HostCtl) {
        let Some(asked) = self.driver().app.main_surface() else { return };
        if asked == self.asked {
            return;
        }
        self.asked = asked;
        let fit = vo_scene::surface_for(asked.0, self.insets);
        self.spec.size = fit.size;
        self.spec.placement = inset_placement(asked.1, self.insets);
        let id = self.surface;
        let result = host
            .update(id, SurfaceChange::Size(fit.size))
            .and_then(|()| host.update(id, SurfaceChange::Placement(self.spec.placement)));
        if let Err(e) = result {
            eprintln!("vo: cannot resize the surface: {e}");
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
            driver.scene.set_viewport(t, fit.size, 1.0);
            let (right, bottom) = (fit.size.w - content.w - fit.origin.x, fit.size.h - content.h - fit.origin.y);
            let insets = vo_scene::Insets::new(fit.origin.x, fit.origin.y, right, bottom);
            self.spec.placement = inset_placement(self.spec.placement, insets);
            self.insets = insets;
            driver.set_margin(insets, t);
        }
        if options.cpu_first_frame && host.gpu_in_background() {
            self.cpu = Some(CpuRenderer::new(options.fonts.clone()));
            self.surface = host.open(&self.spec)?;
        } else {
            self.surface = host.open(&self.spec)?;
            let gpu = host.gpu()?;
            self.take_warming();
            self.renderer = Some(main_renderer(&mut self.warm, &gpu, &options));
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
                    self.take_warming();
                    let options = self.options_rest.as_ref().expect("started");
                    self.renderer = Some(main_renderer(&mut self.warm, &gpu, options));
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

/// The main surface's renderer: the warm one if it reads the same font table, else a new one.
/// A warm renderer keeps its `renderer_setup` from the session that made it.
fn main_renderer<M>(warm: &mut Option<Renderer>, gpu: &vo_host::Gpu, options: &Options<M>) -> Renderer {
    match warm.take().filter(|r| r.fonts().same_table(&options.fonts)) {
        Some(mut r) => {
            r.forget_frames();
            r.set_quality(options.quality);
            r.set_light(options.light);
            r
        }
        None => {
            let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, options.fonts.clone());
            options.configure(&mut r);
            r
        }
    }
}

/// A renderer a warm-up thread is building, and then holds until a show takes it.
type WarmSlot = std::sync::Arc<std::sync::Mutex<Option<Renderer>>>;

impl<A: App> Live<A> {
    /// The warm-up's renderer, if it is done and no warm renderer is held yet. The device
    /// thread fills the slot before it hands the device over, so once the host has the device
    /// the slot is filled.
    fn take_warming(&mut self) {
        if self.warm.is_none() {
            self.warm = self.warming.take().and_then(|slot| slot.lock().ok()?.take());
        }
    }

    fn new(app: A, spec: OverlaySpec, options: Options<A::Msg>, warm: Option<Renderer>) -> Live<A> {
        let tokens = options.tokens.clone();
        let asked = (spec.size, spec.placement);
        Live {
            pending: Some((app, options)),
            spec,
            tokens,
            driver: None,
            options_rest: None,
            renderer: None,
            warm,
            warming: None,
            cpu: None,
            first_gpu_frame: None,
            settled_list: None,
            host_asked: false,
            surface: 0,
            asked,
            insets: vo_scene::Insets::ZERO,
            list: DisplayList::default(),
            region: None,
            blur: None,
            paced: None,
            frames: 0,
            extras: Vec::new(),
            startup_logged: false,
            finished: false,
            error: None,
        }
    }
}

/// Runs `app` as a live overlay on the platform host until the app exits or the compositor
/// closes the surface.
#[cfg(feature = "wayland")]
pub fn run_overlay_with<A: App>(app: A, spec: OverlaySpec, options: Options<A::Msg>) -> Result<(), HostError> {
    use vo_host::Host;
    let host = vo_host_wayland::WaylandHost::connect()?;
    let mut live = Live::new(app, spec, options, None);
    let result = host.run(&mut live);
    match live.error.take() {
        Some(e) => Err(e),
        None => result,
    }
}

/// An overlay that shows again and again in one process, such as a dictation popup. Between
/// shows it keeps the display connection, the GPU device and the main surface's renderer
/// (pipelines, glyph atlas, painters), so a show after the first draws its first frame on
/// the GPU without making any of them again. It holds no surface between shows.
///
/// Any error drops all of it; the next show connects again.
#[cfg(feature = "wayland")]
pub struct Resident {
    /// First, so it is dropped before the device's host.
    renderer: Option<Renderer>,
    /// The renderer `warm_up` is building, until a show takes it.
    warming: Option<WarmSlot>,
    host: Option<vo_host_wayland::WaylandHost>,
}

/// What [`Resident::warm_up`] draws once, offscreen, so the pipelines and glyphs a show uses
/// exist before the first show: display lists like the app's own frames, drawn at `size`
/// pixels and `scale`.
pub struct WarmFrames {
    pub lists: Vec<DisplayList>,
    pub size: [u32; 2],
    pub scale: f32,
}

/// The swapchain format the Wayland host picks when the compositor offers it, as KWin does.
/// Pipelines are made per format, so warming another format would not help the first show.
#[cfg(feature = "wayland")]
const WARM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

#[cfg(feature = "wayland")]
impl Default for Resident {
    fn default() -> Self {
        Resident::new()
    }
}

#[cfg(feature = "wayland")]
impl Resident {
    /// Holds nothing until the first show.
    pub fn new() -> Resident {
        Resident { renderer: None, warming: None, host: None }
    }

    /// Gets the first show ready before it comes: connects, then on a background thread makes
    /// the GPU device and the renderer the way a show would (`options`' fonts, quality, light
    /// and `renderer_setup`), draws `frames` once offscreen to build the pipelines and glyph
    /// atlas, and gives freed memory back. `frames` runs on that thread too.
    ///
    /// Only the connection is made here (well under a millisecond), so a show that comes
    /// during the warm-up is not held up: it starts as a cold show does and takes the device
    /// and renderer when they are ready. Returns whether a warm-up started; it does not when
    /// the device is already warm.
    pub fn warm_up<M>(
        &mut self,
        options: &Options<M>,
        frames: impl FnOnce() -> WarmFrames + Send + 'static,
    ) -> Result<bool, HostError> {
        if self.host.is_none() {
            self.renderer = None;
            self.warming = None;
            self.host = Some(vo_host_wayland::WaylandHost::connect()?);
        }
        let slot = WarmSlot::default();
        let (fonts, quality, light, setup) =
            (options.fonts.clone(), options.quality, options.light, options.renderer_setup.clone());
        let filled = slot.clone();
        let then: vo_host_wayland::WarmUp = Box::new(move |gpu| {
            let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts);
            r.set_quality(quality);
            r.set_light(light);
            if let Some(setup) = &setup {
                setup(&mut r);
            }
            let warm = frames();
            let [w, h] = warm.size;
            let mut target = Target::new(&gpu.device, w.max(1), h.max(1), WARM_FORMAT);
            for list in &warm.lists {
                r.render_at(list, warm.scale, &mut target);
            }
            let _ = gpu.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            r.forget_frames();
            drop(target);
            drop(warm);
            trim_heap();
            if let Ok(mut s) = filled.lock() {
                *s = Some(r);
            }
        });
        let started = self.host.as_mut().expect("connected").warm_up(then);
        if started {
            self.warming = Some(slot);
        }
        Ok(started)
    }

    /// Runs `app` as one show, like [`run_overlay_with`], and keeps the warm state for the
    /// next. The renderer keeps the `renderer_setup` of the show that made it; quality and
    /// light mode follow each show's options.
    pub fn run<A: App>(&mut self, app: A, spec: OverlaySpec, options: Options<A::Msg>) -> Result<(), HostError> {
        // A compositor that went away while hidden: connect again for this show.
        if self.host.as_mut().is_some_and(|h| h.check().is_err()) {
            self.release();
        }
        if self.host.is_none() {
            self.renderer = None;
            self.host = Some(vo_host_wayland::WaylandHost::connect()?);
        }
        let host = self.host.as_mut().expect("connected");
        let mut live = Live::new(app, spec, options, self.renderer.take());
        live.warming = self.warming.take();
        let result = host.run_session(&mut live);
        let result = match live.error.take() {
            Some(e) => Err(e),
            None => result,
        };
        match result {
            Ok(()) => {
                self.renderer = live.renderer.take();
                // A show too short to reach the GPU leaves the warm-up's renderer for the next.
                self.warming = live.warming.take();
            }
            Err(_) => {
                drop(live);
                self.release();
            }
        }
        trim_heap();
        result
    }

    /// Drops the renderer, the device and the connection. The next show makes them again.
    pub fn release(&mut self) {
        self.renderer = None;
        self.warming = None;
        self.host = None;
    }

    /// Whether a connection and device are kept from an earlier show.
    pub fn is_warm(&self) -> bool {
        self.host.is_some()
    }
}

/// Gives memory freed during a show back to the system. glibc keeps freed heap pages for
/// reuse; after the GPU driver's first shader compile that is about 35 MB the hidden popup
/// would hold all day. Runs after the surface is gone, so it costs no show any time.
#[cfg(all(feature = "wayland", target_os = "linux", target_env = "gnu"))]
fn trim_heap() {
    extern "C" {
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    // SAFETY: malloc_trim only takes the allocator's own locks and has no preconditions.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(all(feature = "wayland", not(all(target_os = "linux", target_env = "gnu"))))]
fn trim_heap() {}

/// The placement once the effect margin has grown the surface by `insets` around the content:
/// the content stays where it was without the margin. `Centered` is left as it is.
fn inset_placement(placement: Placement, insets: vo_scene::Insets) -> Placement {
    match placement {
        Placement::LineAt { fraction, y } => Placement::LineAt { fraction, y: y + insets.top },
        // The content's top edge at the fraction, not the surface's.
        Placement::FromTop { fraction } => Placement::LineAt { fraction, y: insets.top },
        Placement::Anchored { edges, margin } => {
            let [top, right, bottom, left] = margin;
            let pull = |m: i32, on: bool, inset: f32| if on { m - inset.round() as i32 } else { m };
            Placement::Anchored {
                edges,
                margin: [
                    pull(top, edges.top, insets.top),
                    pull(right, edges.right, insets.right),
                    pull(bottom, edges.bottom, insets.bottom),
                    pull(left, edges.left, insets.left),
                ],
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod inset_tests {
    use super::*;

    use vo_host::Edges;
    use vo_scene::Insets;

    const INSETS: Insets = Insets::new(6.0, 9.0, 7.0, 11.0);

    #[test]
    fn line_at_follows_the_top_inset() {
        let p = inset_placement(Placement::LineAt { fraction: 0.5, y: 120.0 }, INSETS);
        assert_eq!(p, Placement::LineAt { fraction: 0.5, y: 129.0 });
        assert_eq!(inset_placement(Placement::Centered, INSETS), Placement::Centered);
    }

    #[test]
    fn from_top_puts_the_content_top_at_the_fraction() {
        let p = inset_placement(Placement::FromTop { fraction: 0.85 }, INSETS);
        assert_eq!(p, Placement::LineAt { fraction: 0.85, y: 9.0 });
        assert_eq!(
            inset_placement(Placement::FromTop { fraction: 0.85 }, Insets::ZERO),
            Placement::LineAt { fraction: 0.85, y: 0.0 }
        );
    }

    #[test]
    fn anchored_margins_shrink_by_the_insets_on_anchored_edges() {
        let p = inset_placement(Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [24, 24, 24, 24] }, INSETS);
        assert_eq!(p, Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [24, 17, 13, 24] });
    }
}

#[cfg(test)]
mod resize_tests {
    use std::cell::Cell;

    use vo_host::{Gpu, HostCaps, HostDiag, Waker};
    use vo_scene::geom::Size;
    use vo_scene::Insets;

    use super::*;
    use crate::app::Cx;

    /// An app whose main surface size and place the test sets.
    struct Sized(Cell<Option<(Size, Placement)>>);

    impl App for Sized {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<()>) {}
        fn view(&self, _: &Tokens) -> vo_scene::element::Element<()> {
            vo_scene::element::Element::column("root")
        }
        fn main_surface(&self) -> Option<(Size, Placement)> {
            self.0.get()
        }
    }

    /// A host that records the changes asked of it.
    #[derive(Default)]
    struct Changes {
        caps: HostCaps,
        diag: HostDiag,
        log: Vec<SurfaceChange>,
    }

    impl HostCtl for Changes {
        fn caps(&self) -> &HostCaps {
            &self.caps
        }
        fn diag(&self) -> &HostDiag {
            &self.diag
        }
        fn now(&self) -> f64 {
            1.0
        }
        fn gpu(&mut self) -> Result<Gpu, HostError> {
            Err(HostError::Gpu("none in tests".into()))
        }
        fn open(&mut self, _: &OverlaySpec) -> Result<SurfaceId, HostError> {
            Ok(7)
        }
        fn update(&mut self, _: SurfaceId, change: SurfaceChange) -> Result<(), HostError> {
            self.log.push(change);
            Ok(())
        }
        fn close(&mut self, _: SurfaceId) {}
        fn request_frame(&mut self, _: SurfaceId) {}
        fn wake_at(&mut self, _: Option<f64>) {}
        fn waker(&self) -> Waker {
            Waker::new(|| {})
        }
        fn exit(&mut self) {}
    }

    #[test]
    fn a_new_main_surface_size_resizes_and_moves_the_open_surface() {
        let place = Placement::LineAt { fraction: 0.5, y: 40.0 };
        let spec = OverlaySpec { size: Size::new(600.0, 180.0), placement: place, ..OverlaySpec::default() };
        let app = Sized(Cell::new(None));
        let mut live = Live::new(app, spec, crate::Options::default(), None);
        let driver = Driver::new(live.pending.take().unwrap().0, vo_scene::Scene::new(), Tokens::default(), 0.0);
        live.driver = Some(driver);
        live.insets = Insets::new(10.0, 20.0, 10.0, 20.0);
        let mut host = Changes::default();

        // Nothing asked, or the same as at the start: no change.
        live.follow_main_surface(&mut host);
        live.driver().app.0.set(Some((Size::new(600.0, 180.0), place)));
        live.follow_main_surface(&mut host);
        assert!(host.log.is_empty());

        // A taller look with its line lower: the size and the place, margin included.
        let place2 = Placement::LineAt { fraction: 0.5, y: 90.0 };
        live.driver().app.0.set(Some((Size::new(700.0, 320.0), place2)));
        live.follow_main_surface(&mut host);
        assert_eq!(
            host.log,
            [
                SurfaceChange::Size(Size::new(720.0, 360.0)),
                SurfaceChange::Placement(Placement::LineAt { fraction: 0.5, y: 110.0 }),
            ]
        );
        // Asked once only.
        live.follow_main_surface(&mut host);
        assert_eq!(host.log.len(), 2);
    }
}
