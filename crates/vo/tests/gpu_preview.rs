//! The preview's contact sheets on a real device: a custom scene (`Preview::custom`) draws a
//! colour that depends on `t`, and an app scene renders the same sheet twice.

use vo::preview::sheet::SheetSpec;
use vo::preview::Preview;
use vo::scene::element::Element;
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, Options, Scenario, Tokens};

/// Clears the target to a grey that is `t / 2` bright, from a fresh pass each frame.
fn custom() -> Preview {
    Preview::custom("grey", Size::new(64.0, 32.0), 2.0, |device, queue| {
        let (device, queue) = (device.clone(), queue.clone());
        move |t: f64, target: &vo_render::Target| {
            let view = target.texture().create_view(&Default::default());
            let mut enc = device.create_command_encoder(&Default::default());
            let v = (t / 2.0).clamp(0.0, 1.0);
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: v, g: v, b: v, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            queue.submit([enc.finish()]);
        }
    })
}

fn pixel(sheet: &vo::preview::sheet::Sheet, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * sheet.width + x) * 4) as usize;
    sheet.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn custom_scene_draws_each_frame_from_t() {
    let spec = SheetSpec { from: 0.0, to: 2.0, frames: 3, cols: 3, thumb_w: 64, fps: 60.0 };
    let sheet = custom().sheet(&spec).expect("sheet");
    // Thumbnails start 8 px in and are 64 + 8 px apart; sample each one's middle.
    let grey = |i: u32| pixel(&sheet, 8 + i * 72 + 32, 8 + 16)[0];
    assert_eq!(grey(0), 0);
    assert!(grey(1) > 150 && grey(1) < 200, "{}", grey(1));
    assert_eq!(grey(2), 255);
}

struct Grow;

impl App for Grow {
    type Msg = ();

    fn init(&mut self, cx: &mut Cx<()>) {
        cx.after(0.5, ());
    }

    fn update(&mut self, _: (), _: &mut Cx<()>) {}

    fn view(&self, _: &Tokens) -> Element<()> {
        Element::panel("root").size(64.0, 32.0).fill(Color::hex(0x3fc1ff, 1.0))
    }
}

#[test]
fn app_sheets_are_reproducible() {
    let make = || {
        let scenario = Scenario { size: Size::new(64.0, 32.0), scale: 1.0, duration: 1.0, inputs: Vec::new() };
        Preview::new("grow", || Grow, Options::default, scenario)
    };
    let spec = SheetSpec { from: 0.0, to: 1.0, frames: 4, cols: 2, thumb_w: 64, fps: 60.0 };
    let a = make().sheet(&spec).expect("sheet");
    let b = make().sheet(&spec).expect("sheet");
    assert_eq!((a.width, a.height), (2 * 64 + 3 * 8, 2 * (32 + 20) + 3 * 8));
    assert!(a.rgba == b.rgba);
    assert_eq!(pixel(&a, 8 + 32, 8 + 16), [0x3f, 0xc1, 0xff, 255]);
}
