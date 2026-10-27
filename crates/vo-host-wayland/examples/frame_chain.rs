//! Checks that a frame asked for from inside `HostApp::frame` is drawn. Run it only inside a
//! private nested KWin; `just budget` does.
//!
//! The app asks for its first frame when the surface is configured and every later one from
//! inside the frame before it, never from `Presented` or a timer. A host that drops a request
//! made during a frame stops after one frame.
//!
//!   frame_chain [--frames N] [--secs S]
//!
//! Prints `frame_chain: ok, N frames in T s` and exits 0, or names the frame it stalled at
//! after S seconds and exits 1.

use vo_host::{Frame, Gpu, Host, HostApp, HostCtl, HostEvent, OverlaySpec};
use vo_host_wayland::WaylandHost;

struct Chain {
    want: u64,
    secs: f64,
    gpu: Option<Gpu>,
    frames: u64,
    first_at: Option<f64>,
    ok: bool,
}

impl HostApp for Chain {
    fn start(&mut self, host: &mut dyn HostCtl) {
        if let Err(e) = host.open(&OverlaySpec { app_id: "vo-frame-chain".into(), ..OverlaySpec::default() }) {
            eprintln!("frame_chain: open failed: {e}");
            host.exit();
            return;
        }
        match host.gpu() {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(e) => {
                eprintln!("frame_chain: no GPU: {e}");
                host.exit();
                return;
            }
        }
        host.wake_at(Some(host.now() + self.secs));
    }

    fn event(&mut self, host: &mut dyn HostCtl, event: HostEvent) {
        match event {
            HostEvent::Configured { id, .. } if self.frames == 0 => host.request_frame(id),
            HostEvent::Timer { .. } => {
                eprintln!("frame_chain: stalled after {} of {} frames in {} s", self.frames, self.want, self.secs);
                host.exit();
            }
            HostEvent::Closed { .. } => host.exit(),
            _ => {}
        }
    }

    fn frame(&mut self, host: &mut dyn HostCtl, frame: &Frame<'_>) {
        let gpu = self.gpu.as_ref().expect("GPU before the first frame");
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let shade = (self.frames % 60) as f64 / 60.0;
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame_chain"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: shade * 0.5, b: 0.5, a: 0.5 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
        self.frames += 1;
        let first = *self.first_at.get_or_insert(frame.t);
        if self.frames < self.want {
            host.request_frame(frame.id);
        } else {
            println!("frame_chain: ok, {} frames in {:.2} s", self.frames, frame.t - first);
            self.ok = true;
            host.exit();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok());
    let mut app = Chain {
        want: val("--frames").unwrap_or(60.0) as u64,
        secs: val("--secs").unwrap_or(5.0),
        gpu: None,
        frames: 0,
        first_at: None,
        ok: false,
    };
    let host = match WaylandHost::connect() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("frame_chain: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = host.run(&mut app) {
        eprintln!("frame_chain: {e}");
    }
    std::process::exit(if app.ok { 0 } else { 1 });
}
