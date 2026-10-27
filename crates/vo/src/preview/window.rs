//! The preview window: a normal resizable window (`SurfaceKind::Window`) with the stage, the
//! timeline bar and the debug overlays. It is a `HostApp` of its own; the app under preview
//! never sees the host. Its stage comes from a [`Source`]: an app through a `Player`, so any
//! time can be shown in any order, or a custom draw function.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use vo_host::{
    Frame, HostApp, HostCtl, HostError, HostEvent, InputMode, OverlaySpec, PointerKind, Size, SurfaceChange, SurfaceId,
    SurfaceKind,
};
use vo_render::offscreen::{write_png, FORMAT};
use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;
use vo_scene::geom::Rect;
use vo_scene::input::Button;
use vo_scene::{FontRegistry, Motion, Scene};

use super::chrome::{self, ChromeIn, Overlay, UiMsg, BAR_H, STAGE_MARGIN};
use super::composite::{read_back, Compositor, PxRect};
use super::control::{self, Cmd, Request};
use super::reload::{parse_tokens, Watch};
use super::source::{OverlayAsk, Source};
use super::State;

/// Frames kept for the frame time graph.
const GRAPH_FRAMES: usize = 120;
/// How often watched files and the control socket are checked when nothing else wakes the loop.
const POLL_SECS: f64 = 0.25;

pub(crate) struct Setup {
    pub fps: f64,
    pub markers: Vec<(String, f64)>,
    pub state: State,
    pub control: Option<PathBuf>,
    pub state_path: Option<PathBuf>,
    pub tokens: Option<PathBuf>,
    pub shaders: Option<PathBuf>,
    pub app_id: String,
}

pub(crate) struct Window {
    p: Box<dyn Source>,
    setup: Setup,
    st: State,
    duration: f64,
    events: Vec<f64>,
    fonts: FontRegistry,
    font: u32,
    mono: u32,
    ui: Scene<UiMsg>,
    ui_t: f64,
    renderer: Option<Renderer>,
    comp: Option<Compositor>,
    stage_tex: Option<Target>,
    chrome_tex: Option<Target>,
    surface: SurfaceId,
    size: Size,
    scale: f32,
    /// Host time of the last frame while playing.
    last_play_t: Option<f64>,
    last_frame_t: Option<f64>,
    drag: bool,
    graph: VecDeque<(f32, f32)>,
    ui_list: DisplayList,
    status: String,
    control: Option<mpsc::Receiver<Request>>,
    tokens_watch: Watch,
    shader_watch: Watch,
    shots: Vec<(PathBuf, mpsc::Sender<String>)>,
    pub(crate) error: Option<HostError>,
}

impl Window {
    pub(crate) fn new(p: Box<dyn Source>, setup: Setup) -> Window {
        let fonts = p.fonts();
        let font = add_font(&fonts, "Noto Sans:medium");
        let mono = add_font(&fonts, "monospace");
        let duration = p.duration();
        let events = p.event_times();
        let st = setup.state.clone();
        let mut tokens_watch = Watch::default();
        if let Some(t) = &setup.tokens {
            tokens_watch.add(t, None);
        }
        let mut shader_watch = Watch::default();
        if let Some(d) = &setup.shaders {
            shader_watch.add(d, Some("wgsl"));
        }
        let ui = Scene::new()
            .with_default_motion(Motion::Snap)
            .with_retention(1.0)
            .with_text(Box::new(vo_scene::text::ShapedLayout::new(fonts.clone())));
        Window {
            p,
            setup,
            size: Size::new(st.size.0, st.size.1),
            st,
            duration,
            events,
            fonts,
            font,
            mono,
            ui,
            ui_t: 0.0,
            renderer: None,
            comp: None,
            stage_tex: None,
            chrome_tex: None,
            surface: 0,
            scale: 1.0,
            last_play_t: None,
            last_frame_t: None,
            drag: false,
            graph: VecDeque::new(),
            ui_list: DisplayList::default(),
            status: String::new(),
            control: None,
            tokens_watch,
            shader_watch,
            shots: Vec::new(),
            error: None,
        }
    }

    fn frame_step(&self) -> f64 {
        1.0 / self.setup.fps
    }

    /// Goes to `t`, on the frame grid when `snap`, and pauses.
    fn go(&mut self, t: f64, snap: bool) {
        let fps = self.setup.fps;
        let t = if snap { (t * fps).round() / fps } else { t };
        self.st.t = t.clamp(0.0, self.duration);
        self.st.playing = false;
        self.last_play_t = None;
    }

    fn ui_msg(&mut self, msg: UiMsg) {
        let step = self.frame_step();
        let frame = (self.st.t / step).round();
        match msg {
            UiMsg::Start => self.go(if self.st.looping { self.st.range.0 } else { 0.0 }, true),
            UiMsg::End => self.go(if self.st.looping { self.st.range.1 } else { self.duration }, true),
            UiMsg::StepBack => self.go((frame - 1.0) * step, true),
            UiMsg::Step => self.go((frame + 1.0) * step, true),
            UiMsg::PlayPause => self.set_playing(!self.st.playing),
            UiMsg::Speed(s) => self.st.speed = s,
            UiMsg::Loop => {
                self.st.looping = !self.st.looping;
                if self.st.range.1 <= self.st.range.0 {
                    self.st.range = (0.0, self.duration);
                }
            }
            UiMsg::LoopIn => {
                self.st.range.0 = self.st.t;
                self.st.range.1 = self.st.range.1.max(self.st.t + step);
                self.st.looping = true;
            }
            UiMsg::LoopOut => {
                self.st.range.1 = self.st.t;
                self.st.range.0 = self.st.range.0.min(self.st.t - step).max(0.0);
                self.st.looping = true;
            }
            UiMsg::Marker(i) => {
                if let Some((_, t)) = self.setup.markers.get(i) {
                    self.go(*t, false);
                }
            }
            UiMsg::Toggle(o) => {
                let on = !self.st.overlays.contains(&o);
                self.set_overlay(o, on);
            }
            UiMsg::OneToOne => self.st.one_to_one = !self.st.one_to_one,
        }
    }

    fn set_playing(&mut self, on: bool) {
        if on && !self.st.playing {
            let end = if self.st.looping { self.st.range.1 } else { self.duration };
            if self.st.t >= end - 1e-9 {
                self.st.t = if self.st.looping { self.st.range.0 } else { 0.0 };
            }
        }
        self.st.playing = on;
        self.last_play_t = None;
    }

    fn set_overlay(&mut self, o: Overlay, on: bool) {
        self.st.overlays.retain(|x| *x != o);
        if on {
            self.st.overlays.push(o);
        }
    }

    fn command(&mut self, host: &mut dyn HostCtl, cmd: Cmd, reply: mpsc::Sender<String>) {
        let ok = |reply: &mpsc::Sender<String>| {
            let _ = reply.send("ok".into());
        };
        match cmd {
            Cmd::Seek(t) => {
                self.go(t, false);
                ok(&reply);
            }
            Cmd::Play => {
                self.set_playing(true);
                ok(&reply);
            }
            Cmd::Pause => {
                self.set_playing(false);
                ok(&reply);
            }
            Cmd::Toggle => {
                self.set_playing(!self.st.playing);
                ok(&reply);
            }
            Cmd::Step(n) => {
                let step = self.frame_step();
                self.go(((self.st.t / step).round() + n as f64) * step, true);
                ok(&reply);
            }
            Cmd::Speed(s) => {
                self.st.speed = s.clamp(0.05, 8.0);
                ok(&reply);
            }
            Cmd::Marker(name) => match self.setup.markers.iter().find(|(n, _)| *n == name) {
                Some((_, t)) => {
                    let t = *t;
                    self.go(t, false);
                    ok(&reply);
                }
                None => {
                    let names: Vec<&str> = self.setup.markers.iter().map(|(n, _)| n.as_str()).collect();
                    let _ = reply.send(format!("error: no marker '{name}' (have {names:?})"));
                }
            },
            Cmd::Loop(r) => {
                match r {
                    Some((a, b)) => {
                        self.st.range = (a.min(b).max(0.0), a.max(b).min(self.duration));
                        self.st.looping = true;
                    }
                    None => self.st.looping = false,
                }
                ok(&reply);
            }
            Cmd::Overlay(o, on) => {
                self.set_overlay(o, on);
                ok(&reply);
            }
            Cmd::OneToOne(on) => {
                self.st.one_to_one = on;
                ok(&reply);
            }
            Cmd::Size(w, h) => {
                let r = host.update(self.surface, SurfaceChange::Size(Size::new(w, h)));
                let _ = reply.send(r.map_or_else(|e| format!("error: {e}"), |_| "ok".into()));
            }
            Cmd::Shot(path) => self.shots.push((path, reply)),
            Cmd::State => {
                let _ = reply.send(serde_json::to_string(&self.st).unwrap_or_default());
            }
            Cmd::Quit => {
                self.save_state();
                ok(&reply);
                host.exit();
            }
        }
        host.request_frame(self.surface);
    }

    fn save_state(&self) {
        if let Some(path) = &self.setup.state_path {
            if let Ok(json) = serde_json::to_string_pretty(&self.st) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// Checks the control socket and watched files.
    fn poll(&mut self, host: &mut dyn HostCtl) {
        let reqs: Vec<Request> = self.control.as_ref().map(|c| c.try_iter().collect()).unwrap_or_default();
        for (cmd, reply) in reqs {
            self.command(host, cmd, reply);
        }
        if !self.tokens_watch.changed().is_empty() {
            self.reload_tokens();
            host.request_frame(self.surface);
        }
        let shaders = self.shader_watch.changed();
        if !shaders.is_empty() {
            self.reload_shaders(&shaders);
            host.request_frame(self.surface);
        }
    }

    fn reload_tokens(&mut self) {
        let Some(path) = self.setup.tokens.clone() else { return };
        let started = Instant::now();
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| parse_tokens(&s)) {
            Ok(tokens) => match self.p.set_tokens(tokens) {
                Ok(()) => {
                    self.p.seek(self.st.t);
                    self.events = self.p.event_times();
                    self.status = format!("tokens reloaded, replayed to {:.3} s in {:.1} ms", self.st.t, ms(started));
                }
                Err(e) => self.status = format!("tokens: {e}"),
            },
            Err(e) => self.status = format!("tokens: {e}"),
        }
        eprintln!("vo-preview: {}", self.status);
    }

    fn reload_shaders(&mut self, changed: &[PathBuf]) {
        let Some(r) = self.renderer.as_mut() else { return };
        let started = Instant::now();
        let names: Vec<String> =
            changed.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        self.status = match r.reload_shaders() {
            Ok(n) => format!("{} reloaded: {n} shader modules in {:.1} ms", names.join(", "), ms(started)),
            Err(e) => {
                eprintln!("{e}");
                format!(
                    "shader error, old pipelines kept: {}",
                    e.lines().find(|l| l.contains("error:")).or(e.lines().next()).unwrap_or("").trim()
                )
            }
        };
        eprintln!("vo-preview: {}", self.status);
    }

    /// The stage's scale and its rect in window units.
    fn stage_geometry(&self) -> (f32, Rect) {
        let sc = self.p.size();
        let area_w = (self.size.w - 2.0 * STAGE_MARGIN).max(1.0);
        let area_h = (self.size.h - BAR_H - 2.0 * STAGE_MARGIN).max(1.0);
        let fit = if self.st.one_to_one { 1.0 } else { (area_w / sc.w).min(area_h / sc.h).min(2.0) };
        let (w, h) = (sc.w * fit, sc.h * fit);
        let x = STAGE_MARGIN + ((area_w - w) * 0.5).max(0.0);
        let y = STAGE_MARGIN + ((area_h - h) * 0.5).max(0.0);
        // Whole physical pixels, so the stage texture lands 1:1.
        let s = self.scale;
        (fit, Rect::new((x * s).round() / s, (y * s).round() / s, w, h))
    }

    fn stats(&self, stage_px: [u32; 2], fit: f32) -> Vec<String> {
        let mut out = self.p.stats();
        out.push(format!("stage {}x{} px at {:.3}", stage_px[0], stage_px[1], fit * self.scale));
        out
    }

    fn draw(&mut self, frame: &Frame<'_>) -> Result<(), String> {
        let started = Instant::now();
        let interval = self.last_frame_t.map_or(0.0, |p| (frame.t - p) * 1e3) as f32;
        self.last_frame_t = Some(frame.t);
        if self.st.playing && !self.drag {
            if let Some(prev) = self.last_play_t {
                self.st.t += (frame.t - prev).max(0.0) * self.st.speed;
            }
            self.last_play_t = Some(frame.t);
            let (start, end) = if self.st.looping { self.st.range } else { (0.0, self.duration) };
            if self.st.t > end {
                if self.st.looping {
                    self.st.t = start + (self.st.t - end) % (end - start).max(self.frame_step());
                } else {
                    self.st.t = end;
                    self.st.playing = false;
                    self.last_play_t = None;
                }
            }
        }
        let t = self.st.t;
        // The overlays read the scene at `t`, before the stage is drawn.
        self.p.seek(t);

        let (fit, stage) = self.stage_geometry();
        let s = frame.scale;
        let stage_px = [((stage.w * s).round() as u32).max(1), ((stage.h * s).round() as u32).max(1)];
        let (boxes, springs) = if self.st.overlays.iter().any(|o| matches!(o, Overlay::Bounds | Overlay::Springs)) {
            self.p.overlays(t, &OverlayAsk { springs: self.st.overlays.contains(&Overlay::Springs), fit, stage })
        } else {
            (Vec::new(), Vec::new())
        };
        let stats = if self.st.overlays.contains(&Overlay::Stats) { self.stats(stage_px, fit) } else { Vec::new() };
        let graph: Vec<(f32, f32)> = self.graph.iter().copied().collect();
        let view = chrome::view(&ChromeIn {
            size: (self.size.w, self.size.h),
            font: self.font,
            mono: self.mono,
            t,
            duration: self.duration,
            fps: self.setup.fps,
            playing: self.st.playing,
            speed: self.st.speed,
            looping: self.st.looping,
            range: self.st.range,
            one_to_one: self.st.one_to_one,
            fit,
            markers: &self.setup.markers,
            events: &self.events,
            overlays: &self.st.overlays,
            boxes: &boxes,
            springs: &springs,
            graph: &graph,
            stats: &stats,
            status: &self.status,
        });
        self.ui_t = self.ui_t.max(frame.t);
        self.ui.apply(view, self.ui_t);
        self.ui.sample_into(self.ui_t, &mut self.ui_list);

        let r = self.renderer.as_mut().ok_or("no renderer")?;
        let device = r.device().clone();
        let queue = r.queue().clone();
        let fresh = |t: &Option<Target>, size: [u32; 2]| {
            t.as_ref().is_none_or(|t| t.width() != size[0] || t.height() != size[1])
        };
        if fresh(&self.stage_tex, stage_px) {
            self.stage_tex = Some(Target::new(&device, stage_px[0], stage_px[1], FORMAT));
        }
        if fresh(&self.chrome_tex, frame.size) {
            self.chrome_tex = Some(Target::new(&device, frame.size[0], frame.size[1], FORMAT));
        }
        let stage_tex = self.stage_tex.as_mut().unwrap();
        let chrome_tex = self.chrome_tex.as_mut().unwrap();
        self.p.draw(t, fit * s, r, stage_tex);
        r.render_at(&self.ui_list, s, chrome_tex);
        let rect = PxRect { x: stage.x * s, y: stage.y * s, w: stage_px[0] as f32, h: stage_px[1] as f32 };
        let comp = self.comp.as_mut().ok_or("no compositor")?;
        let mut enc = device.create_command_encoder(&Default::default());
        comp.encode(&mut enc, frame.texture, stage_tex.texture(), rect, chrome_tex.texture());
        queue.submit([enc.finish()]);

        for (path, reply) in std::mem::take(&mut self.shots) {
            let shot = Target::new(&device, frame.size[0], frame.size[1], FORMAT);
            let mut enc = device.create_command_encoder(&Default::default());
            comp.encode(&mut enc, shot.texture(), stage_tex.texture(), rect, chrome_tex.texture());
            queue.submit([enc.finish()]);
            let rgba = read_back(&device, &queue, shot.texture());
            let msg = match write_png(&path, frame.size[0], frame.size[1], &rgba) {
                Ok(()) => format!("ok {}", path.display()),
                Err(e) => format!("error: {e}"),
            };
            let _ = reply.send(msg);
        }

        self.graph.push_back((interval, ms(started) as f32));
        while self.graph.len() > GRAPH_FRAMES {
            self.graph.pop_front();
        }
        Ok(())
    }
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

/// Adds the font fontconfig picks for `family`, or returns font 0 when there is none.
fn add_font(fonts: &FontRegistry, family: &str) -> u32 {
    fonts.add_system(family).unwrap_or_else(|e| {
        eprintln!("vo-preview: {e}; the chrome uses font 0");
        0
    })
}

impl HostApp for Window {
    fn start(&mut self, host: &mut dyn HostCtl) {
        let spec = OverlaySpec {
            size: self.size,
            input: InputMode::Whole,
            kind: SurfaceKind::Window,
            app_id: self.setup.app_id.clone(),
            ..OverlaySpec::default()
        };
        let opened = host.open(&spec).and_then(|id| Ok((id, host.gpu()?)));
        let (id, gpu) = match opened {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                host.exit();
                return;
            }
        };
        self.surface = id;
        let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, self.fonts.clone());
        self.p.configure(&mut r);
        self.renderer = Some(r);
        self.comp = Some(Compositor::new(&gpu.device, &gpu.queue));
        if let Some(path) = &self.setup.control {
            match control::listen(path, host.waker()) {
                Ok(rx) => self.control = Some(rx),
                Err(e) => eprintln!("vo-preview: no control socket at {}: {e}", path.display()),
            }
        }
        eprintln!(
            "vo-preview: {} s at {} fps, {} events, markers {:?}",
            self.duration,
            self.setup.fps,
            self.events.len(),
            self.setup.markers.iter().map(|m| &m.0).collect::<Vec<_>>()
        );
        host.wake_at(Some(host.now() + POLL_SECS));
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Configured { id, size, scale, .. } if id == self.surface => {
                self.size = size;
                self.scale = scale;
                self.st.size = (size.w, size.h);
            }
            HostEvent::Pointer(mut ev) if ev.surface == self.surface => {
                ev.t = ev.t.max(self.ui_t);
                let track = chrome::track_rect((self.size.w, self.size.h));
                let grab = Rect::new(track.x - 6.0, track.y - 12.0, track.w + 12.0, track.h + 16.0);
                match ev.kind {
                    PointerKind::Down(Button::Primary) if grab.contains(ev.pos) => {
                        self.drag = true;
                        let t = chrome::track_time((self.size.w, self.size.h), ev.pos.x, self.duration);
                        self.go(t, false);
                    }
                    PointerKind::Motion if self.drag => {
                        let t = chrome::track_time((self.size.w, self.size.h), ev.pos.x, self.duration);
                        self.go(t, false);
                    }
                    PointerKind::Up(Button::Primary) | PointerKind::Leave if self.drag => self.drag = false,
                    _ => {
                        for msg in self.ui.pointer(&ev) {
                            self.ui_msg(msg);
                        }
                    }
                }
            }
            HostEvent::Timer { .. } => {
                self.poll(host);
                host.wake_at(Some(host.now() + POLL_SECS));
                return;
            }
            HostEvent::External { .. } => self.poll(host),
            // The host drops a frame request made inside `frame`, so playback asks again here.
            HostEvent::Presented { id, .. } if id == self.surface && (self.st.playing || !self.shots.is_empty()) => {}
            HostEvent::Closed { id, .. } if id == self.surface => {
                self.save_state();
                host.exit();
                return;
            }
            _ => return,
        }
        host.request_frame(self.surface);
    }

    fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
        if let Err(e) = self.draw(frame) {
            eprintln!("vo-preview: {e}");
        }
        if self.st.playing || !self.shots.is_empty() {
            host.request_frame(frame.id);
        }
    }
}
