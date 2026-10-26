// `cs_anchor`, with `particles.wgsl` in front.

// --- compute: re-anchor one word slot at the time of its change ---

@group(0) @binding(0) var<uniform> anchor: Anchor;
@group(0) @binding(1) var<storage, read_write> words_rw: array<Word>;
@group(0) @binding(2) var<storage, read> targets: array<Target>;

@compute @workgroup_size(64)
fn cs_anchor (@builtin(global_invocation_id) gid: vec3u) {
  let k = gid.x;
  if (k >= anchor.capacity) {
    return;
  }
  let i = anchor.slot * anchor.capacity + k;
  var p = words_rw[i];
  let tg = targets[i];
  let t = anchor.t;
  // Where the old spring has the particle exactly at the change time.
  let old = eval_word (p, t);
  let h0 = to_unit (rand (anchor.seed, anchor.slot, k, anchor.gen * 4u));
  let h1 = to_unit (rand (anchor.seed, anchor.slot, k, anchor.gen * 4u + 1u));
  let h2 = to_unit (rand (anchor.seed, anchor.slot, k, anchor.gen * 4u + 2u));
  let was_active = (p.flags & W_ACTIVE) != 0u && (p.flags & W_DYING) == 0u;
  let s = anchor.size;
  p.t0 = t;
  p.gen = anchor.gen;
  p.w = anchor.w * (0.8 + 0.4 * h0);
  p.stroke = anchor.stroke;
  if ((tg.flags & T_ACTIVE) != 0u) {
    p.goal = tg.pos;
    p.reveal_t0 = tg.reveal_t0;
    p.reveal_delay = tg.reveal_delay;
    let persist = select (0u, W_PERSIST, (tg.flags & T_PERSIST) != 0u);
    if ((tg.flags & T_START) != 0u) {
      // A start given from outside: a dot peeling off the grid, a point leaving a shape.
      p.start = tg.start;
      p.v0 = vec2f (0.0);
      p.delay = tg.reveal_delay;
      p.flags = W_ACTIVE | persist;
    } else if (!was_active) {
      // Fresh: start in a loose cloud around the target, a little low and to the left.
      let a = h1 * 2.0 * PI;
      let r = sqrt (h2) * 0.6 * s;
      p.start = tg.pos + vec2f (cos (a), sin (a)) * r + vec2f (-0.25, 0.3) * s;
      p.v0 = vec2f (0.0);
      // The glyph's sweep delay, so a word forms left to right.
      p.delay = tg.reveal_delay;
      p.flags = W_ACTIVE | W_FRESH | persist;
    } else if ((tg.flags & T_CHANGED) != 0u) {
      // A changed glyph: scatter from where the particle is, then form the new glyph.
      let a = h1 * 2.0 * PI;
      let out = normalize (old.x - vec2f (anchor.x0 + 0.5 * anchor.width, -0.35 * s) + vec2f (cos (a), sin (a)) * 0.3 * s);
      p.start = old.x;
      p.v0 = old.v + out * anchor.scatter * s * (0.6 + 0.8 * h2);
      p.delay = 0.0;
      p.flags = W_ACTIVE | persist;
    } else {
      // Unchanged glyph: slide to its (maybe moved) place, no impulse, stays hidden.
      p.start = old.x;
      p.v0 = old.v;
      p.delay = 0.0;
      p.flags = W_ACTIVE | persist;
    }
  } else if (was_active) {
    // The glyph went away: scatter and fade out.
    let a = h1 * 2.0 * PI;
    p.start = old.x;
    p.v0 = old.v + vec2f (cos (a), sin (a)) * anchor.scatter * s * (0.4 + 0.6 * h2);
    p.goal = old.x + p.v0 / p.w;
    p.delay = 0.0;
    p.flags = W_DYING;
  } else {
    p.flags = 0u;
  }
  words_rw[i] = p;
}
