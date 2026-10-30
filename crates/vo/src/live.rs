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

use crate::app::{App, Driver, FocusRequest, EXIT_MOTION_MAX};
use crate::surfaces::PanelAnchor;
use crate::{Options, SurfaceMargin, Tokens};

/// Channel history kept by a live overlay, in seconds. Seeking is an offline need.
const LIVE_RETENTION: f64 = 10.0;

/// A margin that follows the show grows in steps of this many logical pixels a side.
const MARGIN_STEP: f32 = 8.0;

/// The least a side grows by: a reach that creeps up, such as a glow that follows the voice,
/// then resizes the surface a few times a show, not at every step. Each later growth at least
/// doubles what the side has over the first view's margin.
const MARGIN_MIN_GROW: f32 = 32.0;

/// How long the first view's margin must be enough again before the surface shrinks back to
/// it, in seconds.
const MARGIN_HOLD: f64 = 1.0;

/// While the scene moves without a new view, the margin is checked this often, in seconds:
/// a particle's reach grows with time, and an effect that fades out lets the margin shrink.
const MARGIN_RECHECK: f64 = 0.5;

/// The transparent margin round the main view while it follows the show (EG-08). It grows at
/// once when the view reaches further, with room to spare (`MARGIN_MIN_GROW`), and shrinks
/// only all the way back to the first view's margin, once that has been enough for
/// `MARGIN_HOLD` and the app has not asked to exit. So a show resizes a few times at most:
/// every resize costs the compositor a configure. On an axis where the host centres the
/// surface, both sides grow by the same amount, so the panel stays where it is on screen.
#[derive(Clone, Debug, PartialEq)]
struct MarginFollow {
    /// The first view's margin: the least it ever is.
    base: vo_scene::Insets,
    /// The margin the surface has (or was last asked for).
    now: vo_scene::Insets,
    /// The host centres the surface across (`x`) or down (`y`) the output.
    paired: (bool, bool),
    /// Since when a smaller margin has been enough.
    below: Option<f64>,
    /// The most each side may have: the room between the panel and the output's edge. A
    /// taller or wider surface would be pushed back onto the output, moving the panel.
    cap: Option<vo_scene::Insets>,
}

impl MarginFollow {
    fn new(base: vo_scene::Insets, placement: Placement) -> Option<MarginFollow> {
        let paired = match placement {
            Placement::Centered => (true, true),
            Placement::Anchored { edges, .. } => (edges.left == edges.right, edges.top == edges.bottom),
            Placement::FromTop { .. } | Placement::LineAt { .. } => (true, false),
            Placement::Fullscreen => return None,
        };
        Some(MarginFollow { base, now: base, paired, below: None, cap: None })
    }

    /// The margin for a view that reaches `need`: the first view's, plus whole steps.
    fn want(&self, need: vo_scene::Insets) -> vo_scene::Insets {
        let extra = |need: f32, base: f32| {
            if need <= base + 0.01 {
                0.0
            } else {
                ((need - base) / MARGIN_STEP).ceil() * MARGIN_STEP
            }
        };
        let (b, n) = (self.base, need);
        let (mut l, mut t, mut r, mut bo) =
            (extra(n.left, b.left), extra(n.top, b.top), extra(n.right, b.right), extra(n.bottom, b.bottom));
        if self.paired.0 {
            l = l.max(r);
            r = l;
        }
        if self.paired.1 {
            t = t.max(bo);
            bo = t;
        }
        vo_scene::Insets::new(b.left + l, b.top + t, b.right + r, b.bottom + bo)
    }

    /// The new margin at `t` for a view that reaches `need`, or `None` to keep the one there.
    /// `may_shrink` is false once the app has asked to exit: the surface closes soon anyway.
    fn step(&mut self, need: vo_scene::Insets, t: f64, may_shrink: bool) -> Option<vo_scene::Insets> {
        let want = self.want(need);
        let (n, b) = (self.now, self.base);
        if want.left > n.left || want.top > n.top || want.right > n.right || want.bottom > n.bottom {
            // Each side that needs more gets room to spare; on a centred axis both sides then
            // take the larger extra.
            let grow = |w: f32, cur: f32, base: f32| {
                let (w, cur) = (w - base, cur - base);
                if w > cur {
                    w.max(2.0 * cur).max(MARGIN_MIN_GROW)
                } else {
                    cur
                }
            };
            let (mut l, mut t, mut r, mut bo) = (
                grow(want.left, n.left, b.left),
                grow(want.top, n.top, b.top),
                grow(want.right, n.right, b.right),
                grow(want.bottom, n.bottom, b.bottom),
            );
            if self.paired.0 {
                l = l.max(r);
                r = l;
            }
            if self.paired.1 {
                t = t.max(bo);
                bo = t;
            }
            // Never past the output's edge, nor below what the surface has.
            if let Some(c) = self.cap {
                let room = |c: f32, base: f32, cur: f32| (c - base).max(cur - base).max(0.0);
                l = l.min(room(c.left, b.left, n.left));
                r = r.min(room(c.right, b.right, n.right));
                t = t.min(room(c.top, b.top, n.top));
                bo = bo.min(room(c.bottom, b.bottom, n.bottom));
                if self.paired.0 {
                    l = l.min(r);
                    r = l;
                }
                if self.paired.1 {
                    t = t.min(bo);
                    bo = t;
                }
            }
            let grown = vo_scene::Insets::new(b.left + l, b.top + t, b.right + r, b.bottom + bo);
            self.below = None;
            if grown == n {
                return None;
            }
            self.now = grown;
            self.below = None;
            return Some(self.now);
        }
        if want != b || n == b || !may_shrink {
            self.below = None;
            return None;
        }
        let since = *self.below.get_or_insert(t);
        if t - since >= MARGIN_HOLD {
            self.now = b;
            self.below = None;
            return Some(b);
        }
        None
    }

    /// When a waiting shrink is due.
    fn wake(&self) -> Option<f64> {
        self.below.map(|b| b + MARGIN_HOLD)
    }
}

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
    /// The transparent margin round the view (`Options::margin`): the first view's, or the
    /// one the surface was last resized for while it follows the show.
    insets: vo_scene::Insets,
    /// The margin following the show (`SurfaceMargin::Auto` and `AtLeast`).
    follow: Option<MarginFollow>,
    /// A margin the surface was resized for: the view moves into it with the compositor's
    /// configure, so the panel stays still on screen.
    pending_margin: Option<vo_scene::Insets>,
    /// The main scene's view count (`Driver::views`) when the margin was last checked.
    margin_checked: u64,
    /// When the margin was last checked.
    margin_at: f64,
    list: DisplayList,
    /// The list `list` held before the last `prepare_frame` sampled into it: the frame on
    /// screen when that frame is kept. A spare buffer otherwise.
    shown: DisplayList,
    /// The time `prepare_frame` sampled `list` at, for the frame that follows it.
    prepared: Option<f64>,
    /// The last region sent to the host, for `InputMode::Regions` specs.
    region: Option<InputRegion>,
    /// The last compositor blur region sent, from the glass panels in the list (FX07).
    blur: Option<InputRegion>,
    /// The blur region of the last sampled list, built again only when its inputs change
    /// (RV-01): an outline costs a rasterise, a sample does not need one.
    blur_cache: vo_scene::glass::BlurCache,
    /// Whether "no compositor blur" was logged: once, the first time the list asks for blur
    /// behind on a host without it (EG-07). The glass then draws its plain tint.
    no_blur_logged: bool,
    /// When a capped-rate animation (glass at 30 fps, FX08) wants its next frame.
    paced: Option<f64>,
    /// The time of the last main frame, for the app's frame pace (`App::frame_pace`).
    last_frame: Option<f64>,
    frames: u64,
    extras: Vec<LiveExtra>,
    startup_logged: bool,
    finished: bool,
    error: Option<HostError>,
    /// The focus proxy open for the app (`Cx::take_keyboard_focus`).
    proxy: Option<SurfaceId>,
}

impl<A: App> Live<A> {
    fn driver(&mut self) -> &mut Driver<A> {
        self.driver.as_mut().expect("started")
    }

    /// After any change: ask for a frame if needed, arm the next timer, exit when done.
    fn after_change(&mut self, host: &mut dyn HostCtl) {
        self.follow_focus(host);
        self.follow_main_surface(host);
        let now = host.now();
        let id = self.surface;
        let mut paced = self.paced;
        // Under the app's frame pace a change waits for its slot; a frame the host asked for
        // (a resize, the GPU hand-over) does not.
        let slot = pace_slot(self.last_frame, self.driver().app.frame_pace()).filter(|&s| s > now && !self.host_asked);
        let d = self.driver();
        // A capped-rate animation is not due at every event (each present is one), only at
        // its own wake time.
        let due = d.scene.next_wake(now).is_some_and(|w| w <= now) || paced.is_some_and(|w| w <= now);
        if d.dirty || d.stale() || due {
            match slot {
                Some(s) => paced = Some(s),
                None => host.request_frame(id),
            }
        }
        let next = d.next_timer_at();
        let paced = paced.filter(|&w| w > now);
        // A new view may reach further: check it before its frame.
        let fresh = !self.driver().stale() && self.driver().views != self.margin_checked;
        let shrink_due = self.follow.as_ref().and_then(MarginFollow::wake).is_some_and(|w| w <= now);
        if fresh || shrink_due {
            self.follow_margin(host, now);
        }
        let extra_paced = self.sync_extras(host, now);
        let deadline = self.driver().exit_deadline();
        let fly_out = self.driver().extras.fly_out_deadline().filter(|&w| w > now);
        let shrink = self.follow.as_ref().and_then(MarginFollow::wake).filter(|&w| w > now);
        let wake = [next, paced, extra_paced, deadline, fly_out, shrink].into_iter().flatten().min_by(f64::total_cmp);
        host.wake_at(wake);
        let d = self.driver();
        let drawn = !d.dirty && !d.stale() && !d.extras.list.iter().any(|e| e.dirty);
        if d.done(now) && (drawn || d.overdue(now)) {
            self.finish(host);
        }
    }

    /// Opens or closes the focus proxy when the app asked to take or give back keyboard
    /// focus. A host that cannot take focus tells the app at once that it has none.
    fn follow_focus(&mut self, host: &mut dyn HostCtl) {
        let Some(request) = self.driver().focus.take() else { return };
        match request {
            FocusRequest::Take(spec) if self.proxy.is_none() => match host.open_focus_proxy(&spec) {
                Ok(id) => self.proxy = Some(id),
                Err(e) => {
                    eprintln!("vo: no keyboard focus: {e}");
                    let t = host.now();
                    self.driver().keyboard_focus(t, false, false);
                }
            },
            FocusRequest::Take(_) => {}
            FocusRequest::Release => {
                if let Some(id) = self.proxy.take() {
                    host.close_focus_proxy(id);
                }
            }
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
        self.set_anchor();
    }

    /// Checks the margin the view needs at `t` against the one the surface has, and resizes
    /// the surface when the margin follows the show and needs to change (EG-08).
    fn follow_margin(&mut self, host: &mut dyn HostCtl, t: f64) {
        if self.follow.is_none() {
            return;
        }
        self.margin_checked = self.driver().views;
        self.margin_at = t;
        let mut need = self.driver().scene.inner_reach(t);
        if let Some(SurfaceMargin::AtLeast(m)) = self.options_rest.as_ref().map(|o| o.margin) {
            need = need.max(m);
        }
        let may_shrink = self.driver().exit_at().is_none();
        if let (Some(out), Some(f)) = (host.output_size(), self.follow.as_mut()) {
            // The panel's place on the output, and the room on each side of it.
            let (content, placement) = self.asked;
            let a = PanelAnchor::of(placement, content, vo_scene::geom::Point::ZERO);
            let (x, y) = (a.fx * out.w + a.dx, a.fy * out.h + a.dy);
            f.cap = Some(vo_scene::Insets::new(
                x.floor().max(0.0),
                y.floor().max(0.0),
                (out.w - x - content.w).floor().max(0.0),
                (out.h - y - content.h).floor().max(0.0),
            ));
        }
        let Some(m) = self.follow.as_mut().and_then(|f| f.step(need, t, may_shrink)) else { return };
        self.insets = m;
        let fit = vo_scene::surface_for(self.asked.0, m);
        self.spec.size = fit.size;
        self.spec.placement = inset_placement(self.asked.1, m);
        self.pending_margin = Some(m);
        let id = self.surface;
        let result = host
            .update(id, SurfaceChange::Size(fit.size))
            .and_then(|()| host.update(id, SurfaceChange::Placement(self.spec.placement)));
        if let Err(e) = result {
            eprintln!("vo: cannot resize the surface for its margin: {e}");
        }
        if surface_log() {
            eprintln!(
                "vo: surface {}x{} at t {t:.3}, margin {} {} {} {}",
                fit.size.w, fit.size.h, m.left, m.top, m.right, m.bottom
            );
        }
        self.set_anchor();
    }

    /// Tells fly-outs where the panel now sits on its output.
    fn set_anchor(&mut self) {
        let inset = vo_scene::geom::Point::new(self.insets.left, self.insets.top);
        let anchor = PanelAnchor::of(self.spec.placement, self.spec.size, inset);
        if let Some(d) = self.driver.as_mut() {
            d.extras.anchor = anchor;
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
        let t = host.now();
        if let Some(id) = self.proxy.take() {
            host.close_focus_proxy(id);
        }
        if self.driver().overdue(t) && self.driver().moving(t) {
            eprintln!("vo: still moving {EXIT_MOTION_MAX} s after the exit, closing anyway");
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
        driver.lazy = true;
        driver.extras.reduced = options.reduced_motion;
        let follows = matches!(options.margin, SurfaceMargin::Auto | SurfaceMargin::AtLeast(_))
            && self.spec.placement != Placement::Fullscreen;
        let first = options.margin.reach(&driver.scene, t);
        if let Some(m) = first {
            let content = self.spec.size;
            let fit = vo_scene::surface_for(content, m);
            self.spec.size = fit.size;
            driver.scene.set_viewport(t, fit.size, 1.0);
            let (right, bottom) = (fit.size.w - content.w - fit.origin.x, fit.size.h - content.h - fit.origin.y);
            let insets = vo_scene::Insets::new(fit.origin.x, fit.origin.y, right, bottom);
            self.spec.placement = inset_placement(self.spec.placement, insets);
            self.insets = insets;
            driver.set_margin(insets, t);
        } else if follows {
            // No margin yet, but one may come: the view sits in an empty one from the start,
            // so a later margin is a padding change, not a new tree.
            driver.set_margin(vo_scene::Insets::ZERO, t);
        }
        if follows {
            self.follow = MarginFollow::new(self.insets, self.asked.1);
        }
        if surface_log() {
            let (s, m) = (self.spec.size, self.insets);
            eprintln!("vo: surface {}x{} at t {t:.3}, margin {} {} {} {}", s.w, s.h, m.left, m.top, m.right, m.bottom);
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
        self.set_anchor();
        self.margin_at = t;
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
        self.driver().flush();
        if let Some(below) = self.follow.as_ref().map(|f| f.below.is_some()) {
            let check = self.driver().views != self.margin_checked || below || t - self.margin_at >= MARGIN_RECHECK;
            if check && self.pending_margin.is_none() {
                self.follow_margin(host, t);
            }
        }
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
            let blur = self.blur_cache.region(&list);
            if self.blur.as_ref() != Some(blur) {
                let change = if blur.rects.is_empty() { None } else { Some(blur.clone()) };
                let _ = host.update(id, SurfaceChange::Blur(change));
                self.blur = Some(blur.clone());
            }
        } else if !self.no_blur_logged
            && (list.glass.iter().any(|g| g.glass.blur_behind) || !list.blur_behind.is_empty())
            && !self.blur_cache.region(&list).rects.is_empty()
        {
            eprintln!("vo: no compositor blur; glass draws its plain tint");
            self.no_blur_logged = true;
        }
        self.list = list;
    }

    /// After a frame, CPU or GPU: report it, then ask for the next one or report the settle.
    /// `drawn` is false for a frame kept on screen (`prepare_frame`): it paces and settles as
    /// a drawn one, but is not counted or reported as one.
    fn after_frame(&mut self, host: &mut dyn HostCtl, id: SurfaceId, t: f64, drawn: bool) {
        let n = self.frames;
        if drawn {
            self.frames += 1;
        }
        let host_asked = std::mem::take(&mut self.host_asked);
        if let Some(before) = self.settled_list.take() {
            if drawn && !host_asked && before == self.list {
                if let Some(d) = self.diag() {
                    d.idle_frame(n, t);
                }
            }
        }
        let host_diag = host.diag().clone();
        let moving = self.driver().moving(t) && !self.driver().overdue(t);
        self.driver().dirty = false;
        // The scene settled: what it needs now is what it keeps needing, so a margin grown for
        // an effect that has gone can start to shrink.
        let grown = self.follow.as_ref().is_some_and(|f| f.now != f.base);
        if !moving && grown && self.pending_margin.is_none() {
            self.follow_margin(host, t);
            if let Some(w) = self.follow.as_ref().and_then(MarginFollow::wake) {
                host.wake_at(Some(w));
            }
        }
        self.last_frame = Some(t);
        let pace = self.driver().app.frame_pace();
        if let Some(d) = self.diag() {
            if drawn {
                d.frame(n, t, &host_diag);
            } else {
                d.kept_frame(t);
            }
        }
        self.paced = None;
        if moving {
            // Glass animates at a capped rate (FX08): a wake time after `t` is a timer, not
            // the next vblank.
            // Under the app's frame pace (`App::frame_pace`) the next frame waits a whole
            // period; a later wake time stays later.
            let wake = self.driver().scene.next_wake(t).map(|w| match pace_period(pace) {
                Some(p) => w.max(t + p),
                None => w,
            });
            match wake {
                Some(w) if w > t => {
                    // The frame comes at the first vblank after the request, so ask half a
                    // 60 Hz refresh early, or a 30 fps loop lands at 20 fps.
                    let w = (w - 1.0 / 120.0).max(t);
                    self.paced = Some(w);
                    let timer = self.driver().next_timer_at();
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
                self.driver().flush();
                // A margin the surface was resized for: the view moves into it now, with the
                // size that makes room for it.
                if let Some(m) = self.pending_margin.take() {
                    self.driver().set_margin(m, t);
                }
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
            // Keys come to whichever surface has focus: the focus proxy, or a main surface
            // opened with keyboard interactivity.
            HostEvent::Key(ev) => self.driver().key(&ev),
            HostEvent::KeyboardFocus { id, t, gained, requested } => {
                if !gained && self.proxy == Some(id) {
                    // Focus was taken away: the proxy goes, and a new take opens a new one.
                    self.proxy = None;
                    host.close_focus_proxy(id);
                }
                self.driver().keyboard_focus(t, gained, requested);
            }
            HostEvent::Modifiers { t, mods, .. } => self.driver().modifiers(t, mods),
            HostEvent::KeyRepeatInfo { info, .. } => self.driver().repeat_info(info),
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
        if self.prepared.take() != Some(frame.t) {
            self.sample(host, frame.id, frame.t);
        }
        let mut target = Target::from_texture(frame.texture.clone());
        self.renderer.as_mut().expect("GPU ready").render_at(&self.list, frame.scale, &mut target);
        if self.first_gpu_frame == Some(frame.frame) && !host.caps().present_feedback && !self.startup_logged {
            self.startup_logged = true;
            let host_diag = host.diag().clone();
            if let Some(d) = self.diag() {
                d.startup(&host_diag);
            }
        }
        self.after_frame(host, frame.id, frame.t, true);
    }

    /// RD-14: under the app's frame pace, a frame whose display list draws the same pixels as
    /// the one on screen is kept: no encode, submit or present. Room tone on a look whose
    /// waveform holds still then costs one scene sample per paced frame. Frames the host
    /// asked for, the first GPU frames and frames without a pace are always drawn.
    fn prepare_frame(&mut self, host: &mut dyn HostCtl, id: SurfaceId, t: f64) -> bool {
        // A prepared frame that was never drawn (the host could not get a texture) left `list`
        // holding pixels that are not on screen; `shown` still holds those that are.
        let undrawn = self.prepared.take().is_some();
        let eligible = id == self.surface
            && self.cpu.is_none()
            && self.first_gpu_frame.is_some()
            && self.frames > 0
            && !self.host_asked
            && pace_period(self.driver().app.frame_pace()).is_some();
        if !eligible {
            return true;
        }
        if !undrawn {
            std::mem::swap(&mut self.list, &mut self.shown);
        }
        self.sample(host, id, t);
        if self.list.draws_same_as(&self.shown) {
            self.after_frame(host, id, t, false);
            return false;
        }
        self.prepared = Some(t);
        true
    }

    fn cpu_frame(&mut self, host: &mut dyn HostCtl, frame: &mut CpuFrame<'_>) -> bool {
        if self.cpu.is_none() {
            return false;
        }
        self.sample(host, frame.id, frame.t);
        let [w, h] = frame.size;
        self.cpu.as_mut().unwrap().draw_bgra(&self.list, frame.scale, w, h, frame.stride, frame.pixels);
        self.after_frame(host, frame.id, frame.t, true);
        true
    }
}

/// The main surface's renderer: the warm one if it reads the same font table, else a new one.
/// A warm renderer keeps its `renderer_setup` from the session that made it.
/// Seconds between frames under an app frame pace (`App::frame_pace`), when it caps anything.
fn pace_period(pace: Option<f64>) -> Option<f64> {
    pace.filter(|f| f.is_finite() && *f > 0.0).map(|f| 1.0 / f)
}

/// The earliest host time to ask for the next main frame under an app frame pace, from the last
/// frame's time: half a 60 Hz refresh before the slot, since the frame comes at the first vblank
/// after the request (as for glass in `after_frame`).
fn pace_slot(last_frame: Option<f64>, pace: Option<f64>) -> Option<f64> {
    Some(last_frame? + pace_period(pace)? - 1.0 / 120.0)
}

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
            follow: None,
            pending_margin: None,
            margin_checked: 0,
            margin_at: 0.0,
            list: DisplayList::default(),
            shown: DisplayList::default(),
            prepared: None,
            region: None,
            blur: None,
            blur_cache: Default::default(),
            no_blur_logged: false,
            paced: None,
            last_frame: None,
            frames: 0,
            extras: Vec::new(),
            startup_logged: false,
            finished: false,
            error: None,
            proxy: None,
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

    /// Whether the warm-up `warm_up` started has finished, so a show started now draws on the
    /// warm renderer from its first GPU frame. True when no warm-up is running. A tool that
    /// measures a show as one after the first would be waits for this before `run`.
    pub fn warm_up_done(&self) -> bool {
        self.warming.as_ref().is_none_or(|slot| slot.lock().map_or(true, |r| r.is_some()))
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

/// Whether to log the main surface's size and margin at the start and at each margin change
/// (`VO_SURFACE_LOG=1`).
fn surface_log() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VO_SURFACE_LOG").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes")))
}

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

#[cfg(test)]
mod pace_tests {
    use std::cell::Cell;

    use vo_host::{Gpu, HostCaps, HostDiag, Waker};

    use super::*;
    use crate::app::Cx;

    /// An app that redraws a counter on every message, at the pace the test sets.
    struct Counter {
        n: u32,
        pace: Cell<Option<f64>>,
        /// How many times the view was built.
        views: Cell<u32>,
    }

    impl App for Counter {
        type Msg = ();
        fn update(&mut self, _: (), _: &mut Cx<()>) {
            self.n += 1;
        }
        fn view(&self, _: &Tokens) -> vo_scene::element::Element<()> {
            self.views.set(self.views.get() + 1);
            vo_scene::element::Element::column("root").size(100.0 + self.n as f32, 50.0)
        }
        fn frame_pace(&self) -> Option<f64> {
            self.pace.get()
        }
    }

    /// A host that counts frame requests and keeps the last wake time, at a clock the test sets.
    #[derive(Default)]
    struct Clock {
        caps: HostCaps,
        diag: HostDiag,
        now: f64,
        frames: u32,
        wake: Option<f64>,
    }

    impl HostCtl for Clock {
        fn caps(&self) -> &HostCaps {
            &self.caps
        }
        fn diag(&self) -> &HostDiag {
            &self.diag
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn gpu(&mut self) -> Result<Gpu, HostError> {
            Err(HostError::Gpu("none in tests".into()))
        }
        fn open(&mut self, _: &OverlaySpec) -> Result<SurfaceId, HostError> {
            Ok(7)
        }
        fn update(&mut self, _: SurfaceId, _: SurfaceChange) -> Result<(), HostError> {
            Ok(())
        }
        fn close(&mut self, _: SurfaceId) {}
        fn request_frame(&mut self, _: SurfaceId) {
            self.frames += 1;
        }
        fn wake_at(&mut self, t: Option<f64>) {
            self.wake = t;
        }
        fn waker(&self) -> Waker {
            Waker::new(|| {})
        }
        fn exit(&mut self) {}
    }

    fn live(pace: Option<f64>) -> Live<Counter> {
        let app = Counter { n: 0, pace: Cell::new(pace), views: Cell::new(0) };
        let mut live = Live::new(app, OverlaySpec::default(), crate::Options::default(), None);
        let driver = Driver::new(live.pending.take().unwrap().0, vo_scene::Scene::new(), Tokens::default(), 0.0);
        live.driver = Some(driver);
        live.driver().dirty = false;
        live.last_frame = Some(1.0);
        live
    }

    #[test]
    fn a_paced_change_waits_for_its_slot() {
        let mut live = live(Some(30.0));
        let mut host = Clock { now: 1.01, ..Clock::default() };
        live.driver().dispatch((), 1.01);
        assert!(live.driver().dirty, "the view changed");
        live.after_change(&mut host);
        assert_eq!(host.frames, 0, "no frame before the slot");
        let slot = 1.0 + 1.0 / 30.0 - 1.0 / 120.0;
        assert!(host.wake.is_some_and(|w| (w - slot).abs() < 1e-9), "wakes at the slot: {:?}", host.wake);
        // The wake: now the frame comes, and it shows the newest state.
        host.now = slot;
        live.after_change(&mut host);
        assert_eq!(host.frames, 1);
    }

    /// A live overlay as `start` makes it: views wait for the frame under a pace.
    fn lazy(pace: Option<f64>) -> Live<Counter> {
        let mut live = live(pace);
        live.driver().lazy = true;
        live.driver().app.views.set(0);
        live
    }

    #[test]
    fn paced_messages_build_one_view_per_frame() {
        let mut live = lazy(Some(30.0));
        let mut host = Clock { now: 1.01, ..Clock::default() };
        // An audio batch as two messages, then the next batch 20 ms later: no view yet.
        live.driver().dispatch((), 1.01);
        live.driver().dispatch((), 1.01);
        live.after_change(&mut host);
        live.driver().dispatch((), 1.02);
        host.now = 1.02;
        live.after_change(&mut host);
        assert_eq!(live.driver().app.views.get(), 0, "no view before the frame");
        assert_eq!(host.frames, 0, "no frame before the slot");
        let slot = 1.0 + 1.0 / 30.0 - 1.0 / 120.0;
        assert!(host.wake.is_some_and(|w| (w - slot).abs() < 1e-9), "wakes at the slot: {:?}", host.wake);
        host.now = slot;
        live.after_change(&mut host);
        assert_eq!(host.frames, 1);
        // The frame builds the view once, with the newest state.
        live.sample(&mut host, 7, 1.04);
        assert_eq!(live.driver().app.views.get(), 1);
        assert!(!live.driver().stale());
        let mut list = DisplayList::default();
        live.driver().scene.sample_into(1.04, &mut list);
        assert_eq!(list, live.list, "the frame shows the scene as built");
        live.sample(&mut host, 7, 1.05);
        assert_eq!(live.driver().app.views.get(), 1, "nothing new, no view");
    }

    #[test]
    fn unpaced_and_exiting_messages_build_their_view_at_once() {
        let mut live = lazy(None);
        live.driver().dispatch((), 1.01);
        assert_eq!(live.driver().app.views.get(), 1);
        // A message that ends the pace builds at once, with what waited before it.
        let mut live = lazy(Some(30.0));
        live.driver().dispatch((), 1.01);
        live.driver().app.pace.set(None);
        live.driver().dispatch((), 1.02);
        assert_eq!(live.driver().app.views.get(), 1);
        assert!(!live.driver().stale());
        assert_eq!(live.driver().app.n, 2);
    }

    #[test]
    fn without_a_pace_a_change_draws_at_once() {
        let mut live = live(None);
        let mut host = Clock { now: 1.01, ..Clock::default() };
        live.driver().dispatch((), 1.01);
        live.after_change(&mut host);
        assert_eq!(host.frames, 1);
        // The pace ends with the quiet: the next change draws at once too.
        let mut live = self::live(Some(30.0));
        live.driver().app.pace.set(None);
        live.driver().dispatch((), 1.01);
        live.after_change(&mut host);
        assert_eq!(host.frames, 2);
    }

    #[test]
    fn a_frame_the_host_asked_for_is_not_paced() {
        let mut live = live(Some(30.0));
        let mut host = Clock { now: 1.01, ..Clock::default() };
        live.driver().dirty = true;
        live.host_asked = true;
        live.after_change(&mut host);
        assert_eq!(host.frames, 1);
    }

    /// RD-14: a paced frame that would show what is on screen is kept, not drawn.
    #[test]
    fn a_paced_frame_with_nothing_new_is_kept() {
        let mut live = live(Some(30.0));
        let mut host = Clock { now: 1.01, ..Clock::default() };
        live.surface = 7;
        live.first_gpu_frame = Some(0);
        live.frames = 1;
        live.sample(&mut host, 7, 1.0);
        assert!(!live.prepare_frame(&mut host, 7, 1.04), "same list: kept");
        assert_eq!((live.frames, live.last_frame), (1, Some(1.04)), "paced as a frame, not counted as one");
        // A change draws, and the frame uses the list sampled for it. (The counter's view
        // draws nothing, so a new surface size is the change.)
        live.driver().scene.set_viewport(1.05, vo_scene::geom::Size::new(320.0, 200.0), 1.0);
        assert!(live.prepare_frame(&mut host, 7, 1.08));
        assert_eq!(live.prepared, Some(1.08));
        assert_ne!(live.list, live.shown, "the list on screen is kept apart");
        // Not under a pace, or when the host asked: always drawn.
        live.after_frame(&mut host, 7, 1.08, true);
        live.host_asked = true;
        assert!(live.prepare_frame(&mut host, 7, 1.12));
        live.host_asked = false;
        live.driver().app.pace.set(None);
        assert!(live.prepare_frame(&mut host, 7, 1.12));
    }

    #[test]
    fn a_moving_scene_under_a_pace_asks_a_period_later() {
        assert_eq!(pace_period(Some(30.0)), Some(1.0 / 30.0));
        assert_eq!(pace_period(Some(0.0)), None);
        assert_eq!(pace_period(None), None);
        assert_eq!(pace_slot(None, Some(30.0)), None);
    }
}

#[cfg(test)]
mod margin_tests {
    use vo_host::{Edges, Gpu, HostCaps, HostDiag, Waker};
    use vo_scene::element::Element;
    use vo_scene::geom::{Color, Size};
    use vo_scene::Insets;

    use super::*;
    use crate::app::Cx;

    const BASE: Insets = Insets::new(4.0, 4.0, 4.0, 4.0);

    fn follow(placement: Placement) -> MarginFollow {
        MarginFollow::new(BASE, placement).expect("follows")
    }

    #[test]
    fn grows_at_once_with_room_to_spare() {
        let mut f = follow(Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [0; 4] });
        assert_eq!(f.step(Insets::uniform(3.0), 0.0, true), None, "the first view's margin is enough");
        // 5 more than the base on the left: at least 32 more.
        assert_eq!(f.step(Insets::new(9.0, 0.0, 0.0, 0.0), 0.1, true), Some(Insets::new(36.0, 4.0, 4.0, 4.0)));
        // A reach that creeps up inside that room: no resize.
        for (i, need) in [12.0, 20.0, 30.0, 36.0].into_iter().enumerate() {
            assert_eq!(f.step(Insets::new(need, 0.0, 0.0, 0.0), 0.2 + 0.1 * i as f64, true), None);
        }
        // Past it: the extra at least doubles (32 to 64), or takes what is needed if more.
        assert_eq!(f.step(Insets::new(41.0, 0.0, 0.0, 0.0), 1.0, true), Some(Insets::new(68.0, 4.0, 4.0, 4.0)));
        assert_eq!(f.step(Insets::new(300.0, 0.0, 0.0, 0.0), 1.1, true), Some(Insets::new(300.0, 4.0, 4.0, 4.0)));
        // A ramp from 8 to 200 over the base resizes four times (32, 64, 128, 256), not 25.
        let mut g = follow(Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [0; 4] });
        let n = (1..=25)
            .filter(|i| g.step(Insets::uniform(4.0 + 8.0 * *i as f32), *i as f64 * 0.02, true).is_some())
            .count();
        assert_eq!(n, 4);
    }

    #[test]
    fn shrinks_only_to_the_first_view_after_the_hold() {
        let mut f = follow(Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [0; 4] });
        let big = f.step(Insets::uniform(60.0), 1.0, true).expect("grows");
        assert_eq!(big, Insets::uniform(60.0));
        // Less, but still more than the first view's: no shrink to a middle size.
        assert_eq!(f.step(Insets::uniform(20.0), 1.5, true), None);
        assert_eq!(f.wake(), None);
        // The effect is gone: the first view's margin waits for the hold.
        assert_eq!(f.step(Insets::ZERO, 2.0, true), None);
        assert_eq!(f.wake(), Some(2.0 + MARGIN_HOLD));
        assert_eq!(f.step(Insets::ZERO, 2.0 + MARGIN_HOLD * 0.5, true), None);
        // Back up within the hold: the wait starts again.
        assert_eq!(f.step(Insets::uniform(60.0), 2.7, true), None);
        assert_eq!(f.wake(), None);
        assert_eq!(f.step(Insets::ZERO, 3.0, true), None);
        // The app asked to exit: no shrink, the surface closes soon.
        assert_eq!(f.step(Insets::ZERO, 3.0 + MARGIN_HOLD, false), None);
        assert_eq!(f.step(Insets::ZERO, 3.1 + MARGIN_HOLD, true), None, "the wait starts again");
        assert_eq!(f.step(Insets::ZERO, 3.1 + 2.0 * MARGIN_HOLD, true), Some(BASE), "back to the first view's");
        assert_eq!(f.step(Insets::ZERO, 9.0, true), None);
    }

    #[test]
    fn never_grows_past_the_output_edge() {
        // 50 px between the panel and the output's top, 30 on the left, 100 on the right.
        let mut f = follow(Placement::Anchored { edges: Edges::BOTTOM_RIGHT, margin: [0; 4] });
        f.cap = Some(Insets::new(30.0, 50.0, 100.0, 2.0));
        let m = f.step(Insets::uniform(300.0), 0.0, true).expect("grows");
        // The bottom keeps its first view's 4, more than the 2 of room there.
        assert_eq!(m, Insets::new(30.0, 50.0, 100.0, 4.0));
        assert_eq!(f.step(Insets::uniform(400.0), 0.1, true), None, "no room left: no resize");
        // On a centred axis both sides take the smaller room.
        let mut c = follow(Placement::LineAt { fraction: 0.5, y: 0.0 });
        c.cap = Some(Insets::new(30.0, 500.0, 100.0, 500.0));
        assert_eq!(c.step(Insets::uniform(60.0), 0.0, true), Some(Insets::new(30.0, 60.0, 30.0, 60.0)));
    }

    #[test]
    fn a_centred_axis_grows_on_both_sides() {
        // `LineAt` centres across: left and right grow together, top and bottom on their own.
        let mut f = follow(Placement::LineAt { fraction: 0.5, y: 40.0 });
        let m = f.step(Insets::new(0.0, 0.0, 20.0, 50.0), 0.0, true).expect("grows");
        assert_eq!(m, Insets::new(36.0, 4.0, 36.0, 52.0));
        assert!(MarginFollow::new(BASE, Placement::Fullscreen).is_none(), "a full-screen surface has no margin");
    }

    /// A card that glows with radius 40 between the messages `true` and `false`.
    struct Glow(bool);

    impl App for Glow {
        type Msg = bool;
        fn update(&mut self, on: bool, _: &mut Cx<bool>) {
            self.0 = on;
        }
        fn view(&self, _: &Tokens) -> Element<bool> {
            let card = Element::panel("card").size(200.0, 60.0).fill(Color::WHITE);
            if self.0 {
                card.glow(Color::WHITE, 40.0, 1.0)
            } else {
                card
            }
        }
    }

    /// A host with a clock the test sets, that records size changes and draws on the CPU.
    #[derive(Default)]
    struct Host {
        caps: HostCaps,
        diag: HostDiag,
        now: f64,
        sizes: Vec<Size>,
        wake: Option<f64>,
    }

    impl HostCtl for Host {
        fn caps(&self) -> &HostCaps {
            &self.caps
        }
        fn diag(&self) -> &HostDiag {
            &self.diag
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn gpu(&mut self) -> Result<Gpu, HostError> {
            Err(HostError::Gpu("none in tests".into()))
        }
        fn gpu_in_background(&mut self) -> bool {
            true
        }
        fn open(&mut self, spec: &OverlaySpec) -> Result<SurfaceId, HostError> {
            self.sizes.push(spec.size);
            Ok(7)
        }
        fn update(&mut self, _: SurfaceId, change: SurfaceChange) -> Result<(), HostError> {
            if let SurfaceChange::Size(s) = change {
                self.sizes.push(s);
            }
            Ok(())
        }
        fn close(&mut self, _: SurfaceId) {}
        fn request_frame(&mut self, _: SurfaceId) {}
        fn wake_at(&mut self, t: Option<f64>) {
            self.wake = t;
        }
        fn waker(&self) -> Waker {
            Waker::new(|| {})
        }
        fn exit(&mut self) {}
    }

    /// Runs the glow app at 60 fps for 5 s, the glow on from 1 s to 2 s, and checks that no
    /// frame clips. Returns the surface size at each frame.
    fn run(margin: SurfaceMargin) -> Vec<(f64, Size)> {
        let spec = OverlaySpec {
            size: Size::new(200.0, 60.0),
            placement: Placement::LineAt { fraction: 0.8, y: 30.0 },
            ..OverlaySpec::default()
        };
        let options = crate::Options { cpu_first_frame: true, margin, ..crate::Options::default() };
        let mut live = Live::new(Glow(false), spec, options, None);
        let mut host = Host::default();
        live.start(&mut host);
        assert!(live.error.is_none());
        let mut out = Vec::new();
        let mut size = live.spec.size;
        let mut configured = host.sizes.len();
        for i in 0..300 {
            let t = i as f64 / 60.0;
            host.now = t;
            for (at, on) in [(1.0, true), (2.0, false)] {
                if (t - at).abs() < 1e-9 {
                    live.driver().dispatch(on, t);
                    live.after_change(&mut host);
                }
            }
            if host.wake.is_some_and(|w| w <= t) {
                live.event(&mut host, HostEvent::Timer { t });
            }
            // The compositor confirms each new size before the next frame.
            if host.sizes.len() > configured {
                configured = host.sizes.len();
                size = *host.sizes.last().unwrap();
                live.event(&mut host, HostEvent::Configured { id: 7, t, size, scale: 1.0 });
            }
            live.sample(&mut host, 7, t);
            let clipped = live.driver().scene.clipped(t, size);
            assert!(clipped.is_zero(), "clipped {clipped:?} at {t} on {size:?}");
            live.after_frame(&mut host, 7, t, true);
            out.push((t, size));
        }
        out
    }

    #[test]
    fn a_glow_that_comes_later_grows_the_surface_while_it_shows() {
        let sizes = run(SurfaceMargin::Auto);
        let first = sizes[0].1;
        assert_eq!(first, Size::new(200.0, 60.0), "no effect in the first view: no margin");
        let at = |t: f64| sizes.iter().find(|(ft, _)| *ft >= t - 1e-9).unwrap().1;
        assert_eq!(at(0.9), first);
        let grown = at(1.5);
        let reach = vo_scene::reach::glow_reach(40.0, 1.0);
        assert!(grown.w >= 200.0 + 2.0 * reach && grown.h >= 60.0 + 2.0 * reach, "{grown:?}");
        // Back to the first view's size once the glow has faded and the hold has passed.
        let back = sizes.iter().find(|(t, s)| *t > 2.0 && *s == first).map(|(t, _)| *t);
        assert!(back.is_some_and(|t| t < 4.0), "shrinks back: {back:?}");
        assert!(sizes.iter().filter(|(t, _)| *t >= 4.0).all(|(_, s)| *s == first));
        // Only a few resizes: grow once, shrink once (or a step or two while it fades).
        let changes = sizes.windows(2).filter(|w| w[0].1 != w[1].1).count();
        assert_eq!(changes, 2, "grows once, shrinks once");
    }

    #[test]
    fn a_fixed_margin_does_not_follow() {
        let sizes = run(SurfaceMargin::Fixed(Insets::uniform(400.0)));
        assert!(sizes.iter().all(|(_, s)| *s == sizes[0].1));
    }
}

#[cfg(test)]
mod key_tests {
    use vo_host::{FocusProxySpec, Gpu, HostCaps, HostDiag, KeyEvent, KeyState, LogicalKey, Mods, RepeatInfo, Waker};

    use super::*;
    use crate::app::Cx;

    /// Takes keyboard focus on its message, logs focus changes and keys, and gives focus back
    /// on Escape.
    #[derive(Default)]
    struct Editor {
        log: Vec<String>,
    }

    impl App for Editor {
        type Msg = ();
        fn update(&mut self, _: (), cx: &mut Cx<()>) {
            cx.take_keyboard_focus(FocusProxySpec { app_id: "test-edit".into(), output: None });
        }
        fn view(&self, _: &Tokens) -> vo_scene::element::Element<()> {
            vo_scene::element::Element::column("root")
        }
        fn key(&mut self, ev: &KeyEvent, cx: &mut Cx<()>) -> bool {
            self.log.push(format!("{:?} {:.2}", ev.state, ev.t));
            if ev.key == LogicalKey::Named(vo_host::NamedKey::Escape) {
                cx.release_keyboard_focus();
            }
            true
        }
        fn keyboard_focus(&mut self, gained: bool, requested: bool, _: &mut Cx<()>) {
            self.log.push(format!("focus {gained} {requested}"));
        }
    }

    /// A host with focus proxies, at a clock the test sets.
    #[derive(Default)]
    struct Proxies {
        caps: HostCaps,
        diag: HostDiag,
        now: f64,
        wake: Option<f64>,
        open: Vec<SurfaceId>,
        closed: Vec<SurfaceId>,
    }

    impl HostCtl for Proxies {
        fn caps(&self) -> &HostCaps {
            &self.caps
        }
        fn diag(&self) -> &HostDiag {
            &self.diag
        }
        fn now(&self) -> f64 {
            self.now
        }
        fn gpu(&mut self) -> Result<Gpu, HostError> {
            Err(HostError::Gpu("none in tests".into()))
        }
        fn open(&mut self, _: &OverlaySpec) -> Result<SurfaceId, HostError> {
            Ok(7)
        }
        fn update(&mut self, _: SurfaceId, _: SurfaceChange) -> Result<(), HostError> {
            Ok(())
        }
        fn close(&mut self, _: SurfaceId) {}
        fn open_focus_proxy(&mut self, _: &FocusProxySpec) -> Result<SurfaceId, HostError> {
            self.open.push(20 + self.open.len() as SurfaceId);
            Ok(*self.open.last().unwrap())
        }
        fn close_focus_proxy(&mut self, id: SurfaceId) {
            self.closed.push(id);
        }
        fn request_frame(&mut self, _: SurfaceId) {}
        fn wake_at(&mut self, t: Option<f64>) {
            self.wake = t;
        }
        fn waker(&self) -> Waker {
            Waker::new(|| {})
        }
        fn exit(&mut self) {}
    }

    fn key(code: u32, state: KeyState, t: f64) -> HostEvent {
        let key = if code == 1 { LogicalKey::Named(vo_host::NamedKey::Escape) } else { LogicalKey::Char('a') };
        HostEvent::Key(KeyEvent { surface: 20, t, state, key, code, text: None, mods: Mods::NONE })
    }

    #[test]
    fn focus_proxy_keys_and_repeat_through_the_live_loop() {
        let mut live = Live::new(Editor::default(), OverlaySpec::default(), crate::Options::default(), None);
        let driver = Driver::new(live.pending.take().unwrap().0, vo_scene::Scene::new(), Tokens::default(), 0.0);
        live.driver = Some(driver);
        let mut host = Proxies::default();
        live.event(&mut host, HostEvent::KeyRepeatInfo { t: 0.0, info: RepeatInfo { rate_hz: 10.0, delay_s: 0.5 } });

        // The app asks for focus: the proxy opens.
        live.driver().dispatch((), 1.0);
        live.after_change(&mut host);
        assert_eq!(host.open, [20]);
        live.event(&mut host, HostEvent::KeyboardFocus { id: 20, t: 1.0, gained: true, requested: false });

        // A held key: the host is woken at the repeat times.
        live.event(&mut host, key(30, KeyState::Down, 1.1));
        assert_eq!(host.wake, Some(1.6));
        host.now = 1.6;
        live.event(&mut host, HostEvent::Timer { t: 1.6 });
        assert!(host.wake.is_some_and(|w| (w - 1.7).abs() < 1e-9), "{:?}", host.wake);
        live.event(&mut host, key(30, KeyState::Up, 1.65));
        assert_eq!(host.wake, None);

        // Escape gives focus back: the proxy closes.
        live.event(&mut host, key(1, KeyState::Down, 2.0));
        assert_eq!(host.closed, [20]);
        live.event(&mut host, HostEvent::KeyboardFocus { id: 20, t: 2.0, gained: false, requested: true });

        // Focus taken away from a new proxy: it is closed too, and the app hears of it.
        live.driver().dispatch((), 3.0);
        live.after_change(&mut host);
        assert_eq!(host.open, [20, 21]);
        live.event(&mut host, HostEvent::KeyboardFocus { id: 21, t: 3.0, gained: true, requested: false });
        live.event(&mut host, HostEvent::KeyboardFocus { id: 21, t: 3.5, gained: false, requested: false });
        assert_eq!(host.closed, [20, 21]);
        assert_eq!(
            live.driver().app.log,
            [
                "focus true false",
                "Down 1.10",
                "Repeat 1.60",
                "Up 1.65",
                "Down 2.00",
                "focus false true",
                "focus true false",
                "focus false false"
            ]
        );
    }
}
