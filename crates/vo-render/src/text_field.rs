//! [`TextField`]: a distance field of the current text (TY19), so backgrounds, 3D scenes and
//! particles can react to the words without reading the text layout (CAPABILITIES section 3,
//! item 8). It moved here from `vo-3d` (EG-31) so 2D materials can read it too; `vo-3d`
//! re-exports it.
//!
//! The text display list is drawn by the normal 2D renderer (Slug coverage) at half
//! resolution into an `Rgba8Unorm` target, then two compute passes find, for every texel, the
//! nearest texel of the other kind (ink or not) exactly: a column pass, then a row search
//! (EG-39; a twelve-pass jump flood did it before, at about twice the GPU time). The result
//! is one `Rgba16Float` texture: signed distance in target pixels (negative inside the ink),
//! the unit direction away from the nearest ink, and the coverage. Coverage is alpha, so text
//! drawn at partial opacity counts as thinner ink.
//!
//! [`TextField::update`] skips all of it when the list, size and scale are the same as last
//! time, so a field over settled text costs nothing.
//!
//! Materials (EG-31): a material with `Material::reads_text` reads the field and a word table
//! through the textured group 2 (EG-02), with `text_dist (px)` and `word_near (px)` in WGSL
//! (`materials.wgsl` documents them). [`TextField::update_words`] builds the field from the
//! glyphs of a whole frame's list, and only when one of its materials reads text; it also
//! keeps each word's box and the time it first showed. [`TextField::word_table`] turns those
//! into a [`WordTable`] and [`TextField::bind`] hands both to the renderer.

use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use vo_scene::display::{DisplayList, ItemKind};
use vo_scene::geom::Color;
use vo_scene::material::READS_TEXT;

use crate::{Renderer, Target};

const FIELD_WGSL: &str = include_str!("wgsl/text_field.wgsl");

/// The largest field size in texels: the row pass holds a row of up to 3584 texels and its
/// block minimums in the 16 KiB of workgroup memory WebGPU guarantees, and the column pass a
/// column of up to 32768 texels.
const MAX_SIZE: [u32; 2] = [3584, 16384];

/// Words a material can see in the word table, newest first.
pub const MAX_WORDS: usize = 64;

/// The age [`TextField::word_table`] gives every word under reduced motion: long settled, so
/// a material shows its final state with no rise.
pub const SETTLED_AGE: f32 = 1.0e4;

pub fn shader_source() -> &'static str {
    FIELD_WGSL
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    size: [u32; 2],
    /// 1 when the list draws nothing.
    empty: u32,
    scale: f32,
}

/// The text field as a shader binds it.
pub struct FieldBinding<'a> {
    pub view: &'a wgpu::TextureView,
    /// The field texture's size in its own pixels.
    pub size: [u32; 2],
    /// Target pixels per field pixel.
    pub scale: f32,
}

struct Sized {
    size: [u32; 2],
    coverage: Target,
    /// The column pass's output, held for the bind groups.
    _columns: wgpu::Texture,
    field: wgpu::Texture,
    field_view: wgpu::TextureView,
    /// Bind groups: the column pass, the row pass.
    binds: [wgpu::BindGroup; 2],
}

pub struct TextField {
    device: wgpu::Device,
    queue: wgpu::Queue,
    columns: wgpu::ComputePipeline,
    rows: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    /// Whether `params` says the field is empty now.
    empty: bool,
    sized: Option<Sized>,
    /// Target pixels per field texel.
    downscale: f32,
    /// What the field shows now: the list, the target size and the scale.
    drawn: Option<(DisplayList, [u32; 2], f32)>,
    /// Times the field was built.
    builds: u64,
    /// The glyphs-only copy of the last frame's list, reused (`update_words`).
    scratch: DisplayList,
    /// Each word's box in target pixels and the time it first showed (`update_words`).
    words: Vec<WordBox>,
    pub pipeline_time: Duration,
}

/// One word the field holds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WordBox {
    id: u32,
    rect: [f32; 4],
    first: f64,
}

impl TextField {
    /// A field at half the target resolution.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> TextField {
        TextField::with_downscale(device, queue, 2.0)
    }

    /// A field with `downscale` target pixels per field texel.
    pub fn with_downscale(device: &wgpu::Device, queue: &wgpu::Queue, downscale: f32) -> TextField {
        // Measures pipeline creation for the warm-up report; no animation reads it.
        #[allow(clippy::disallowed_methods)]
        let start = Instant::now();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-text-field"),
            source: wgpu::ShaderSource::Wgsl(FIELD_WGSL.into()),
        });
        let pipe = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (columns, rows) = (pipe("cs_columns"), pipe("cs_rows"));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-text-field-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        TextField {
            device: device.clone(),
            queue: queue.clone(),
            columns,
            rows,
            params,
            empty: false,
            sized: None,
            downscale,
            drawn: None,
            builds: 0,
            scratch: DisplayList::default(),
            words: Vec::new(),
            pipeline_time: start.elapsed(),
        }
    }

    /// Records drawing `text` with `renderer` at `scale / downscale`, then the distance passes,
    /// into `enc`, and returns true. `target_size` is the size in physical pixels of the target the
    /// field will be used with. The renderer's buffers are written now, so submit `enc` before
    /// the renderer encodes another list. Records nothing and returns false when the field
    /// already shows this list at this size and scale.
    pub fn update(
        &mut self,
        renderer: &mut Renderer,
        text: &DisplayList,
        target_size: [u32; 2],
        scale: f32,
        enc: &mut wgpu::CommandEncoder,
    ) -> bool {
        if self.drawn.as_ref().is_some_and(|(l, s, k)| l == text && *s == target_size && *k == scale) {
            return false;
        }
        let size: [u32; 2] =
            std::array::from_fn(|i| ((target_size[i] as f32 / self.downscale).ceil() as u32).clamp(1, MAX_SIZE[i]));
        self.resize(size);
        let empty = draws_nothing(text);
        if empty != self.empty {
            self.empty = empty;
            self.write_params();
        }
        let s = self.sized.as_ref().unwrap();
        renderer.encode(text, scale / self.downscale, &s.coverage, enc);
        self.encode_field(enc);
        match &mut self.drawn {
            Some((l, s, k)) => {
                l.clone_from(text);
                (*s, *k) = (target_size, scale);
            }
            None => self.drawn = Some((text.clone(), target_size, scale)),
        }
        self.builds += 1;
        true
    }

    /// The field for materials (EG-31): when a material in `list` reads text
    /// (`Material::reads_text`), builds the field from `list`'s glyphs, as [`TextField::update`]
    /// does, and keeps each word's box and the time `t` it first showed. Returns whether it
    /// recorded a build. A list whose materials read no text costs one scan of its materials
    /// and nothing else; settled text costs a copy of its glyphs and a compare.
    ///
    /// Effects, materials, halos and every non-glyph item are left out of the field, so a
    /// material's own clock does not rebuild it. Submit `enc` before `renderer` encodes the
    /// frame, then call [`TextField::bind`].
    pub fn update_words(
        &mut self,
        renderer: &mut Renderer,
        list: &DisplayList,
        target_size: [u32; 2],
        scale: f32,
        t: f64,
        enc: &mut wgpu::CommandEncoder,
    ) -> bool {
        if !wants_text(list) {
            return false;
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        glyphs_only(list, &mut scratch);
        renderer.collect_word_boxes(true);
        let built = self.update(renderer, &scratch, target_size, scale, enc);
        renderer.collect_word_boxes(false);
        self.scratch = scratch;
        if built {
            let k = self.downscale;
            let mut words: Vec<WordBox> = Vec::new();
            for &(id, b) in renderer.word_boxes() {
                // Back to target pixels, without the pixel of dilation the quad carries.
                let r = [(b[0] + 1.0) * k, (b[1] + 1.0) * k, (b[2] - 1.0) * k, (b[3] - 1.0) * k];
                match words.iter_mut().find(|w| w.id == id) {
                    Some(w) => w.rect = union(w.rect, r),
                    None => {
                        let first = self.words.iter().find(|w| w.id == id).map_or(t, |w| w.first);
                        words.push(WordBox { id, rect: r, first });
                    }
                }
            }
            self.words = words;
        }
        built
    }

    /// The words of the last [`TextField::update_words`], newest first, at most
    /// [`MAX_WORDS`]. A word's age is `t` minus the time it first showed, or
    /// [`SETTLED_AGE`] under `reduced_motion`, so a material shows its settled look at once.
    pub fn word_table(&self, t: f64, reduced_motion: bool) -> WordTable {
        let mut order: Vec<&WordBox> = self.words.iter().collect();
        order.sort_by(|a, b| b.first.total_cmp(&a.first).then(b.id.cmp(&a.id)));
        let mut table = WordTable::default();
        for w in order.into_iter().take(MAX_WORDS) {
            let age = if reduced_motion { SETTLED_AGE } else { (t - w.first).max(0.0) as f32 };
            table.push(w.rect, w.id, age);
        }
        table
    }

    /// Hands the field and `words` to `renderer` for materials that read text. Without a
    /// field yet, materials read no text.
    pub fn bind(&self, renderer: &mut Renderer, words: &WordTable) {
        match &self.sized {
            Some(s) => renderer.set_text_inputs(Some((s.field_view.clone(), s.size, self.downscale)), words),
            None => renderer.set_text_inputs(None, words),
        }
    }

    /// Records the column and row passes. The coverage must already be drawn.
    fn encode_field(&self, enc: &mut wgpu::CommandEncoder) {
        let s = self.sized.as_ref().unwrap();
        let mut pass = enc
            .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo-text-field"), timestamp_writes: None });
        pass.set_pipeline(&self.columns);
        pass.set_bind_group(0, &s.binds[0], &[]);
        pass.dispatch_workgroups(s.size[0], 1, 1);
        pass.set_pipeline(&self.rows);
        pass.set_bind_group(0, &s.binds[1], &[]);
        pass.dispatch_workgroups(1, s.size[1], 1);
    }

    fn write_params(&self) {
        let size = self.sized.as_ref().map_or([0; 2], |s| s.size);
        let params = Params { size, empty: self.empty as u32, scale: self.downscale };
        self.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
    }

    fn resize(&mut self, size: [u32; 2]) {
        if self.sized.as_ref().is_some_and(|s| s.size == size) {
            return;
        }
        let d = &self.device;
        let tex = |label, format, usage| {
            d.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let coverage = Target::new(d, size[0], size[1], wgpu::TextureFormat::Rgba8Unorm);
        let columns =
            tex("vo-text-field-columns", wgpu::TextureFormat::R32Uint, U::STORAGE_BINDING | U::TEXTURE_BINDING);
        let field = tex(
            "vo-text-field",
            wgpu::TextureFormat::Rgba16Float,
            U::STORAGE_BINDING | U::TEXTURE_BINDING | U::COPY_SRC,
        );
        let field_view = field.create_view(&Default::default());
        let col_view = columns.create_view(&Default::default());
        let cov_view = coverage.texture().create_view(&Default::default());
        let view = |v| wgpu::BindingResource::TextureView(v);
        let params = || self.params.as_entire_binding();
        let binds = [
            d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-text-field-columns"),
                layout: &self.columns.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params() },
                    wgpu::BindGroupEntry { binding: 1, resource: view(&cov_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: view(&col_view) },
                ],
            }),
            d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-text-field-rows"),
                layout: &self.rows.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params() },
                    wgpu::BindGroupEntry { binding: 1, resource: view(&cov_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: view(&col_view) },
                    wgpu::BindGroupEntry { binding: 4, resource: view(&field_view) },
                ],
            }),
        ];
        self.sized = Some(Sized { size, coverage, _columns: columns, field, field_view, binds });
        self.write_params();
    }

    /// The field for a shader, once [`TextField::update`] has run.
    pub fn binding(&self) -> Option<FieldBinding<'_>> {
        let s = self.sized.as_ref()?;
        Some(FieldBinding { view: &s.field_view, size: s.size, scale: self.downscale })
    }

    /// The field texture, for readback and debugging.
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.sized.as_ref().map(|s| &s.field)
    }

    /// Times the field has been built (`update` returned true).
    pub fn builds(&self) -> u64 {
        self.builds
    }

    /// GPU memory the field holds now, in bytes: the coverage, the column pass's texture and
    /// the field itself.
    pub fn memory_bytes(&self) -> u64 {
        self.sized.as_ref().map_or(0, |s| {
            let texels = s.size[0] as u64 * s.size[1] as u64;
            texels * (4 + 4 + 8)
        })
    }

    /// The field texture's size in texels, once built.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.sized.as_ref().map(|s| s.size)
    }
}

/// Whether `list` draws nothing at all: every item is a layer or empty.
fn draws_nothing(list: &DisplayList) -> bool {
    list.items.iter().all(|it| it.kind == ItemKind::Layer || it.len == 0)
}

/// Whether a material in `list` reads the text field.
pub fn wants_text(list: &DisplayList) -> bool {
    list.materials.iter().any(|m| m.flags & READS_TEXT != 0)
}

/// `list` with only its glyphs into `out`: the layers and their transforms, clips and
/// opacity stay; every other item is emptied, layer effects that draw something of their own
/// go, and glyph effects keep only what changes the ink's shape.
pub fn glyphs_only(list: &DisplayList, out: &mut DisplayList) {
    out.clear();
    out.size = list.size;
    out.scale = list.scale;
    out.stage = list.stage;
    out.layers.extend(list.layers.iter().map(|l| {
        let mut l = l.clone();
        l.trail = None;
        l.style = None;
        l.backdrop = None;
        l.displace = None;
        l.blur = None;
        l.frost = 0.0;
        l
    }));
    out.items.extend(list.items.iter().map(|it| match it.kind {
        ItemKind::Glyphs | ItemKind::Layer => *it,
        _ => vo_scene::display::Item { kind: it.kind, start: 0, len: 0 },
    }));
    out.glyphs.extend_from_slice(&list.glyphs);
    out.glyph_fx.extend(list.glyph_fx.iter().map(|&(i, fx)| {
        (i, vo_scene::display::GlyphFx { material: 0, halo: Color::TRANSPARENT, halo_radius: 0.0, ..fx })
    }));
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// One word as a material reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuWord {
    /// The word's box in target pixels: left, top, right, bottom.
    pub rect: [f32; 4],
    /// The `vo-core` word id.
    pub id: u32,
    /// Seconds since the word first showed.
    pub age: f32,
    pub _pad: [f32; 2],
}

/// The words a material sees with `word_near` (EG-31): up to [`MAX_WORDS`] boxes in target
/// pixels, each with its word id and age. Push the newest first; `text_word (0u)` in WGSL is
/// the first pushed. [`TextField::word_table`] makes one from the field's own words.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WordTable {
    words: Vec<GpuWord>,
}

/// What `word_near` finds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordHit {
    pub id: u32,
    pub age: f32,
    /// Distance from the point to the word's box in pixels, 0 inside.
    pub dist: f32,
    pub rect: [f32; 4],
}

impl WordTable {
    /// Adds a word; a word past [`MAX_WORDS`] is dropped, and so is one with no box.
    pub fn push(&mut self, rect: [f32; 4], id: u32, age: f32) {
        if self.words.len() < MAX_WORDS && rect[2] >= rect[0] && rect[3] >= rect[1] {
            self.words.push(GpuWord { rect, id, age, _pad: [0.0; 2] });
        }
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn words(&self) -> &[GpuWord] {
        &self.words
    }

    /// Every age set to [`SETTLED_AGE`], for reduced motion.
    pub fn settled(mut self) -> WordTable {
        for w in &mut self.words {
            w.age = SETTLED_AGE;
        }
        self
    }

    /// The word whose box is nearest `px`, as `word_near` in WGSL finds it: the first of
    /// equally near words wins. `None` for an empty table.
    pub fn word_near(&self, px: [f32; 2]) -> Option<WordHit> {
        let mut best: Option<WordHit> = None;
        for w in &self.words {
            let qx = (w.rect[0] - px[0]).max(px[0] - w.rect[2]).max(0.0);
            let qy = (w.rect[1] - px[1]).max(px[1] - w.rect[3]).max(0.0);
            let dist = qx.hypot(qy);
            if best.is_none_or(|b| dist < b.dist) {
                best = Some(WordHit { id: w.id, age: w.age, dist, rect: w.rect });
            }
        }
        best
    }

    /// The buffer the shader reads: a head (count, field pixels per texel, field size), then
    /// [`MAX_WORDS`] words.
    pub fn gpu_bytes(&self, field: Option<([u32; 2], f32)>) -> Vec<u8> {
        let (size, k) = field.map_or(([0.0, 0.0], 0.0), |(s, k)| ([s[0] as f32, s[1] as f32], k));
        let head = [self.words.len() as f32, k, size[0], size[1]];
        let mut bytes = Vec::with_capacity(WORDS_BYTES);
        bytes.extend_from_slice(bytemuck::bytes_of(&head));
        bytes.extend_from_slice(bytemuck::cast_slice(&self.words));
        bytes.resize(WORDS_BYTES, 0);
        bytes
    }
}

/// Size of the word table buffer: the head and [`MAX_WORDS`] words.
pub const WORDS_BYTES: usize = 16 + MAX_WORDS * std::mem::size_of::<GpuWord>();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_shader_validates_in_naga() {
        let src = super::shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    #[test]
    fn word_near_finds_the_nearest_box() {
        let mut t = WordTable::default();
        t.push([10.0, 10.0, 50.0, 30.0], 7, 0.5);
        t.push([100.0, 10.0, 140.0, 30.0], 8, 2.0);
        let inside = t.word_near([20.0, 20.0]).unwrap();
        assert_eq!((inside.id, inside.dist), (7, 0.0));
        let right = t.word_near([130.0, 50.0]).unwrap();
        assert_eq!(right.id, 8);
        assert!((right.dist - 20.0).abs() < 1e-5);
        let corner = t.word_near([53.0, 34.0]).unwrap();
        assert!((corner.dist - 5.0).abs() < 1e-5 && corner.id == 7);
        assert!(WordTable::default().word_near([0.0, 0.0]).is_none());
    }

    #[test]
    fn the_table_holds_at_most_64_words() {
        let mut t = WordTable::default();
        for i in 0..80 {
            t.push([i as f32, 0.0, i as f32 + 1.0, 1.0], i, 0.0);
        }
        t.push([5.0, 5.0, 1.0, 1.0], 999, 0.0);
        assert_eq!(t.len(), MAX_WORDS);
        assert_eq!(t.words()[63].id, 63);
    }

    #[test]
    fn settled_ages_and_bytes() {
        let mut t = WordTable::default();
        t.push([0.0, 0.0, 4.0, 4.0], 3, 0.25);
        let s = t.clone().settled();
        assert_eq!(s.words()[0].age, SETTLED_AGE);
        let b = t.gpu_bytes(Some(([300, 150], 2.0)));
        assert_eq!(b.len(), WORDS_BYTES);
        let head: [f32; 4] = *bytemuck::from_bytes(&b[..16]);
        assert_eq!(head, [1.0, 2.0, 300.0, 150.0]);
        let w: GpuWord = *bytemuck::from_bytes(&b[16..48]);
        assert_eq!((w.id, w.age, w.rect), (3, 0.25, [0.0, 0.0, 4.0, 4.0]));
        let none: [f32; 4] = *bytemuck::from_bytes(&WordTable::default().gpu_bytes(None)[..16]);
        assert_eq!(none, [0.0; 4]);
    }

    #[test]
    fn glyphs_only_empties_other_items() {
        use vo_scene::display::{GlyphFx, GlyphInstance, Item, Layer};
        use vo_scene::geom::Point;
        let mut l = DisplayList::default();
        let mut root = Layer::root();
        root.items = 0..2;
        l.layers.push(root);
        l.items.push(Item { kind: ItemKind::Shapes, start: 3, len: 2 });
        l.items.push(Item { kind: ItemKind::Glyphs, start: 0, len: 1 });
        l.glyphs.push(GlyphInstance {
            pos: Point::new(1.0, 2.0),
            size: 10.0,
            font: 0,
            glyph: 5,
            word: 4,
            color: Color::WHITE,
            xform: vo_scene::display::GlyphXform::IDENTITY,
        });
        l.glyph_fx.push((0, GlyphFx { material: 2, blur: 1.0, ..Default::default() }));
        l.materials.push(vo_scene::material::Material::custom(64).reads_text());
        let mut out = DisplayList::default();
        glyphs_only(&l, &mut out);
        assert_eq!(out.items[0].len, 0);
        assert_eq!(out.items[1], l.items[1]);
        assert_eq!(out.glyph_fx[0].1.material, 0);
        assert_eq!(out.glyph_fx[0].1.blur, 1.0);
        assert!(out.materials.is_empty() && wants_text(&l) && !wants_text(&out));
    }
}
