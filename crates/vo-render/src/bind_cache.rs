//! Bind groups kept from frame to frame (bqw3).
//!
//! The renderer binds the same pooled textures, samplers and buffers every frame: the bloom
//! pyramid, the final pass, composites, blur passes. Making a bind group costs validation, a
//! descriptor set and its tracking, and freeing it after the frame costs again. A bind group
//! of the same layout and resources is the same bind group, so this keeps each one while it
//! is used, and drops those no frame used for two frames, with the textures they hold.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(Clone, PartialEq, Eq, Hash)]
enum Res {
    View(wgpu::TextureView),
    Sampler(wgpu::Sampler),
    Buffer(wgpu::Buffer, u64, Option<u64>),
}

type Key = (wgpu::BindGroupLayout, Vec<(u32, Res)>);

#[derive(Default)]
pub(crate) struct BindCache {
    groups: RefCell<HashMap<Key, (wgpu::BindGroup, u64)>>,
    frame: Cell<u64>,
}

impl BindCache {
    /// The bind group `desc` describes: the one kept for its layout and resources, or a new one.
    /// Entries other than texture views, samplers and buffers make a new one every time.
    pub(crate) fn get(&self, device: &wgpu::Device, desc: &wgpu::BindGroupDescriptor<'_>) -> wgpu::BindGroup {
        let mut res = Vec::with_capacity(desc.entries.len());
        for e in desc.entries {
            let r = match &e.resource {
                wgpu::BindingResource::TextureView(v) => Res::View((*v).clone()),
                wgpu::BindingResource::Sampler(s) => Res::Sampler((*s).clone()),
                wgpu::BindingResource::Buffer(b) => Res::Buffer(b.buffer.clone(), b.offset, b.size.map(|s| s.get())),
                _ => return device.create_bind_group(desc),
            };
            res.push((e.binding, r));
        }
        let frame = self.frame.get();
        let mut groups = self.groups.borrow_mut();
        let key = (desc.layout.clone(), res);
        if let Some((group, used)) = groups.get_mut(&key) {
            *used = frame;
            return group.clone();
        }
        let group = device.create_bind_group(desc);
        groups.insert(key, (group.clone(), frame));
        group
    }

    /// Starts a frame: bind groups no frame used for two frames go.
    pub(crate) fn next_frame(&self) {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        self.groups.borrow_mut().retain(|_, (_, used)| frame - *used <= 2);
    }

    /// Drops every kept bind group, and with them the textures they hold.
    pub(crate) fn clear(&self) {
        self.groups.borrow_mut().clear();
    }
}
