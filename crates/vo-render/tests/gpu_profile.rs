//! GPU timestamps per section and the frame report (`vo_render::profile`).
//!
//! GPU test: `just test-gpu` runs it.

use vo_render::profile::{timestamp_features, Section, SurfaceKind};
use vo_render::{Renderer, Target};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

fn device(features: wgpu::Features) -> (wgpu::Device, wgpu::Queue) {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no Vulkan adapter");
    let features = adapter.features() & features;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vo-profile-test"),
        required_features: features,
        ..Default::default()
    }))
    .expect("no GPU device")
}

/// A panel inside a bloom layer near the left edge, and a plain panel.
fn list(bloom: bool) -> vo_scene::display::DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(400.0, 200.0), 1.0);
    let lit = Element::panel("lit").size(60.0, 40.0).fill(Color::hex(0x4cc9f0, 1.0));
    let layer = Element::layer("fx").child(lit);
    let layer = if bloom { layer.bloom(1.0, 12.0) } else { layer };
    scene.apply(
        Element::row("root")
            .size(400.0, 200.0)
            .pad(4.0)
            .gap(40.0)
            .child(layer)
            .child(Element::panel("plain").size(80.0, 40.0).fill(Color::WHITE)),
        0.0,
    );
    scene.sample(0.0)
}

#[test]
fn a_bloom_frame_reports_its_layer_reach_and_section_times() {
    let (device, queue) = device(timestamp_features());
    let mut target = Target::new(&device, 400, 200, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut r = Renderer::new(&device, &queue);
    let timed = r.set_profiling(true);
    r.render(&list(true), &mut target);

    let report = r.frame_report();
    assert!(report.effects, "bloom takes the effect path");
    assert_eq!(report.size, [400, 200]);
    assert_eq!(report.surfaces[0].kind, SurfaceKind::Scene);
    let layer = report.surfaces.iter().find(|s| s.kind == SurfaceKind::Layer).expect("the bloom layer");
    let bloom = layer.bloom.expect("bloom on the layer");
    assert!(bloom.levels >= 1 && bloom.reach >= 6.0, "{bloom:?}");
    // The panel is at x 4, so the bloom's reach crosses the left edge; the texture covers it.
    assert!((layer.content[0] - 4.0).abs() <= 1.5, "{:?}", layer.content);
    assert!(layer.texture[0] < layer.content[0] - bloom.reach + 1.0, "{:?}", layer.texture);
    assert!(layer.label().starts_with("layer "));
    assert!(report.shapes >= 2 && report.pool_bytes > 0);

    if !timed {
        eprintln!("no timestamp queries on this device: section times not checked");
        assert!(r.last_timings().is_none());
        return;
    }
    let t = r.last_timings().expect("timings after the frame");
    let sections: Vec<Section> = t.passes.iter().map(|p| p.section).collect();
    let li = report.surfaces.iter().position(|s| s.kind == SurfaceKind::Layer).unwrap();
    assert!(sections.contains(&Section::Draw { surface: li }));
    assert!(sections.contains(&Section::Bloom { surface: li }));
    assert!(sections.contains(&Section::Draw { surface: 0 }));
    assert_eq!(sections.last(), Some(&Section::Final));
    assert!(t.passes.iter().any(|p| p.label.ends_with("bloom")));
    assert!(t.passes.iter().all(|p| p.ms >= 0.0) && t.total_ms > 0.0);
    let sum = t.sum(|_| true);
    assert!((sum - t.total_ms).abs() < 1e-6, "sections add up to the total: {sum} {}", t.total_ms);
    assert!(r.last_timings().is_none(), "a frame is read once");

    // The direct path: one section, and no layer.
    r.render(&list(false), &mut target);
    let t = r.last_timings().expect("timings");
    assert_eq!(t.passes.len(), 1);
    assert_eq!(t.passes[0].label, "target");
    assert!(!r.frame_report().effects);
}

#[test]
fn profiling_needs_timestamp_features() {
    let (device, queue) = device(wgpu::Features::empty());
    let mut r = Renderer::new(&device, &queue);
    assert!(!r.set_profiling(true));
    let mut target = Target::new(&device, 64, 64, wgpu::TextureFormat::Rgba8UnormSrgb);
    r.render(&list(true), &mut target);
    assert!(r.last_timings().is_none());
    assert!(!r.frame_report().surfaces.is_empty(), "the report needs no timestamps");
}
