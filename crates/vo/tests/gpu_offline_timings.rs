//! `Options::frame_timings` on a real device: an offline render hands the GPU time of every
//! frame of the main surface and of an extra surface (`App::surfaces`) to the sink.

use std::sync::{Arc, Mutex};

use vo::scene::element::Element;
use vo::scene::geom::{Color, Size};
use vo::{App, Cx, FrameTiming, Options, OverlaySpec, Scenario, SurfaceView, Tokens};

struct TwoSurfaces;

impl App for TwoSurfaces {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut Cx<()>) {}

    fn view(&self, _: &Tokens) -> Element<()> {
        Element::panel("card").size(120.0, 60.0).radius(12.0).fill(Color::hex(0x2a6df4, 1.0)).glow(
            Color::hex(0x2a6df4, 0.8),
            12.0,
            0.6,
        )
    }

    fn surfaces(&self, _: &Tokens) -> Vec<SurfaceView<()>> {
        let spec = OverlaySpec { size: Size::new(80.0, 40.0), ..OverlaySpec::default() };
        vec![SurfaceView::new("bar", spec, Element::panel("bar").grow(1.0).fill(Color::hex(0x15171c, 0.9)))]
    }
}

#[derive(Debug, PartialEq)]
struct Seen {
    surface: Option<String>,
    frame: u64,
    size: [u32; 2],
    total_ms: f64,
}

#[test]
fn offline_render_reports_each_frames_gpu_time() {
    let out =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("vo-offline-timings-{}", std::process::id()));
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let sink = seen.clone();
    let options = Options {
        frame_timings: Some(Arc::new(move |f: &FrameTiming| {
            sink.lock().unwrap().push(Seen {
                surface: f.surface.map(str::to_string),
                frame: f.frame,
                size: f.size,
                total_ms: f.timings.total_ms,
            });
        })),
        ..Options::default()
    };
    let scenario = Scenario { size: Size::new(160.0, 100.0), scale: 2.0, duration: 1.0, inputs: Vec::new() };
    let frames = vo::render_offline_with(TwoSurfaces, &scenario, 4.0, &out, options).expect("render");
    let _ = std::fs::remove_dir_all(&out);
    let seen = seen.lock().unwrap();
    if seen.is_empty() {
        eprintln!("skipped: this adapter has no GPU timestamps inside encoders");
        return;
    }
    let main: Vec<&Seen> = seen.iter().filter(|s| s.surface.is_none()).collect();
    let bar: Vec<&Seen> = seen.iter().filter(|s| s.surface.as_deref() == Some("bar")).collect();
    assert_eq!(main.len() as u64, frames, "one timing per main frame");
    assert_eq!(bar.len() as u64, frames, "one timing per extra surface frame");
    for (i, s) in main.iter().enumerate() {
        assert_eq!(s.frame, i as u64);
        assert_eq!(s.size, [320, 200]);
    }
    for (i, s) in bar.iter().enumerate() {
        assert_eq!(s.frame, i as u64);
        assert_eq!(s.size, [160, 80]);
    }
    assert!(seen.iter().all(|s| s.total_ms.is_finite() && s.total_ms >= 0.0), "{seen:?}");
}

#[test]
fn without_a_sink_the_render_is_unchanged() {
    let out =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("vo-offline-no-timings-{}", std::process::id()));
    let scenario = Scenario { size: Size::new(160.0, 100.0), scale: 1.0, duration: 0.5, inputs: Vec::new() };
    let frames = vo::render_offline_with(TwoSurfaces, &scenario, 4.0, &out, Options::default()).expect("render");
    let written = std::fs::read_dir(&out)
        .expect("out dir")
        .filter(|e| e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().starts_with("frame_")));
    assert_eq!(written.count() as u64, frames);
    let _ = std::fs::remove_dir_all(&out);
}
