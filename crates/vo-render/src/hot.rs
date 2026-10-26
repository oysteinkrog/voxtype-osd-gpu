//! Live WGSL for the preview tool: with `VO_SHADER_DIR` set, shader files are read from that
//! folder instead of the copies compiled in, and [`Renderer::reload_shaders`] picks up edits
//! without a restart. Unset (the normal case), nothing is read from disk.
//!
//! [`Renderer::reload_shaders`]: crate::Renderer::reload_shaders

use std::borrow::Cow;
use std::path::Path;

/// The source of `file` (a name such as `shapes.wgsl`): from `VO_SHADER_DIR` when it is set
/// and has the file, else `builtin`.
pub fn wgsl(file: &str, builtin: &'static str) -> Cow<'static, str> {
    match std::env::var_os("VO_SHADER_DIR") {
        Some(dir) => std::fs::read_to_string(Path::new(&dir).join(file)).map_or(Cow::Borrowed(builtin), Cow::Owned),
        None => Cow::Borrowed(builtin),
    }
}
