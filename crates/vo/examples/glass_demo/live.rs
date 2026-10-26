//! Live mode: the glass popup as a layer surface, with the blur region sent through the host
//! on every frame in which the panel moves. Run it only inside a nested compositor.

use vo::scene::display::{DisplayList, IRect, InputRegion};
use vo::scene::geom::{Point, Size};
use vo::scene::Scene;
use vo::{InputMode, OverlaySpec, Placement};
use vo_host::{Frame, Host, HostApp, HostCtl, HostEvent, SurfaceChange, SurfaceId};
use vo_host_wayland::WaylandHost;
use vo_render::{Renderer, Target};

use crate::glass::GlassPass;
use crate::{glass_at, glass_fps, layout, load_fonts, view, LOOK, RADIUS};

/// The show slide: 16 logical pixels over 300 ms, ease out.
const SLIDE: f32 = 16.0;
const SLIDE_SECS: f64 = 0.30;

/// Rectangles covering a rounded rect, one band per pixel row of the corner radius, each
/// rounded outward to whole logical pixels. The stair-step test in the `blur_probe` example
/// showed coarser bands leave visible blur steps outside the corner.
pub fn blur_region(rect: [f32; 4], r: f32) -> InputRegion {
    let (x0, y0) = (rect[0].floor() as i32, rect[1].floor() as i32);
    let (x1, y1) = ((rect[0] + rect[2]).ceil() as i32, (rect[1] + rect[3]).ceil() as i32);
    let r = r.min(rect[2] / 2.0).min(rect[3] / 2.0);
    let ri = r.ceil() as i32;
    let mut rects = Vec::with_capacity(2 * ri as usize + 1);
    for row in 0..ri {
        // The row's lower edge is nearest the straight side, so its inset is the smallest.
        let dy = (r - (row + 1) as f32).max(0.0);
        let inset = (r - (r * r - dy * dy).sqrt()).floor() as i32;
        let w = x1 - x0 - 2 * inset;
        rects.push(IRect { x: x0 + inset, y: y0 + row, w, h: 1 });
        rects.push(IRect { x: x0 + inset, y: y1 - row - 1, w, h: 1 });
    }
    rects.push(IRect { x: x0, y: y0 + ri, w: x1 - x0, h: (y1 - y0 - 2 * ri).max(0) });
    InputRegion { rects }
}

struct Live {
    blur: bool,
    secs: f64,
    size: Size,
    scene: Scene<()>,
    list: DisplayList,
    renderer: Option<Renderer>,
    glass: Option<GlassPass>,
    queue: Option<wgpu::Queue>,
    id: Option<SurfaceId>,
    shown_at: Option<f64>,
    last_rect: Option<[f32; 4]>,
    frames: u64,
    region_sends: u64,
    encode_ms: Vec<f64>,
    first_present: Option<f64>,
}

impl Live {
    fn panel_rect(&self, t: f64) -> [f32; 4] {
        let id = self.scene.find(&["root", "panel"]).expect("panel");
        let s = self.scene.sampled(id, t).expect("panel drawn");
        let o = s.transform.apply(Point::new(0.0, 0.0));
        [o.x, o.y, s.size.w, s.size.h]
    }
}

impl HostApp for Live {
    fn start(&mut self, host: &mut dyn HostCtl) {
        let spec = OverlaySpec {
            size: self.size,
            placement: Placement::FromTop { fraction: 0.2 },
            input: InputMode::None,
            app_id: "vo-glass-demo".into(),
            ..OverlaySpec::default()
        };
        let id = host.open(&spec).expect("open overlay");
        let gpu = host.gpu().expect("gpu");
        self.renderer = Some(Renderer::with_fonts(&gpu.device, &gpu.queue, load_fonts()));
        self.queue = Some(gpu.queue.clone());
        self.id = Some(id);
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Configured { id, t, size, scale } => {
                self.scene.set_viewport(t, size, scale);
                host.request_frame(id);
            }
            HostEvent::Presented { t, .. } if self.first_present.is_none() => {
                let _ = t;
                self.first_present = Some(vo_host_wayland::since_exec());
            }
            HostEvent::Timer { .. } => {
                if let Some(id) = self.id {
                    host.request_frame(id);
                }
            }
            HostEvent::Closed { .. } => host.exit(),
            _ => {}
        }
    }

    fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
        let start = std::time::Instant::now();
        if self.shown_at.is_none() {
            println!(
                "glass_demo live: compositor blur {}",
                if host.caps().backdrop_blur { "offered" } else { "not offered" }
            );
        }
        let shown_at = *self.shown_at.get_or_insert(frame.t);
        let t = frame.t - shown_at;
        if t >= self.secs {
            host.exit();
            return;
        }
        let k = (t / SLIDE_SECS).clamp(0.0, 1.0) as f32;
        let slide = SLIDE * (1.0 - k).powi(3);
        self.scene.apply(view(&LOOK, t, slide), frame.t);
        self.scene.sample_into(frame.t, &mut self.list);
        let rect = self.panel_rect(frame.t);

        let device_queue = self.queue.as_ref().expect("gpu");
        let renderer = self.renderer.as_mut().expect("gpu");
        let format = frame.texture.format();
        let glass = self.glass.get_or_insert_with(|| GlassPass::new(renderer.device(), format));
        let target = Target::from_texture(frame.texture.clone());
        let view_tex = frame.texture.create_view(&Default::default());
        let mut enc = renderer.device().create_command_encoder(&Default::default());
        renderer.encode(&self.list, frame.scale, &target, &mut enc);
        glass.encode(
            device_queue,
            &mut enc,
            &view_tex,
            frame.size,
            frame.scale,
            &glass_at(&LOOK, rect, t, self.blur),
            None,
        );
        device_queue.submit([enc.finish()]);

        // The region rides on this frame's commit, only when the panel moved.
        if self.blur && self.last_rect != Some(rect) {
            let _ = host.update(frame.id, SurfaceChange::Blur(Some(blur_region(rect, RADIUS))));
            self.region_sends += 1;
        }
        self.last_rect = Some(rect);
        self.frames += 1;
        self.encode_ms.push(start.elapsed().as_secs_f64() * 1e3);

        // The host drops frame requests made while it draws, so the next frame comes from a
        // timer: 60 fps during the sheen, 30 fps for the ambient rim (FX08).
        host.wake_at(Some(frame.t + 1.0 / glass_fps(t)));
    }
}

pub fn run(secs: f64, blur: bool) {
    let fonts = load_fonts();
    let (scene, size, _) = layout(&fonts, &LOOK);
    let mut app = Live {
        blur,
        secs,
        size,
        scene,
        list: DisplayList::default(),
        renderer: None,
        glass: None,
        queue: None,
        id: None,
        shown_at: None,
        last_rect: None,
        frames: 0,
        region_sends: 0,
        encode_ms: Vec::new(),
        first_present: None,
    };
    let host = WaylandHost::connect().expect("Wayland");
    if let Err(e) = host.run(&mut app) {
        eprintln!("glass_demo live: {e}");
        std::process::exit(1);
    }
    let mut ms = app.encode_ms.clone();
    ms.sort_by(f64::total_cmp);
    println!(
        "glass_demo live: {} frames in {secs} s, {} blur region updates, first frame on screen {:.1} ms after exec, \
         CPU per frame median {:.3} ms max {:.3} ms",
        app.frames,
        app.region_sends,
        app.first_present.unwrap_or(f64::NAN) * 1e3,
        ms.get(ms.len() / 2).copied().unwrap_or(0.0),
        ms.last().copied().unwrap_or(0.0),
    );
}
