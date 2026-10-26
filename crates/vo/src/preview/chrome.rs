//! The preview window's own UI, built with the framework itself: a transport row, a timeline
//! track with markers, event ticks, the loop range and the playhead, and the debug overlays
//! over the stage. Everything is positioned by hand in window units, so the track geometry
//! that pointer drags use is known without reading layout back.

use vo_scene::display::ShapeKind;
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::{Color, Rect};
use vo_scene::taffy;

/// Height of the bar under the stage, window units.
pub const BAR_H: f32 = 104.0;
const PAD: f32 = 12.0;
const TRACK_H: f32 = 34.0;
/// Space kept around the stage.
pub const STAGE_MARGIN: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Overlay {
    Bounds,
    Springs,
    Graph,
    Stats,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiMsg {
    Start,
    StepBack,
    PlayPause,
    Step,
    End,
    Speed(f64),
    Loop,
    LoopIn,
    LoopOut,
    Marker(usize),
    Toggle(Overlay),
    OneToOne,
}

/// One node box over the stage, in window units.
#[derive(Clone, Debug)]
pub struct NodeBox {
    pub rect: Rect,
    /// Where the node heads, when it moves.
    pub target: Option<Rect>,
    pub depth: usize,
}

/// Everything the chrome shows. Built fresh for each frame.
pub struct ChromeIn<'a> {
    pub size: (f32, f32),
    pub font: u32,
    pub mono: u32,
    pub t: f64,
    pub duration: f64,
    pub fps: f64,
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    pub range: (f64, f64),
    pub one_to_one: bool,
    pub fit: f32,
    pub markers: &'a [(String, f64)],
    pub events: &'a [f64],
    pub overlays: &'a [Overlay],
    pub boxes: &'a [NodeBox],
    /// Lines for the springs panel: moving channels.
    pub springs: &'a [String],
    /// Frame intervals and CPU times in ms, oldest first.
    pub graph: &'a [(f32, f32)],
    pub stats: &'a [String],
    /// One line under the readout: seek time, reload results, errors.
    pub status: &'a str,
}

/// The timeline track in window units.
pub fn track_rect(size: (f32, f32)) -> Rect {
    Rect::new(PAD, size.1 - PAD - TRACK_H, (size.0 - 2.0 * PAD).max(1.0), TRACK_H)
}

/// The time under window x on the track.
pub fn track_time(size: (f32, f32), x: f32, duration: f64) -> f64 {
    let r = track_rect(size);
    (((x - r.x) / r.w).clamp(0.0, 1.0) as f64) * duration
}

fn at<M>(e: Element<M>, x: f32, y: f32, w: f32, h: f32) -> Element<M> {
    use taffy::style::{LengthPercentageAuto, Position};
    e.size(w, h).layout(|s| {
        s.position = Position::Absolute;
        s.inset = taffy::Rect {
            left: LengthPercentageAuto::length(x),
            top: LengthPercentageAuto::length(y),
            right: LengthPercentageAuto::auto(),
            bottom: LengthPercentageAuto::auto(),
        };
    })
}

fn text<M>(key: impl Into<vo_scene::Key>, s: impl Into<String>, font: u32, size: f32, c: Color) -> Element<M> {
    Element::label(key, s).text_style(TextStyle::new(font, size, size * 1.3)).color(c)
}

const INK: u32 = 0xd6d9df;
const DIM: u32 = 0x8a909a;
const ACCENT: u32 = 0xff5a36;
const BOUNDS: u32 = 0x3fc1ff;
const TARGET: u32 = 0xffb02e;

fn button(key: &str, label: &str, font: u32, on: bool, msg: UiMsg) -> Element<UiMsg> {
    let fill = if on { Color::hex(0x4a2a22, 1.0) } else { Color::hex(0x272a31, 1.0) };
    let ink = if on { Color::hex(ACCENT, 1.0) } else { Color::hex(INK, 1.0) };
    Element::panel(key.to_string())
        .direction_row()
        .pad_xy(9.0, 4.0)
        .radius(5.0)
        .fill(fill)
        .on_click(msg)
        .child(text("l", label, font, 13.0, ink))
}

pub fn view(c: &ChromeIn) -> Element<UiMsg> {
    let (w, h) = c.size;
    let f = c.font;
    let frame = (c.t * c.fps).round() as i64;
    let total = (c.duration * c.fps).round() as i64;
    let speeds = [0.25, 0.5, 1.0, 2.0];
    let mut controls = vec![
        button("start", "|<", f, false, UiMsg::Start),
        button("back", "<", f, false, UiMsg::StepBack),
        button("play", if c.playing { "Pause" } else { "Play" }, f, c.playing, UiMsg::PlayPause),
        button("step", ">", f, false, UiMsg::Step),
        button("end", ">|", f, false, UiMsg::End),
        Element::panel("sp0").width(8.0),
    ];
    for (i, s) in speeds.iter().enumerate() {
        let label = if *s < 1.0 { format!("{s}x") } else { format!("{s:.0}x") };
        controls.push(button(&format!("speed{i}"), &label, f, c.speed == *s, UiMsg::Speed(*s)));
    }
    controls.push(Element::panel("sp1").width(8.0));
    controls.push(button("loop", "Loop", f, c.looping, UiMsg::Loop));
    controls.push(button("in", "In", f, false, UiMsg::LoopIn));
    controls.push(button("out", "Out", f, false, UiMsg::LoopOut));
    controls.push(Element::panel("sp2").width(8.0));
    for (i, (name, _)) in c.markers.iter().enumerate() {
        controls.push(button(&format!("m{i}"), name, f, false, UiMsg::Marker(i)));
    }
    controls.push(Element::panel("sp3").width(8.0));
    for (key, label, o) in [
        ("ob", "Bounds", Overlay::Bounds),
        ("os", "Springs", Overlay::Springs),
        ("og", "Graph", Overlay::Graph),
        ("ot", "Stats", Overlay::Stats),
    ] {
        controls.push(button(key, label, f, c.overlays.contains(&o), UiMsg::Toggle(o)));
    }
    controls.push(button("one", "1:1", f, c.one_to_one, UiMsg::OneToOne));

    let readout = format!(
        "{:7.3} s  f {frame:>4}/{total}  {:.0}%  {}",
        c.t,
        c.fit * 100.0,
        if c.looping { format!("loop {:.2}-{:.2}", c.range.0, c.range.1) } else { String::new() }
    );
    let bar_y = h - BAR_H;
    let bar = at(Element::panel("bar"), 0.0, bar_y, w, BAR_H).fill(Color::hex(0x15171c, 1.0)).child(at(
        Element::row("controls").gap(5.0).children(controls),
        PAD,
        8.0,
        w - 2.0 * PAD,
        24.0,
    ));
    let info = at(Element::row("info").gap(16.0), PAD, bar_y + 36.0, w - 2.0 * PAD, 18.0)
        .child(text("readout", readout, c.mono, 13.0, Color::hex(INK, 1.0)))
        .child(text("status", c.status, c.mono, 12.0, Color::hex(DIM, 1.0)));

    Element::panel("root").size(w, h).child(bar).child(info).child(track(c)).children(overlays(c))
}

fn track(c: &ChromeIn) -> Element<UiMsg> {
    let r = track_rect(c.size);
    let x_of = |t: f64| ((t / c.duration.max(1e-9)).clamp(0.0, 1.0) as f32) * r.w;
    let mut kids = Vec::new();
    kids.push(at(Element::panel("played"), 0.0, 0.0, x_of(c.t), r.h).fill(Color::hex(0x2c3340, 1.0)).radius(4.0));
    if c.looping {
        let (a, b) = (x_of(c.range.0), x_of(c.range.1));
        kids.push(
            at(Element::panel("range"), a, 0.0, (b - a).max(1.0), r.h)
                .fill(Color::hex(ACCENT, 0.16))
                .stroke(Color::hex(ACCENT, 0.6), 1.0),
        );
    }
    // Whole seconds.
    for s in 0..=c.duration.floor() as i64 {
        kids.push(at(Element::panel(format!("s{s}")), x_of(s as f64), r.h - 6.0, 1.0, 6.0).fill(Color::hex(DIM, 0.6)));
    }
    // Event ticks: inputs and timers, merged when closer than a pixel.
    let mut last_x = -10.0;
    for (i, e) in c.events.iter().enumerate() {
        let x = x_of(*e);
        if x - last_x < 1.0 {
            continue;
        }
        last_x = x;
        kids.push(at(Element::panel(format!("e{i}")), x, 0.0, 1.0, 7.0).fill(Color::hex(0x7fd4a0, 0.9)));
    }
    for (i, (name, t)) in c.markers.iter().enumerate() {
        let x = x_of(*t);
        kids.push(at(Element::panel(format!("mk{i}")), x - 0.5, 0.0, 2.0, r.h).fill(Color::hex(TARGET, 0.9)));
        kids.push(at(
            text(format!("ml{i}"), name.clone(), c.font, 11.0, Color::hex(TARGET, 1.0)),
            x + 4.0,
            9.0,
            80.0,
            16.0,
        ));
    }
    let x = x_of(c.t);
    kids.push(at(Element::panel("head"), x - 1.0, -4.0, 2.0, r.h + 8.0).fill(Color::hex(ACCENT, 1.0)));
    kids.push(at(Element::shape("knob", ShapeKind::Circle), x - 6.0, -10.0, 12.0, 12.0).fill(Color::hex(ACCENT, 1.0)));
    at(Element::panel("track"), r.x, r.y, r.w, r.h)
        .fill(Color::hex(0x1f2228, 1.0))
        .radius(4.0)
        .interactive()
        .children(kids)
}

fn overlays(c: &ChromeIn) -> Vec<Element<UiMsg>> {
    let mut out = Vec::new();
    if c.overlays.contains(&Overlay::Bounds) || c.overlays.contains(&Overlay::Springs) {
        let springs = c.overlays.contains(&Overlay::Springs);
        let bounds = c.overlays.contains(&Overlay::Bounds);
        for (i, b) in c.boxes.iter().enumerate() {
            let r = b.rect;
            if bounds {
                let a = (1.0 - 0.12 * b.depth as f32).max(0.35);
                out.push(
                    at(Element::panel(format!("bx{i}")), r.x, r.y, r.w.max(1.0), r.h.max(1.0))
                        .stroke(Color::hex(BOUNDS, a), 1.0),
                );
            }
            if let (true, Some(t)) = (springs, b.target) {
                out.push(
                    at(Element::panel(format!("tg{i}")), t.x, t.y, t.w.max(1.0), t.h.max(1.0))
                        .stroke(Color::hex(TARGET, 0.9), 1.0),
                );
                let (cx, cy) = (r.x + 0.5 * r.w, r.y + 0.5 * r.h);
                let (tx, ty) = (t.x + 0.5 * t.w, t.y + 0.5 * t.h);
                out.push(
                    at(Element::shape(format!("cd{i}"), ShapeKind::Circle), cx - 3.0, cy - 3.0, 6.0, 6.0)
                        .fill(Color::hex(BOUNDS, 1.0)),
                );
                out.push(
                    at(Element::shape(format!("td{i}"), ShapeKind::Circle), tx - 3.0, ty - 3.0, 6.0, 6.0)
                        .fill(Color::hex(TARGET, 1.0)),
                );
            }
        }
    }
    let mut panel_y = 12.0;
    let panel = |key: &str, lines: &[String], y: f32| {
        let h = 10.0 + 15.0 * lines.len() as f32;
        let kids =
            lines.iter().enumerate().map(|(i, l)| text(format!("l{i}"), l.clone(), c.mono, 11.5, Color::hex(INK, 1.0)));
        at(
            Element::panel(key.to_string()).pad(6.0).fill(Color::hex(0x0b0c0f, 0.82)).radius(4.0).children(kids),
            12.0,
            y,
            430.0,
            h,
        )
    };
    if c.overlays.contains(&Overlay::Springs) && !c.springs.is_empty() {
        out.push(panel("springs", c.springs, panel_y));
        panel_y += 14.0 + 15.0 * c.springs.len() as f32;
    }
    if c.overlays.contains(&Overlay::Stats) {
        out.push(panel("stats", c.stats, panel_y));
    }
    if c.overlays.contains(&Overlay::Graph) {
        out.push(graph(c));
    }
    out
}

/// Frame intervals (grey) and the CPU time of each frame (accent), with a 16.7 ms line.
fn graph(c: &ChromeIn) -> Element<UiMsg> {
    const GW: f32 = 300.0;
    const GH: f32 = 90.0;
    const MS_MAX: f32 = 40.0;
    let n = c.graph.len().max(1);
    let bw = GW / n as f32;
    let y = |ms: f32| GH - (ms / MS_MAX).clamp(0.0, 1.0) * GH;
    let mut kids = vec![at(Element::panel("budget"), 0.0, y(1000.0 / 60.0), GW, 1.0).fill(Color::hex(0x7fd4a0, 0.8))];
    for (i, (interval, cpu)) in c.graph.iter().enumerate() {
        let x = i as f32 * bw;
        kids.push(
            at(Element::panel(format!("i{i}")), x, y(*interval), bw.max(1.0), GH - y(*interval))
                .fill(Color::hex(DIM, 0.5)),
        );
        kids.push(
            at(Element::panel(format!("c{i}")), x, y(*cpu), bw.max(1.0), GH - y(*cpu)).fill(Color::hex(ACCENT, 0.9)),
        );
    }
    let last = c.graph.last().copied().unwrap_or_default();
    let worst = c.graph.iter().map(|g| g.1).fold(0.0, f32::max);
    kids.push(at(
        text(
            "cap",
            format!("frame {:.1} ms  cpu {:.2} ms  max {:.2}", last.0, last.1, worst),
            c.mono,
            11.0,
            Color::hex(INK, 1.0),
        ),
        4.0,
        2.0,
        GW,
        14.0,
    ));
    at(
        Element::panel("graph").fill(Color::hex(0x0b0c0f, 0.82)).radius(4.0).children(kids),
        c.size.0 - GW - 12.0,
        12.0,
        GW,
        GH,
    )
}
