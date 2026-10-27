//! A small SVG importer for icons and logos (FX-082): reads the shapes of an icon file into
//! paths the vector and glyph pipelines draw.
//!
//! It reads `path`, `circle`, `ellipse`, `rect` (with `rx`), `line`, `polyline` and
//! `polygon`, the `viewBox` (or `width` and `height`), `transform` on any element or group
//! (`matrix`, `translate`, `scale`, `rotate`), and whether each shape is filled, stroked or both
//! (`fill`, `stroke` and `stroke-width` as attributes or in a `style` attribute, inherited from
//! groups, with SVG's defaults: filled black, no stroke). That covers line icon sets such as
//! Lucide and Feather and most single-colour logos.
//!
//! It does not read CSS classes, gradients, `use`, clip paths, masks or text. Colours are not
//! kept: the element that draws the icon gives the colour.
//!
//! [`SvgIcon::filled`] gives one path to fill, with the strokes turned into outlines, ready for
//! `FontRegistry::add_path` and `Element::path(id, icon.view)`. [`SvgIcon::stroke`] keeps the
//! stroked shapes as centre lines for `Outline::new`, to trim or draw on.

use kurbo::{Affine, BezPath, Circle, Ellipse, Line, Point, Rect as KRect, RoundedRect, Shape};

use crate::geom::Rect;
use crate::path::icons;

/// An imported icon. See the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SvgIcon {
    /// The icon's own box (`viewBox`), in its units.
    pub view: Rect,
    /// Shapes to fill.
    pub fill: BezPath,
    /// Shapes to stroke, as centre lines, and the stroke width (the widest one seen).
    pub stroke: BezPath,
    pub stroke_width: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SvgError {
    /// A `d` attribute that does not parse; the text is the attribute.
    Path(String),
    /// No shapes found.
    Empty,
}

impl std::fmt::Display for SvgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvgError::Path(d) => write!(f, "cannot read SVG path data: {d}"),
            SvgError::Empty => write!(f, "no shapes in the SVG"),
        }
    }
}

impl std::error::Error for SvgError {}

impl SvgIcon {
    /// Everything as one path to fill: the fill shapes and the strokes as outlines with round
    /// caps and joins.
    pub fn filled(&self) -> BezPath {
        let mut p = self.fill.clone();
        if !self.stroke.elements().is_empty() {
            p.extend(icons::outline(&self.stroke, self.stroke_width));
        }
        p
    }
}

/// What a group passes to its children.
#[derive(Clone, Copy, Debug)]
struct Style {
    m: Affine,
    fill: bool,
    stroke: bool,
    width: f64,
}

/// Reads `svg`. See the module docs.
pub fn parse(svg: &str) -> Result<SvgIcon, SvgError> {
    let mut icon = SvgIcon::default();
    let mut stack = vec![Style { m: Affine::IDENTITY, fill: true, stroke: false, width: 1.0 }];
    let mut rest = svg;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[..close];
        rest = &rest[close + 1..];
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            if matches!(name.trim(), "g" | "svg" | "a") && stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name = tag.split(|c: char| c.is_whitespace()).next().unwrap_or("");
        let attrs = Attrs(tag);
        let parent = *stack.last().unwrap();
        let style = attrs.style(parent);
        if name == "svg" {
            icon.view = attrs.view_box();
        }
        if matches!(name, "g" | "svg" | "a") {
            if !self_closing {
                stack.push(style);
            }
            continue;
        }
        let Some(shape) = attrs.shape(name)? else { continue };
        let shape = style.m * shape;
        if style.fill {
            icon.fill.extend(shape.iter());
        }
        if style.stroke && style.width > 0.0 {
            icon.stroke.extend(shape.iter());
            icon.stroke_width =
                icon.stroke_width.max(style.width * style.m.as_coeffs()[0].hypot(style.m.as_coeffs()[1]));
        }
    }
    if icon.fill.elements().is_empty() && icon.stroke.elements().is_empty() {
        return Err(SvgError::Empty);
    }
    if icon.view.w <= 0.0 || icon.view.h <= 0.0 {
        let b = icon.filled().bounding_box();
        icon.view = Rect::new(b.x0 as f32, b.y0 as f32, b.width() as f32, b.height() as f32);
    }
    Ok(icon)
}

/// Reads `svg` and gives the path to fill, or `None` when it has no shapes it can read.
pub fn filled(svg: &str) -> Option<(BezPath, Rect)> {
    parse(svg).ok().map(|i| (i.filled(), i.view))
}

/// The attributes of one tag.
struct Attrs<'a>(&'a str);

impl<'a> Attrs<'a> {
    /// The value of attribute `key`, or of `key` in the `style` attribute.
    fn get(&self, key: &str) -> Option<&'a str> {
        let s = self.0;
        let mut from = 0;
        while let Some(i) = s[from..].find(key) {
            let at = from + i;
            from = at + key.len();
            let before = s[..at].chars().next_back();
            if !before.is_some_and(|c| c.is_whitespace()) {
                continue;
            }
            let after = s[from..].trim_start();
            let Some(after) = after.strip_prefix('=') else { continue };
            let after = after.trim_start();
            let q = after.chars().next()?;
            if q != '"' && q != '\'' {
                continue;
            }
            let body = &after[1..];
            return body.find(q).map(|end| &body[..end]);
        }
        let style = if key == "style" { None } else { self.get("style") }?;
        style.split(';').find_map(|decl| {
            let (k, v) = decl.split_once(':')?;
            (k.trim() == key).then_some(v.trim())
        })
    }

    fn num(&self, key: &str) -> f64 {
        self.get(key).and_then(|v| v.trim().trim_end_matches("px").parse().ok()).unwrap_or(0.0)
    }

    fn style(&self, parent: Style) -> Style {
        let paint = |v: Option<&str>, inherited: bool| v.map_or(inherited, |v| v.trim() != "none");
        Style {
            m: parent.m * self.get("transform").map_or(Affine::IDENTITY, transform),
            fill: paint(self.get("fill"), parent.fill),
            stroke: paint(self.get("stroke"), parent.stroke),
            width: self
                .get("stroke-width")
                .and_then(|v| v.trim().trim_end_matches("px").parse().ok())
                .unwrap_or(parent.width),
        }
    }

    fn view_box(&self) -> Rect {
        if let Some(v) = self.get("viewBox") {
            let n = numbers(v);
            if n.len() == 4 {
                return Rect::new(n[0] as f32, n[1] as f32, n[2] as f32, n[3] as f32);
            }
        }
        Rect::new(0.0, 0.0, self.num("width") as f32, self.num("height") as f32)
    }

    fn shape(&self, name: &str) -> Result<Option<BezPath>, SvgError> {
        let n = |k| self.num(k);
        let tol = 0.01;
        Ok(Some(match name {
            "path" => {
                let d = self.get("d").unwrap_or("");
                BezPath::from_svg(d).map_err(|_| SvgError::Path(d.into()))?
            }
            "circle" => Circle::new((n("cx"), n("cy")), n("r")).to_path(tol),
            "ellipse" => Ellipse::new((n("cx"), n("cy")), (n("rx"), n("ry")), 0.0).to_path(tol),
            "rect" => {
                let r = KRect::new(n("x"), n("y"), n("x") + n("width"), n("y") + n("height"));
                let (rx, ry) = (self.get("rx").map(|_| n("rx")), self.get("ry").map(|_| n("ry")));
                let radius = rx.or(ry).unwrap_or(0.0).min(0.5 * r.width().min(r.height()));
                RoundedRect::from_rect(r, radius).to_path(tol)
            }
            "line" => Line::new((n("x1"), n("y1")), (n("x2"), n("y2"))).to_path(tol),
            "polyline" | "polygon" => {
                let v = numbers(self.get("points").unwrap_or(""));
                let mut p = BezPath::new();
                for (i, &[x, y]) in v.as_chunks::<2>().0.iter().enumerate() {
                    let q = Point::new(x, y);
                    if i == 0 {
                        p.move_to(q);
                    } else {
                        p.line_to(q);
                    }
                }
                if name == "polygon" {
                    p.close_path();
                }
                p
            }
            _ => return Ok(None),
        }))
    }
}

/// Numbers separated by spaces or commas.
fn numbers(s: &str) -> Vec<f64> {
    s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect()
}

/// A `transform` attribute: a list of `matrix`, `translate`, `scale` and `rotate`, applied
/// right to left as SVG does.
fn transform(s: &str) -> Affine {
    let mut m = Affine::IDENTITY;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else { break };
        let v = numbers(&rest[open + 1..open + close]);
        rest = &rest[open + close + 1..];
        let a = |i: usize, d: f64| v.get(i).copied().unwrap_or(d);
        let t = match name {
            "matrix" if v.len() == 6 => Affine::new([v[0], v[1], v[2], v[3], v[4], v[5]]),
            "translate" => Affine::translate((a(0, 0.0), a(1, 0.0))),
            "scale" => Affine::scale_non_uniform(a(0, 1.0), a(1, a(0, 1.0))),
            "rotate" => Affine::rotate_about(a(0, 0.0).to_radians(), Point::new(a(1, 0.0), a(2, 0.0))),
            _ => Affine::IDENTITY,
        };
        m *= t;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUCIDE_MIC: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"
        fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/>
        <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
        <line x1="12" x2="12" y1="19" y2="22"/>
      </svg>"#;

    #[test]
    fn a_line_icon_reads_as_strokes_in_its_view_box() {
        let icon = parse(LUCIDE_MIC).unwrap();
        assert_eq!(icon.view, Rect::new(0.0, 0.0, 24.0, 24.0));
        assert!(icon.fill.elements().is_empty());
        assert_eq!(icon.stroke_width, 2.0);
        let b = icon.stroke.bounding_box();
        assert!((b.x0 - 5.0).abs() < 1e-6 && (b.y0 - 2.0).abs() < 1e-6 && (b.y1 - 22.0).abs() < 1e-6, "{b:?}");
        // Filled, the strokes grow by half their width.
        let f = icon.filled().bounding_box();
        assert!((f.x0 - 4.0).abs() < 0.05 && (f.y1 - 23.0).abs() < 0.05, "{f:?}");
    }

    #[test]
    fn groups_pass_on_transforms_and_paint() {
        let svg = r#"<svg viewBox="0 0 100 50"><g transform="translate(10 5) scale(2)" style="fill:none;stroke:#000;stroke-width:3">
            <rect x="0" y="0" width="10" height="10" rx="2"/></g>
            <circle cx="80" cy="25" r="5"/><polygon points="0,0 4,0 2,3"/></svg>"#;
        let icon = parse(svg).unwrap();
        assert_eq!(icon.view, Rect::new(0.0, 0.0, 100.0, 50.0));
        let s = icon.stroke.bounding_box();
        assert!((s.x0 - 10.0).abs() < 1e-6 && (s.x1 - 30.0).abs() < 1e-6 && (s.y1 - 25.0).abs() < 1e-6, "{s:?}");
        // The stroke scales with the group: 3 times 2.
        assert_eq!(icon.stroke_width, 6.0);
        // The circle and the triangle sit outside the group: filled by default.
        let f = icon.fill.bounding_box();
        assert!((f.x0 - 0.0).abs() < 1e-6 && (f.x1 - 85.0).abs() < 1e-6, "{f:?}");
    }

    #[test]
    fn bad_path_data_and_empty_files_are_errors() {
        assert_eq!(parse("<svg viewBox='0 0 1 1'></svg>"), Err(SvgError::Empty));
        assert!(matches!(parse("<svg><path d='M 0 0 Q'/></svg>"), Err(SvgError::Path(_))));
        // No view box: the shapes' own bounds.
        let i = parse("<svg><rect x='2' y='3' width='4' height='5'/></svg>").unwrap();
        assert_eq!(i.view, Rect::new(2.0, 3.0, 4.0, 5.0));
    }

    #[test]
    fn attribute_names_match_whole_words() {
        let a = Attrs(r#"path stroke-width="2" d="M0 0L1 1" width="9""#);
        assert_eq!(a.get("width"), Some("9"));
        assert_eq!(a.get("stroke-width"), Some("2"));
        assert_eq!(a.get("d"), Some("M0 0L1 1"));
        assert_eq!(a.get("fill"), None);
    }
}
