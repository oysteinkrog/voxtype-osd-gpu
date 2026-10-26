/* Test-only reference: dumps HarfBuzz hb-gpu blobs for every glyph of a
 * font so the Rust encoder in vo-render can be compared byte for byte.
 * Never linked into the overlay.
 *
 * Build: cc -O2 tools/hb-gpu-dump.c -o /var/tmp/claude/vo-p0/hb-gpu-dump \
 *          $(pkg-config --cflags --libs harfbuzz harfbuzz-gpu)
 * Run:   hb-gpu-dump FONT OUT
 * Output: per glyph, u32 gid, u32 byte length (0xFFFFFFFF on encode
 * failure), then the blob bytes, all little endian.
 */
#include <hb.h>
#include <hb-gpu.h>
#include <stdio.h>
#include <stdint.h>

int main (int argc, char **argv)
{
  if (argc != 3) { fprintf (stderr, "usage: %s FONT OUT\n", argv[0]); return 2; }
  hb_blob_t *fb = hb_blob_create_from_file_or_fail (argv[1]);
  if (!fb) { fprintf (stderr, "cannot read %s\n", argv[1]); return 1; }
  hb_face_t *face = hb_face_create (fb, 0);
  hb_font_t *font = hb_font_create (face);
  FILE *out = fopen (argv[2], "wb");
  if (!out) { fprintf (stderr, "cannot write %s\n", argv[2]); return 1; }
  hb_gpu_draw_t *draw = hb_gpu_draw_create_or_fail ();
  unsigned n = hb_face_get_glyph_count (face);
  for (uint32_t gid = 0; gid < n; gid++)
  {
    hb_gpu_draw_glyph (draw, font, gid);
    hb_blob_t *blob = hb_gpu_draw_encode (draw, NULL);
    unsigned len = 0;
    const char *data = blob ? hb_blob_get_data (blob, &len) : NULL;
    uint32_t hdr[2] = { gid, blob ? len : 0xFFFFFFFFu };
    fwrite (hdr, 4, 2, out);
    if (len) fwrite (data, 1, len, out);
    hb_blob_destroy (blob);
  }
  fclose (out);
  fprintf (stderr, "%u glyphs\n", n);
  return 0;
}
