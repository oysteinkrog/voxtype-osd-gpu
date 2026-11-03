//! Cursor shape check (F6 and its fallback). Run it only inside a private nested
//! `kwin_wayland --virtual`; `scripts/cursor-fallback.sh` does, with the shape global hidden.
//!
//!   cursor_probe [--shape text|default|pointer] [--secs S]
//!
//! Opens a 400 x 120 overlay that takes pointer input, anchored bottom centre 8 px up, and
//! asks for the shape over all of it. Move the pointer onto it with `fake_click move`. After
//! S seconds (default 3) it prints the host's `cursor:` notes and one line
//! `RESULT entered=<n>`, and exits.

use vo_host::{
    CursorShape, Frame, Gpu, Host, HostApp, HostCtl, HostEvent, InputMode, OverlaySpec, PointerKind, SurfaceChange,
};
use vo_host_wayland::WaylandHost;

struct Probe {
    shape: CursorShape,
    secs: f64,
    gpu: Option<Gpu>,
    entered: u32,
}

impl HostApp for Probe {
    fn start(&mut self, host: &mut dyn HostCtl) {
        let spec = OverlaySpec { input: InputMode::Whole, app_id: "vo-cursor-probe".into(), ..OverlaySpec::default() };
        let id = match host.open(&spec) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("cursor_probe: open failed: {e}");
                host.exit();
                return;
            }
        };
        if let Err(e) = host.update(id, SurfaceChange::Cursor(self.shape)) {
            eprintln!("cursor_probe: cursor: {e}");
        }
        match host.gpu() {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(e) => {
                eprintln!("cursor_probe: no GPU: {e}");
                host.exit();
                return;
            }
        }
        host.wake_at(Some(host.now() + self.secs));
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Configured { id, .. } => host.request_frame(id),
            HostEvent::Pointer(p) if p.kind == PointerKind::Enter => {
                self.entered += 1;
                println!("cursor_probe: pointer entered at ({:.0}, {:.0})", p.pos.x, p.pos.y);
            }
            HostEvent::Timer { .. } => {
                for n in host.diag().notes.iter().filter(|n| n.starts_with("cursor:")) {
                    println!("{n}");
                }
                println!("RESULT entered={}", self.entered);
                host.exit();
            }
            HostEvent::Closed { .. } => host.exit(),
            _ => {}
        }
    }

    fn frame(&mut self, _: &mut dyn HostCtl, frame: &Frame<'_>) {
        let gpu = self.gpu.as_ref().expect("GPU before the first frame");
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cursor_probe"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.1, g: 0.2, b: 0.3, a: 0.8 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let shape = match arg("--shape").as_deref() {
        None | Some("text") => CursorShape::Text,
        Some("default") => CursorShape::Default,
        Some("pointer") => CursorShape::Pointer,
        Some(other) => {
            eprintln!("cursor_probe: unknown shape {other}; use text, default or pointer");
            std::process::exit(2);
        }
    };
    let secs = arg("--secs").map_or(3.0, |v| v.parse().expect("--secs S"));
    let mut app = Probe { shape, secs, gpu: None, entered: 0 };
    let host = WaylandHost::connect().unwrap_or_else(|e| {
        eprintln!("cursor_probe: {e}");
        std::process::exit(2);
    });
    if let Err(e) = host.run(&mut app) {
        eprintln!("cursor_probe: {e}");
        std::process::exit(1);
    }
}
