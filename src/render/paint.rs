//! Canvas paint pipeline: raster shapes with tiny-skia into an RGBA8 pixmap.
//!
//! Glyph rasterization lives in `crate::render::text`; this module owns the
//! canvas and the rectangle work (background, highlight, border) that has to
//! be correct before a single glyph is drawn.

use tiny_skia::{Color, Paint, Pixmap, Rect, Stroke, Transform};

use crate::config::RenderConfig;
use crate::errors::{Error, Result};
use crate::render::layout::{cell_x, PopupMetrics};

/// Parse `#rrggbb` or `#rrggbbaa` into a colour.
///
/// Returns `None` for anything else — a short string, a missing `#`, a
/// non-hex digit, or a length that is neither 6 nor 8. The plan's original
/// version indexed the hex string with unchecked ranges and panicked on
/// short input such as `"#abc"`; parsing is fallible here and callers pick
/// their own default.
pub fn parse_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) {
        return None;
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    // Length is already validated to 6 or 8, so every slice here is in bounds
    // and well formed; `?` on the parse can only fail on a non-hex digit,
    // which the `all` above already excluded.
    let r = byte(0)?;
    let g = byte(2)?;
    let b = byte(4)?;
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some(Color::from_rgba8(r, g, b, a))
}

/// `parse_color` with a caller-supplied fallback for invalid input.
pub fn color_or(s: &str, fallback: Color) -> Color {
    parse_color(s).unwrap_or(fallback)
}

/// Allocate a transparent canvas the size of the popup.
///
/// A zero-width or zero-height pixmap cannot be allocated, and the popup is
/// sized from user input, so this is fallible rather than panicking.
pub fn make_canvas(m: PopupMetrics) -> Result<Pixmap> {
    Pixmap::new(m.width.max(1), m.height.max(1)).ok_or_else(|| {
        Error::Render(format!(
            "cannot allocate {:?} canvas",
            (m.width.max(1), m.height.max(1))
        ))
    })
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

/// Fill the whole canvas with the configured background colour.
pub fn clear_bg(canvas: &mut Pixmap, cfg: &RenderConfig) {
    canvas.fill(color_or(&cfg.bg, Color::BLACK));
}

/// Paint the highlighted background behind candidate `idx`.
///
/// No-ops when `total` is zero, and clamps rather than overflowing when `idx`
/// is out of range, because `idx` arrives from fcitx5 over Dbus.
pub fn draw_highlight(
    canvas: &mut Pixmap,
    cfg: &RenderConfig,
    m: PopupMetrics,
    idx: usize,
    total: usize,
) {
    if total == 0 || idx >= total {
        return;
    }
    let x = cell_x(&m, cfg, idx) as f32;
    let w = m.cell_w as f32;
    let h = m.height as f32;
    // The highlight fills the full popup height so the cell reads as one
    // continuous bar rather than a box floating inside it.
    if let Some(rect) = Rect::from_xywh(x, 0.0, w, h) {
        canvas.fill_rect(
            rect,
            &paint(color_or(&cfg.highlight_bg, Color::BLACK)),
            Transform::identity(),
            None,
        );
    }
}

/// Stroke a border of `border_width` pixels around the canvas edge.
///
/// A zero width, or a width wider than the canvas, draws nothing rather than
/// producing a degenerate stroke.
pub fn draw_border(canvas: &mut Pixmap, cfg: &RenderConfig) {
    let w = cfg.border_width;
    if w == 0 || w >= canvas.width() || w >= canvas.height() {
        return;
    }
    let stroke = Stroke {
        width: w as f32,
        ..Stroke::default()
    };
    let inset = (w / 2) as f32;
    let rect = Rect::from_xywh(
        inset,
        inset,
        canvas.width() as f32 - w as f32,
        canvas.height() as f32 - w as f32,
    );
    if let Some(rect) = rect {
        canvas.stroke_path(
            &tiny_skia::PathBuilder::from_rect(rect),
            &paint(color_or(&cfg.border, Color::WHITE)),
            &stroke,
            Transform::identity(),
            None,
        );
    }
}

/// Count pixels that are not exactly `color`.
///
/// Used by tests to assert that drawing actually marked the canvas. The
/// plan's original assertion (`red() != 255 || green() != 255 || blue() !=
/// 255`) is satisfied by a blank transparent canvas and therefore passed even
/// when nothing was drawn at all.
pub fn pixels_differing_from(canvas: &Pixmap, color: Color) -> usize {
    let want = color.premultiply().to_color_u8();
    canvas.pixels().iter().filter(|p| **p != want).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg() -> RenderConfig {
        Config::default_built().render
    }

    fn metrics() -> PopupMetrics {
        crate::render::layout::measure(&["你好".into(), "世界".into()], &cfg())
    }

    #[test]
    fn parse_color_reads_six_digits() {
        let c = parse_color("#3a3a3a").expect("valid");
        assert_eq!(
            (
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8,
                (c.alpha() * 255.0) as u8
            ),
            (0x3a, 0x3a, 0x3a, 0xff)
        );
    }

    #[test]
    fn parse_color_reads_eight_digits() {
        let c = parse_color("#11223380").expect("valid");
        assert_eq!((c.alpha() * 255.0) as u8, 0x80);
    }

    /// The plan's parser indexed `hex[i..i+2]` without bounds checking and
    /// panicked on every short input.
    #[test]
    fn parse_color_rejects_short_input_without_panicking() {
        for bad in ["#", "#a", "#ab", "#abc", "#abcd", "#abcde", ""] {
            assert!(parse_color(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn parse_color_rejects_malformed() {
        for bad in ["3a3a3a", "#gggggg", "#3a3a3a3a3a", "#  3a3a"] {
            assert!(parse_color(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn parse_color_tolerates_surrounding_whitespace() {
        assert!(parse_color("  #3a3a3a\n").is_some());
    }

    #[test]
    fn color_or_falls_back_on_bad_input() {
        assert_eq!(color_or("nope", Color::BLACK), Color::BLACK);
    }

    #[test]
    fn canvas_has_configured_size() {
        let m = metrics();
        let c = make_canvas(m).expect("allocates");
        assert_eq!(c.width(), m.width);
        assert_eq!(c.height(), m.height);
    }

    #[test]
    fn zero_sized_metrics_still_allocate() {
        let m = PopupMetrics {
            width: 0,
            height: 0,
            char_w: 0,
            cell_w: 0,
        };
        let c = make_canvas(m).expect("allocates");
        assert_eq!((c.width(), c.height()), (1, 1));
    }

    #[test]
    fn clear_bg_paints_every_pixel() {
        let mut c = make_canvas(metrics()).expect("allocates");
        clear_bg(&mut c, &cfg());
        assert_eq!(
            pixels_differing_from(&c, Color::from_rgba8(0x1e, 0x1e, 0x1e, 0xff)),
            0,
            "background fill must cover the whole canvas"
        );
    }

    #[test]
    fn highlight_changes_only_its_own_cell() {
        let cfg = cfg();
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);

        let bg = color_or(&cfg.bg, Color::BLACK);
        let before = pixels_differing_from(&c, bg);
        draw_highlight(&mut c, &cfg, m, 1, 2);
        let after = pixels_differing_from(&c, bg);

        assert!(
            after > before && after > 0,
            "highlight must paint pixels (before={before} after={after})"
        );
        // Cell 1 is the right half of a two-candidate popup; it must not have
        // spilled across the padding on the left.
        let left_bg_x = cfg.line_padding / 2;
        assert_eq!(
            c.pixel(left_bg_x, m.height / 2).expect("in bounds"),
            bg.premultiply().to_color_u8(),
            "left padding must stay background"
        );
    }

    #[test]
    fn highlight_out_of_range_is_a_noop() {
        let cfg = cfg();
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);
        let bg = color_or(&cfg.bg, Color::BLACK);

        draw_highlight(&mut c, &cfg, m, 9, 2);
        assert_eq!(
            pixels_differing_from(&c, bg),
            0,
            "idx past the end must draw nothing"
        );
        draw_highlight(&mut c, &cfg, m, 0, 0);
        assert_eq!(
            pixels_differing_from(&c, bg),
            0,
            "zero candidates must draw nothing"
        );
    }

    #[test]
    fn border_marks_the_edges() {
        let cfg = cfg();
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);
        let bg = color_or(&cfg.bg, Color::BLACK);
        draw_border(&mut c, &cfg);
        assert!(
            pixels_differing_from(&c, bg) > 0,
            "a 1px border must mark at least one pixel"
        );
    }

    #[test]
    fn zero_border_width_draws_nothing() {
        let mut cfg = cfg();
        cfg.border_width = 0;
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);
        let bg = color_or(&cfg.bg, Color::BLACK);
        draw_border(&mut c, &cfg);
        assert_eq!(pixels_differing_from(&c, bg), 0);
    }

    #[test]
    fn border_wider_than_canvas_draws_nothing() {
        let mut cfg = cfg();
        cfg.border_width = 10_000;
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);
        let bg = color_or(&cfg.bg, Color::BLACK);
        draw_border(&mut c, &cfg);
        assert_eq!(pixels_differing_from(&c, bg), 0);
    }

    #[test]
    fn invalid_config_colours_do_not_panic() {
        let mut cfg = cfg();
        cfg.bg = "not a colour".into();
        cfg.highlight_bg = String::new();
        cfg.border = "#zz".into();
        let m = metrics();
        let mut c = make_canvas(m).expect("allocates");
        clear_bg(&mut c, &cfg);
        draw_highlight(&mut c, &cfg, m, 0, 1);
        draw_border(&mut c, &cfg);
    }
}
