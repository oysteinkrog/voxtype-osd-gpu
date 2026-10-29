// Breathing Grid that knows where the words are (EG-31): a dark panel with a 16 px grid and
// contour lines around the text, every 8 px out from the ink. Around each new word the
// contours and the grid brighten and lift: a rise over 0.4 s, a peak that settles to a
// third over 3 s, and a light from the top left on the side of each contour that faces
// away from the ink. Shading only: nothing moves.
//
// p[0]: the line colour. p[1].x: breathing speed (0 under reduced motion). p[1].y: how far
// the lift reaches from a word's box, in pixels.
fn mat_grid_words (m: Material, i: MatIn) -> vec4f {
  let line_c = mat_lab (m.p[0]).rgb;
  let reach = max (m.p[1].y, 1.0);
  let td = text_dist (i.px);
  let d = td.x;
  // The nearest word and how lit it is: rising, then settling to a third.
  let w = word_near (i.px);
  var near = 0.0;
  if (w.id != NO_WORD_ID) {
    let rise = smoothstep (0.0, 0.4, w.age);
    let hold = mix (0.34, 1.0, exp (-w.age / 3.0));
    near = exp (-w.dist / reach) * rise * hold;
  }
  // The grid, breathing slowly.
  let g = abs (fract (i.px / 16.0) - 0.5) * 16.0;
  let grid = 1.0 - smoothstep (0.35, 1.15, min (g.x, g.y));
  let breathe = 0.5 + 0.5 * sin (m.timing.x * m.p[1].x);
  // Contours of the text field, outside the ink, fading with distance.
  let c = abs (fract (d / 8.0) - 0.5) * 8.0;
  let contour = (1.0 - smoothstep (0.3, 1.1, c)) * step (1.0, d) * exp (-d / 70.0);
  // The lift: the contour side facing the light catches it.
  let facing = max (dot (td.yz, normalize (vec2f (-1.0, -1.0))), 0.0);
  let lum = (0.07 + 0.03 * breathe) * grid
    + (0.12 + 0.55 * near) * contour
    + 0.30 * near * grid
    + 0.35 * near * facing * contour;
  let a = i.cov * i.color.a;
  let base = vec3f (0.008, 0.010, 0.018);
  return vec4f ((base + line_c * lum) * a, a);
}
