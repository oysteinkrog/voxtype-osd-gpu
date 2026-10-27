//! Vector content animation: the status icon story, and the waveform, container, progress,
//! frame and erase motions. Each golden is a contact sheet: rows of frames of one motion at
//! chosen times, drawn through canvas elements.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::canvas::Painter;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Point, Rect, Size};
use vo_scene::status_icon::{IconStyle, StatusIcon};
use vo_scene::Scene;

/// One frame: draws into its box.
type Frame = Arc<dyn Fn(Rect, &mut Painter) + Send + Sync>;

const BG: u32 = 0x111318;
const CELL: u32 = 0x181b22;

/// Renders rows of `w` x `h` frames on a dark sheet and checks it against golden `name`.
fn sheet(name: &str, w: f32, h: f32, rows: Vec<Vec<Frame>>) -> Image {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as f32;
    let (pad, gap) = (8.0, 6.0);
    let size =
        Size::new(2.0 * pad + cols * w + (cols - 1.0).max(0.0) * gap, 2.0 * pad + rows.len() as f32 * (h + gap) - gap);
    let mut root = Element::<()>::panel("sheet").pad(pad).gap(gap).fill(Color::hex(BG, 1.0));
    for (r, row) in rows.into_iter().enumerate() {
        let mut line = Element::row(format!("row{r}")).gap(gap);
        for (c, frame) in row.into_iter().enumerate() {
            let cell = Element::canvas(format!("f{c}"), 0.0, move |_t, rect, p: &mut Painter| frame(rect, p));
            line = line
                .child(Element::panel(format!("c{c}")).fill(Color::hex(CELL, 1.0)).radius(6.0).child(cell.size(w, h)));
        }
        root = root.child(line);
    }
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(root, 0.0);
    let list = scene.sample(0.0);
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let [pw, ph] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let px = off.render_to_rgba(&list, [pw, ph], 1.0);
    Image { name: name.into(), w: pw, h: ph, channels: 4, px }
}

fn icon_frames(icon: Arc<StatusIcon>, times: &[f64]) -> Vec<Frame> {
    times
        .iter()
        .map(|&t| {
            let icon = icon.clone();
            Arc::new(move |r: Rect, p: &mut Painter| {
                icon.paint(t, Point::new(r.x + 0.5 * r.w, r.y + 0.5 * r.h), r.w.min(r.h), p)
            }) as Frame
        })
        .collect()
}

#[test]
fn status_icon_story_golden() {
    let mut main = StatusIcon::new(IconStyle::default());
    main.record(0.0);
    main.stop(1.0);
    main.finish(1.3);
    main.done(2.77);
    let mut cancel = StatusIcon::new(IconStyle::default());
    cancel.record(0.0);
    cancel.cancel(0.8);
    let rows = vec![
        // Pop, dot, dot to square, square, rounding, ring opening, spinning, spinning.
        icon_frames(Arc::new(main.clone()), &[0.06, 0.6, 1.06, 1.3, 1.38, 1.47, 1.8, 2.4]),
        // Done mid-spin: the arc closes forwards, then the check draws on.
        icon_frames(Arc::new(main), &[2.77, 2.85, 2.95, 3.05, 3.15, 3.25, 3.35, 3.8]),
        // Cancel: the dot shrinks while the X draws on.
        icon_frames(Arc::new(cancel), &[0.8, 0.86, 0.92, 0.98, 1.05, 1.15, 1.3, 1.6]),
    ];
    let img = sheet("story-icon", 64.0, 64.0, rows);
    support::check(&img, Tolerance::default()).unwrap();
}
