//! Glyph rasterization: load a font, shape a string, and paint the outline
//! into a [`tiny_skia::Pixmap`]. `paint.rs` owns the rectangles and the
//! colour parsing; this module owns the part that actually draws letters.

use std::path::Path;

use swash::scale::outline::Outline;
use swash::scale::ScaleContext;
use swash::shape::ShapeContext;
use swash::zeno::Verb;
use swash::{FontRef, GlyphId};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

use crate::config::RenderConfig;
use crate::errors::{Error, Result};
use crate::render::layout::{cell_x, PopupMetrics};
use crate::render::paint;

/// A loaded font. Owns the file bytes because [`FontRef`] only borrows them.
#[derive(Debug)]
pub struct FontFace {
    data: Vec<u8>,
    offset: u32,
    // Cached because both `upem` and `font_ref` would otherwise re-derive it
    // every call; the key itself is cheap but the new `CacheKey` would
    // invalidate swash's internal lookup tables.
    key: swash::CacheKey,
    // Number of font design units per em. Cached so repeated `upem` calls
    // re-parse the head table.
    upem: f32,
}

impl FontFace {
    /// Load font index 0 from a `.ttf`/`.otf`/`.ttc` file.
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|source| {
            Error::Render(format!("failed to read font {}: {source}", path.display()))
        })?;
        Self::from_bytes(data).map_err(|e| match e {
            Error::Render(msg) => Error::Render(format!("{}: {}", path.display(), msg)),
            other => other,
        })
    }

    /// Parse an already-loaded font collection. Used by tests.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        // Pull out the cheap scalar metadata first so we can move `data` into
        // the struct afterwards without the borrow checker complaining that
        // `data` is still borrowed by `font`.
        let (offset, key, upem) = {
            let font = FontRef::from_index(&data, 0).ok_or_else(|| {
                Error::Render("data is not a TrueType/OpenType font or collection".into())
            })?;
            let upem = font.metrics(&[]).units_per_em as f32;
            if upem <= 0.0 {
                return Err(Error::Render("font reports zero units-per-em".into()));
            }
            (font.offset, font.key, upem)
        };
        Ok(Self {
            data,
            offset,
            key,
            upem,
        })
    }

    /// Units per em, as f32. Zero is impossible for a valid font.
    pub fn upem(&self) -> f32 {
        self.upem
    }

    /// Borrow a [`FontRef`]. Cheap; re-derived per call rather than cached on
    /// `self` so no self-reference is needed.
    fn font_ref(&self) -> FontRef<'_> {
        FontRef {
            data: &self.data,
            offset: self.offset,
            key: self.key,
        }
    }
}

/// Draw `text` onto `canvas` with its left edge at `x`, baseline at
/// `y_baseline`, in `color`. Returns the total advance in pixels.
///
/// Returns `0.0` for empty input and changes no pixels. With a zero
/// `px_size` the shaper still runs but produces zero-sized glyphs, which is
/// the safest no-op we can offer without inspecting the shaper's internals.
pub fn draw_text(
    canvas: &mut Pixmap,
    font: &FontFace,
    text: &str,
    x: f32,
    y_baseline: f32,
    px_size: f32,
    color: Color,
) -> f32 {
    if text.is_empty() || px_size <= 0.0 {
        return 0.0;
    }
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let mut shape_ctx = ShapeContext::new();
    let font_ref = font.font_ref();
    let mut shaper = shape_ctx.builder(font_ref).size(px_size).build();
    shaper.add_str(text);

    // Scale context caches renders of the same outline; reusing it across
    // glyphs is the difference between ~per-glyph-ms and ~per-glyph-µs.
    let mut scale_ctx = ScaleContext::new();
    let mut scaler = scale_ctx.builder(font_ref).size(px_size).build();

    let mut pen_x = x;
    let mut total_advance = 0.0_f32;
    shaper.shape_with(|cluster| {
        for glyph in cluster.glyphs {
            paint_glyph(
                canvas,
                &mut scaler,
                &paint,
                FillRule::Winding,
                glyph.id,
                pen_x + glyph.x,
                y_baseline - glyph.y,
            );
            pen_x += glyph.advance;
            total_advance += glyph.advance;
        }
    });
    total_advance
}

/// Convenience: draw candidate `idx` of a popup at its measured cell
/// position, vertically centred inside the canvas.
///
/// `label` is the short position marker ("1.", "2.", …) drawn before the
/// candidate text. Returns the advance of `text` only.
pub fn draw_cell(
    canvas: &mut Pixmap,
    font: &FontFace,
    m: PopupMetrics,
    cfg: &RenderConfig,
    idx: usize,
    label: &str,
    text: &str,
) -> f32 {
    let px_size = cfg.font_size as f32;
    // Place the baseline so the em-box (font_size px tall) is centred
    // vertically inside the canvas — the renderer in `paint.rs` has already
    // filled the cell, this only positions the text within it.
    let baseline = (m.height as f32 + px_size) * 0.5;
    let left = cell_x(&m, cfg, idx) as f32;
    let color = paint::color_or(&cfg.text, Color::WHITE);
    let label_advance = draw_text(canvas, font, label, left, baseline, px_size, color);
    let text_left = if label_advance > 0.0 {
        left + label_advance + cfg.label_gap as f32
    } else {
        left
    };
    draw_text(canvas, font, text, text_left, baseline, px_size, color)
}

fn paint_glyph(
    canvas: &mut Pixmap,
    scaler: &mut swash::scale::Scaler<'_>,
    paint: &Paint<'_>,
    fill_rule: FillRule,
    glyph_id: GlyphId,
    x: f32,
    y: f32,
) {
    let Some(outline) = scaler.scale_outline(glyph_id) else {
        return;
    };
    if outline.is_empty() {
        return;
    }
    let Some(path) = outline_to_path(&outline, x, y) else {
        return;
    };
    canvas.fill_path(&path, paint, fill_rule, Transform::identity(), None);
}

/// Translate a [`swash::scale::outline::Outline`] into a [`tiny_skia::Path`]
/// positioned at `(x, y)`.
fn outline_to_path(outline: &Outline, x: f32, y: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut idx = 0_usize;
    for verb in outline.verbs() {
        match *verb {
            Verb::MoveTo => {
                let p = outline.points()[idx];
                idx += 1;
                pb.move_to(p.x + x, y - p.y);
            }
            Verb::LineTo => {
                let p = outline.points()[idx];
                idx += 1;
                pb.line_to(p.x + x, y - p.y);
            }
            Verb::QuadTo => {
                let c = outline.points()[idx];
                let p = outline.points()[idx + 1];
                idx += 2;
                pb.quad_to(c.x + x, y - c.y, p.x + x, y - p.y);
            }
            Verb::CurveTo => {
                let c1 = outline.points()[idx];
                let c2 = outline.points()[idx + 1];
                let p = outline.points()[idx + 2];
                idx += 3;
                pb.cubic_to(c1.x + x, y - c1.y, c2.x + x, y - c2.y, p.x + x, y - p.y);
            }
            Verb::Close => {
                pb.close();
            }
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::render::paint;
    use std::sync::OnceLock;

    /// Cached copy of the Noto CJK TTC so we only parse it once per test run.
    /// `OnceLock` lives for the process, so repeated `load_file` would
    /// otherwise pay a ~20 ms parse for every test that touches the font.
    fn noto_bytes() -> &'static [u8] {
        static NOTO: OnceLock<Vec<u8>> = OnceLock::new();
        NOTO.get_or_init(|| {
            std::fs::read("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")
                .expect("Noto CJK font must be readable for tests")
        })
    }

    fn noto_face() -> FontFace {
        FontFace::from_bytes(noto_bytes().to_vec()).expect("Noto CJK parses cleanly")
    }

    fn cfg() -> RenderConfig {
        Config::default_built().render
    }

    fn cfg_with_size(px: u32) -> RenderConfig {
        let mut c = cfg();
        c.font_size = px;
        c
    }

    fn metrics() -> PopupMetrics {
        crate::render::layout::measure(&[], &["你好".into(), "世界".into()], &cfg())
    }

    #[test]
    fn load_real_font_reports_a_sane_upem() {
        let face = FontFace::load(Path::new(
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ))
        .expect("Noto CJK loads");
        // Noto CJK reports 1000 upem; we just need >0 to prove the head table
        // was parsed and the metrics call succeeded.
        assert!(face.upem() > 0.0);
        assert!(
            face.upem() >= 100.0,
            "upem {} is implausibly small",
            face.upem()
        );
    }

    #[test]
    fn load_missing_path_names_the_file_in_the_error() {
        let mut p = std::env::temp_dir();
        p.push("rime-popup-font-that-should-not-exist-1234567890.ttf");
        let err = FontFace::load(&p).expect_err("missing file must error");
        let msg = err.to_string();
        assert!(
            msg.contains("rime-popup-font-that-should-not-exist-1234567890.ttf"),
            "error must name the path; got {msg:?}"
        );
    }

    #[test]
    fn from_bytes_rejects_garbage_without_panicking() {
        let err = FontFace::from_bytes(b"not a font at all".to_vec())
            .expect_err("garbage bytes must error");
        // We don't constrain the wording, just that it surfaced as an Err.
        assert!(matches!(err, Error::Render(_)));
    }

    #[test]
    fn draw_text_changes_pixels_on_a_blank_canvas() {
        let face = noto_face();
        let cfg = cfg();
        let m = metrics();
        let mut canvas = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut canvas, &cfg);

        // Compare against the bg colour, not transparent: the canvas is fully
        // opaque after `clear_bg`, so any pixel that differs from the bg is
        // one the renderer actually drew into.
        let bg = paint::color_or(&cfg.bg, Color::BLACK);
        let before = paint::pixels_differing_from(&canvas, bg);
        let advance = draw_text(
            &mut canvas,
            &face,
            "你好",
            cfg.line_padding as f32,
            (m.height as f32 + cfg.font_size as f32) * 0.5,
            cfg.font_size as f32,
            Color::from_rgba8(0xe6, 0xe6, 0xe6, 0xff),
        );
        let after = paint::pixels_differing_from(&canvas, bg);

        assert!(
            after > before && after > 0,
            "drawing CJK must paint pixels (before={before}, after={after})"
        );
        assert!(advance > 0.0, "non-empty text must advance");
    }

    #[test]
    fn draw_text_empty_string_is_a_noop() {
        let face = noto_face();
        let cfg = cfg();
        let m = metrics();
        let mut canvas = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut canvas, &cfg);

        let bg = paint::color_or(&cfg.bg, Color::BLACK);
        let before = paint::pixels_differing_from(&canvas, bg);
        let advance = draw_text(
            &mut canvas,
            &face,
            "",
            0.0,
            m.height as f32 * 0.5,
            cfg.font_size as f32,
            Color::from_rgba8(0xff, 0xff, 0xff, 0xff),
        );
        let after = paint::pixels_differing_from(&canvas, bg);

        assert_eq!(advance, 0.0, "empty text must report zero advance");
        assert_eq!(after, before, "empty text must not touch the canvas");
    }

    #[test]
    fn draw_text_distinguishes_different_strings() {
        let face = noto_face();
        let cfg = cfg();
        let m = metrics();

        let mut a = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut a, &cfg);
        draw_text(
            &mut a,
            &face,
            "你好",
            cfg.line_padding as f32,
            (m.height as f32 + cfg.font_size as f32) * 0.5,
            cfg.font_size as f32,
            Color::from_rgba8(0xff, 0xff, 0xff, 0xff),
        );

        let mut b = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut b, &cfg);
        draw_text(
            &mut b,
            &face,
            "世界",
            cfg.line_padding as f32,
            (m.height as f32 + cfg.font_size as f32) * 0.5,
            cfg.font_size as f32,
            Color::from_rgba8(0xff, 0xff, 0xff, 0xff),
        );

        // Count pixels that differ from the bg colour, not all opaque pixels:
        // the text must visibly mark the canvas, but the marker is the diff
        // against the bg, not against transparent.
        let bg = paint::color_or(&cfg.bg, Color::BLACK);
        let ca = paint::pixels_differing_from(&a, bg);
        let cb = paint::pixels_differing_from(&b, bg);
        assert!(
            ca > 0 && cb > 0,
            "each string must paint something (a={ca}, b={cb})"
        );
        assert_ne!(
            ca, cb,
            "different CJK strings should produce different pixel counts (a={ca}, b={cb})"
        );
    }

    #[test]
    fn draw_cell_stays_inside_the_canvas() {
        let face = noto_face();
        let cfg = cfg();
        let m = metrics();
        let mut canvas = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut canvas, &cfg);

        // No panic and no wraparound — success is sufficient; the explicit
        // bounds check below adds a stronger invariant.
        let advance = draw_cell(&mut canvas, &face, m, &cfg, 1, "2.", "好");
        assert!(advance > 0.0, "draw_cell must report positive advance");

        // Out-of-bounds reads must return None: tiny-skia computes the byte
        // index as `width * y + x`, so probe a row past the bottom edge to
        // catch any wrap-around.
        assert!(canvas.pixel(0, m.height).is_none());

        // The drawn pixels must lie inside the cell for `idx` rather than
        // spilling into the left padding.
        let cell_left = cell_x(&m, &cfg, 1);
        assert!(cell_left > cfg.line_padding);
        // Sample a pixel in the left padding band — it must still be the
        // background colour.
        let pad_pixel = canvas
            .pixel(cfg.line_padding / 2 + 1, m.height / 2)
            .expect("padding pixel is inside the canvas");
        assert_eq!(
            pad_pixel,
            paint::color_or(&cfg.bg, Color::BLACK)
                .premultiply()
                .to_color_u8(),
            "left padding must remain background"
        );
    }

    #[test]
    fn draw_text_zero_size_does_not_panic() {
        let face = noto_face();
        let cfg = cfg();
        let m = metrics();
        let mut canvas = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut canvas, &cfg);

        let bg = paint::color_or(&cfg.bg, Color::BLACK);
        let before = paint::pixels_differing_from(&canvas, bg);
        let advance = draw_text(
            &mut canvas,
            &face,
            "你好",
            cfg.line_padding as f32,
            (m.height as f32) * 0.5,
            0.0,
            Color::from_rgba8(0xff, 0xff, 0xff, 0xff),
        );
        let after = paint::pixels_differing_from(&canvas, bg);

        assert_eq!(advance, 0.0, "size 0 must report zero advance");
        assert_eq!(after, before, "size 0 must not change pixels");
    }

    #[test]
    fn draw_cell_uses_configured_font_size() {
        let face = noto_face();
        let cfg = cfg_with_size(28);
        let m = crate::render::layout::measure(&[], &["你".into()], &cfg);
        let mut canvas = paint::make_canvas(m).expect("allocates");
        paint::clear_bg(&mut canvas, &cfg);

        let advance = draw_cell(&mut canvas, &face, m, &cfg, 0, "1.", "你");
        // One CJK char at 28px should advance roughly one em.
        assert!(
            advance > 20.0 && advance < 40.0,
            "advance {advance} outside plausible range for 28px CJK"
        );
        let bg = paint::color_or(&cfg.bg, Color::BLACK);
        assert!(
            paint::pixels_differing_from(&canvas, bg) > 0,
            "larger font must still mark pixels"
        );
    }
}
