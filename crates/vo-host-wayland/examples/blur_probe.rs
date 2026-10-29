//! Probe for compositor blur behind a layer-shell surface (`ext-background-effect-v1`, FX07).
//! Run it only inside a private nested KWin, never on the real session.
//!
//! It opens two layer surfaces from one client: a busy test pattern on the background layer,
//! and a translucent overlay on the overlay layer with a rounded panel drawn into it. The blur
//! region follows the panel. With `--move` the panel slides inside the overlay and the region
//! is sent again with every surface commit, which tests per-frame region updates.
//!
//!   blur_probe [--secs S] [--move] [--no-blur] [--steps N] [--backdrop-only]
//!              [--octagon C] [--turn DEG] [--checker N]
//!
//! `--backdrop-only` shows just the test pattern, as a busy desktop behind another overlay.
//!
//! EG-07: `--octagon C` makes the panel an octagon with corners chamfered C pixels, and
//! `--turn DEG` turns the panel (octagon or rounded) about its centre. Either one sends the
//! blur region `vo_scene::glass::outline_blur_rects` makes for the outline, which must keep
//! the blur inside it. `--checker N` draws a plain black and white checkerboard of N pixel
//! squares as the backdrop, for the screenshot check in `scripts/blur-outline-check.sh`.
//! When the compositor offers no blur the probe still draws and prints "no compositor blur"
//! once.
//!
//! Prints the compositor's capabilities, then one line per second with frames and region
//! updates sent.

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm, registry_handlers,
};
use std::time::Instant;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

const OW: u32 = 720;
const OH: u32 = 320;
const PW: f32 = 380.0;
const PH: f32 = 180.0;
const R: f32 = 28.0;

struct App {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    shm: Shm,
    pool: SlotPool,
    back: LayerSurface,
    back_size: (u32, u32),
    back_drawn: bool,
    over: LayerSurface,
    over_ready: bool,
    effect: Option<ExtBackgroundEffectSurfaceV1>,
    caps: Option<u32>,
    moving: bool,
    backdrop_only: bool,
    steps: u32,
    /// The panel's outline in overlay pixels when it is an octagon or turned (EG-07).
    outline: Option<Vec<vo_scene::geom::Point>>,
    checker: u32,
    start: Instant,
    frames: u64,
    region_sends: u64,
    last_report: f64,
    done: bool,
    secs: f64,
}

/// Rectangles that cover a rounded rectangle, rounded outward to whole pixels. Each corner is
/// cut into `steps` bands; a band takes the smallest inset in its rows, so no shape pixel is
/// left out.
fn rounded_rects(x: f32, y: f32, w: f32, h: f32, r: f32, steps: u32) -> Vec<(i32, i32, i32, i32)> {
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (x1, y1) = ((x + w).ceil() as i32, (y + h).ceil() as i32);
    let r = r.min(w / 2.0).min(h / 2.0);
    let ri = r.ceil() as i32;
    let mut out = Vec::new();
    if steps == 0 || ri == 0 {
        out.push((x0, y0, x1 - x0, y1 - y0));
        return out;
    }
    for i in 0..steps as i32 {
        let a = ri * i / steps as i32;
        let b = ri * (i + 1) / steps as i32;
        if b <= a {
            continue;
        }
        // Inset at the band's row nearest the straight edge: the smallest in the band.
        let dy = r - b as f32;
        let inset = (r - (r * r - dy.max(0.0) * dy.max(0.0)).sqrt()).floor() as i32;
        let rw = x1 - x0 - 2 * inset;
        out.push((x0 + inset, y0 + a, rw, b - a));
        out.push((x0 + inset, y1 - b, rw, b - a));
    }
    out.push((x0, y0 + ri, x1 - x0, (y1 - y0 - 2 * ri).max(0)));
    out
}

/// Whether `p` lies inside the polygon `pts` (even-odd).
fn inside(pts: &[vo_scene::geom::Point], x: f32, y: f32) -> bool {
    let n = pts.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        if (a.y <= y) != (b.y <= y) && x < a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y) {
            odd = !odd;
        }
    }
    odd
}

/// The panel as a polygon: the same 22% white fill, its coverage from 4x4 samples a pixel.
fn draw_outline(canvas: &mut [u8], pts: &[vo_scene::geom::Point]) {
    let (x0, y0, x1, y1) = pts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, c, d), p| {
        (a.min(p.x), b.min(p.y), c.max(p.x), d.max(p.y))
    });
    for (i, px) in canvas.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % OW) as f32, (i as u32 / OW) as f32);
        let mut hits = 0;
        if x + 1.0 >= x0 && x <= x1 && y + 1.0 >= y0 && y <= y1 {
            for k in 0..16 {
                hits += inside(pts, x + 0.125 + 0.25 * (k % 4) as f32, y + 0.125 + 0.25 * (k / 4) as f32) as u32;
            }
        }
        let v = (hits as f32 / 16.0 * 0.22 * 255.0) as u8;
        px.copy_from_slice(&[v, v, v, v]);
    }
}

fn sd_round_box(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - hw + r;
    let qy = (py - cy).abs() - hh + r;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

impl App {
    fn t(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn panel_x(&self) -> f32 {
        if self.moving {
            let s = (self.t() * 1.3).sin() as f32;
            (OW as f32 - PW) * 0.5 + s * (OW as f32 - PW - 40.0) * 0.5
        } else {
            (OW as f32 - PW) * 0.5
        }
    }

    fn draw_back(&mut self) {
        let (w, h) = self.back_size;
        if w == 0 || h == 0 || self.back_drawn {
            return;
        }
        let stride = w as i32 * 4;
        let (buf, canvas) =
            self.pool.create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888).expect("buffer");
        for (i, px) in canvas.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = (i as u32 % w) as f32;
            let y = (i as u32 / w) as f32;
            if let (Some(cx), Some(cy)) = ((i as u32 % w).checked_div(self.checker), (i as u32 / w).checked_div(self.checker)) {
                let on = (cx + cy).is_multiple_of(2);
                let v = if on { 255 } else { 0 };
                px.copy_from_slice(&[v, v, v, 255]);
                continue;
            }
            // Wide colour bars, thin black and white lines, and a checker: blur smears all three.
            let bar = ((x / 96.0) as u32 + (y / 160.0) as u32) % 6;
            let (mut r, mut g, mut b) =
                [(230, 40, 50), (250, 200, 30), (40, 180, 80), (30, 120, 230), (150, 50, 200), (240, 240, 240)]
                    [bar as usize];
            if (x as u32 / 6).is_multiple_of(2) && (y as u32 / 40).is_multiple_of(3) {
                (r, g, b) = (0, 0, 0);
            }
            if (y as u32) % 24 < 2 {
                (r, g, b) = (255, 255, 255);
            }
            if ((x as u32 / 12) + (y as u32 / 12)).is_multiple_of(2) && (y as u32 / 80) % 4 == 1 {
                (r, g, b) = (r / 3, g / 3, b / 3);
            }
            px.copy_from_slice(&[b, g, r, 255]);
        }
        let wl = self.back.wl_surface();
        buf.attach_to(wl).expect("attach");
        wl.damage_buffer(0, 0, w as i32, h as i32);
        wl.commit();
        self.back_drawn = true;
    }

    fn draw_over(&mut self, qh: &QueueHandle<App>) {
        if !self.over_ready || self.backdrop_only {
            return;
        }
        let px0 = self.panel_x();
        let py0 = (OH as f32 - PH) * 0.5;
        let stride = OW as i32 * 4;
        let (buf, canvas) =
            self.pool.create_buffer(OW as i32, OH as i32, stride, wl_shm::Format::Argb8888).expect("buffer");
        let (cx, cy) = (px0 + PW * 0.5, py0 + PH * 0.5);
        if let Some(pts) = &self.outline {
            draw_outline(canvas, pts);
        }
        for (i, px) in canvas.as_chunks_mut::<4>().0.iter_mut().enumerate().filter(|_| self.outline.is_none()) {
            let x = (i as u32 % OW) as f32 + 0.5;
            let y = (i as u32 / OW) as f32 + 0.5;
            let d = sd_round_box(x, y, cx, cy, PW * 0.5, PH * 0.5, R);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            // A light 22% white fill and a 1 px brighter rim, premultiplied.
            let rim = (1.0 - (d + 1.0).abs()).clamp(0.0, 1.0) * 0.5;
            let a = (cov * 0.22 + rim).min(1.0);
            let v = (a * 255.0) as u8;
            px.copy_from_slice(&[v, v, v, v]);
        }
        let wl = self.over.wl_surface().clone();
        if let Some(e) = &self.effect {
            let region = Region::new(&self.compositor).expect("region");
            if let Some(pts) = &self.outline {
                let mut rects = Vec::new();
                vo_scene::glass::outline_blur_rects(pts, 1.0, &mut rects);
                for r in &rects {
                    region.add(r.x, r.y, r.w, r.h);
                }
                if self.region_sends == 0 {
                    println!("blur_probe: outline region {} rects", rects.len());
                }
            } else {
                for (x, y, w, h) in rounded_rects(px0, py0, PW, PH, R, self.steps) {
                    region.add(x, y, w, h);
                }
            }
            e.set_blur_region(Some(region.wl_region()));
            self.region_sends += 1;
        }
        buf.attach_to(&wl).expect("attach");
        wl.damage_buffer(0, 0, OW as i32, OH as i32);
        if self.moving {
            wl.frame(qh, wl.clone());
        }
        wl.commit();
        self.frames += 1;
        let t = self.t();
        if t - self.last_report >= 1.0 {
            self.last_report = t;
            println!("blur_probe: t={t:.1}s frames={} region updates={}", self.frames, self.region_sends);
        }
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for App {
    fn event(
        app: &mut App,
        _: &ExtBackgroundEffectManagerV1,
        ev: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = ev {
            let bits = match flags {
                WEnum::Value(f) => f.bits(),
                WEnum::Unknown(u) => u,
            };
            app.caps = Some(bits);
            println!("blur_probe: capabilities 0x{bits:x} (blur {})", if bits & 1 != 0 { "yes" } else { "no" });
            if bits & 1 == 0 {
                println!("blur_probe: no compositor blur; the panel draws its plain tint");
            }
        }
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for App {
    fn event(
        _: &mut App,
        _: &ExtBackgroundEffectSurfaceV1,
        _: ext_background_effect_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        if self.t() >= self.secs {
            self.done = true;
            return;
        }
        self.draw_over(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.done = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        l: &LayerSurface,
        c: LayerSurfaceConfigure,
        _: u32,
    ) {
        if l == &self.back {
            self.back_size = c.new_size;
            self.draw_back();
        } else if l == &self.over && !self.over_ready {
            self.over_ready = true;
            self.draw_over(qh);
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers!(OutputState);
}

delegate_compositor!(App);
delegate_output!(App);
delegate_layer!(App);
delegate_shm!(App);
delegate_registry!(App);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let val =
        |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f64>().ok());
    let secs = val("--secs").unwrap_or(6.0);
    let steps = val("--steps").unwrap_or(4.0) as u32;
    let turn = val("--turn").unwrap_or(0.0) as f32;
    let octagon = val("--octagon").map(|c| c as f32);
    let checker = val("--checker").unwrap_or(0.0) as u32;
    // The still panel's outline in overlay pixels (EG-07): an octagon, or the rounded panel
    // turned. `--move` keeps the rounded panel.
    let outline = (octagon.is_some() || turn != 0.0).then(|| {
        use vo_scene::geom::{Affine, Point};
        use vo_scene::glass::GlassShape;
        let (px0, py0) = ((OW as f32 - PW) * 0.5, (OH as f32 - PH) * 0.5);
        let rect = [px0, py0, PW, PH];
        let shape = octagon.map_or(GlassShape::Corners([vo_scene::frame::Corner::Round(R); 4]), GlassShape::chamfer);
        let pts = shape.points(rect, R).expect("the panel has an outline");
        let (cx, cy) = (px0 + PW * 0.5, py0 + PH * 0.5);
        let m = Affine::translate(-cx, -cy).then(&Affine::rotate(turn.to_radians())).then(&Affine::translate(cx, cy));
        pts.into_iter().map(|p| m.apply(Point::new(p.x, p.y))).collect::<Vec<_>>()
    });

    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("zwlr_layer_shell_v1");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let manager: Option<ExtBackgroundEffectManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    println!(
        "blur_probe: ext_background_effect_manager_v1 {}",
        if manager.is_some() { "bound" } else { "NOT advertised" }
    );
    if manager.is_none() {
        println!("blur_probe: no compositor blur; the panel draws its plain tint");
    }

    let back = layer_shell.create_layer_surface(
        &qh,
        compositor.create_surface(&qh),
        Layer::Background,
        Some("vo-blur-backdrop"),
        None,
    );
    back.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    back.set_exclusive_zone(-1);
    back.set_keyboard_interactivity(KeyboardInteractivity::None);
    back.commit();

    let over = layer_shell.create_layer_surface(
        &qh,
        compositor.create_surface(&qh),
        Layer::Overlay,
        Some("vo-blur-probe"),
        None,
    );
    over.set_anchor(Anchor::TOP);
    over.set_margin(120, 0, 0, 0);
    over.set_size(OW, OH);
    over.set_keyboard_interactivity(KeyboardInteractivity::None);
    let empty = Region::new(&compositor).expect("region");
    over.wl_surface().set_input_region(Some(empty.wl_region()));
    let effect = if flag("--no-blur") {
        None
    } else {
        manager.as_ref().map(|m| m.get_background_effect(over.wl_surface(), &qh, ()))
    };
    over.commit();

    let pool = SlotPool::new((OW * OH * 4 * 3) as usize, &shm).expect("pool");
    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        shm,
        pool,
        back,
        back_size: (0, 0),
        back_drawn: false,
        over,
        over_ready: false,
        effect,
        caps: None,
        moving: flag("--move"),
        backdrop_only: flag("--backdrop-only"),
        steps,
        outline,
        checker,
        start: Instant::now(),
        frames: 0,
        region_sends: 0,
        last_report: 0.0,
        done: false,
        secs,
    };
    while !app.done {
        if !app.moving && app.t() >= app.secs {
            break;
        }
        if app.moving {
            queue.blocking_dispatch(&mut app).expect("dispatch");
        } else {
            queue.roundtrip(&mut app).expect("roundtrip");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    println!("blur_probe: done, caps={:?}, frames={}, region updates={}", app.caps, app.frames, app.region_sends);
}
