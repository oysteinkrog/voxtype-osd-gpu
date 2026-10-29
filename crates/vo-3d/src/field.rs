//! [`TextField`]: a distance field of the current text (TY19). It lives in
//! `vo_render::text_field` since EG-31, so 2D materials can read it too; it is re-exported
//! here with the same API.

pub use vo_render::text_field::{shader_source, TextField};

#[cfg(test)]
mod tests {
    #[test]
    fn field_shader_validates_in_naga() {
        let src = super::shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
