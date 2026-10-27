//! The signal bus block (SIG04): the renderer uploads `DisplayList::signals` once per encode
//! and binds it as group 3, and a shader reads it through `sig_level()` and friends.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use vo_core::signals::{SignalBlock, SIGNALS_WGSL};
use vo_render::{Renderer, Target};
use vo_scene::DisplayList;

/// The shader text in common.wgsl matches the layout in vo-core.
#[test]
fn common_wgsl_declares_the_vo_core_block() {
    let common = include_str!("../src/wgsl/common.wgsl");
    for line in SIGNALS_WGSL.lines().map(str::trim).filter(|l| !l.is_empty()) {
        assert!(common.contains(line), "common.wgsl lacks `{line}`");
    }
}

/// A shader bound to `Renderer::signals_binding` reads the level, a band and the history of
/// the list last encoded.
#[test]
fn a_shader_reads_the_block_of_the_encoded_list() {
    let gpu = support::gpu();
    let (d, q) = (&gpu.device, &gpu.queue);
    let mut renderer = Renderer::new(d, q);
    let mut list = DisplayList::default();
    let mut block = SignalBlock::default();
    block.head[4] = 0.5;
    block.bands[7] = 0.25;
    block.ring[3] = 0.75;
    list.signals = block;
    let target = Target::new(d, 4, 4, wgpu::TextureFormat::Rgba8Unorm);
    let mut enc = d.create_command_encoder(&Default::default());
    renderer.encode(&list, 1.0, &target, &mut enc);
    q.submit([enc.finish()]);

    let (layout, bind) = renderer.signals_binding();
    let src = format!(
        "{SIGNALS_WGSL}\n@group(3) @binding(0) var<uniform> signals: Signals;\n{}",
        r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn fs(@builtin(position) p: vec4f) -> @location(0) vec4f {
    // Column 0 the level, column 1 band 7, column 2 history entry 3.
    let v = select(select(sig_ring(3u), sig_band(7u), p.x < 2.0), sig_level(), p.x < 1.0);
    return vec4f(0.0, 0.0, 0.0, v);
}
"#
    );
    let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("signals-test"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[None, None, None, Some(layout)],
        immediate_size: 0,
    });
    let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("signals-test"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });
    let alpha = support::render_alpha(&gpu, 3, 1, |pass| {
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(3, bind, &[]);
        pass.draw(0..3, 0..1);
    });
    let near = |a: u8, v: f32| (a as f32 / 255.0 - v).abs() < 0.01;
    assert!(near(alpha[0], 0.5) && near(alpha[1], 0.25) && near(alpha[2], 0.75), "{alpha:?}");
}
