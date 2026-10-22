//! End-to-end check of the paint pipeline with real CJK glyphs.
//!
//! The unit tests cover layout, text and painting separately against the
//! in-memory canvas. This exercises the path the daemon actually takes at
//! runtime — load a font from disk, measure a candidate list, allocate a
//! shm-sized canvas, clear it, draw — and asserts on the *result* rather than
//! printing it, so a regression fails the build instead of scrolling past.
//!
//! The font path is fixed because the test needs CJK coverage: a Latin-only
//! face would let a broken glyph lookup pass unnoticed. Where the font is
//! absent the test skips rather than fails, so the suite stays usable on a
//! machine without it.

use rime_popup::config::Config;
use rime_popup::render::layout;
use rime_popup::render::paint;
use rime_popup::render::text::{draw_cell, draw_text, FontFace};
use tiny_skia::Color;

const CJK_FONT: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";

fn font() -> Option<FontFace> {
    match FontFace::load(std::path::Path::new(CJK_FONT)) {
        Ok(f) => Some(f),
        Err(_) => {
            eprintln!("SKIP: {CJK_FONT} is not present on this machine");
            None
        }
    }
}

#[test]
fn cjk_candidates_are_measured_larger_than_empty() {
    let cfg = Config::default_built().render;
    let empty = layout::measure(&[], &[], &cfg);
    let two = layout::measure(&[], &["你好".into(), "世界".into()], &cfg);

    assert!(
        two.width > empty.width,
        "two CJK candidates ({}) must be wider than none ({})",
        two.width,
        empty.width
    );
    assert!(two.height > 0, "a candidate row has height");
}

#[test]
fn position_labels_mark_the_canvas_before_the_text() {
    let Some(face) = font() else { return };
    let cfg = Config::default_built().render;
    let labels = vec!["1.".into(), "2.".into()];
    let candidates = vec!["你".into(), "泥".into()];
    let m = layout::measure(&labels, &candidates, &cfg);
    let mut canvas = paint::make_canvas(m).expect("canvas");
    paint::clear_bg(&mut canvas, &cfg);
    let bg = paint::color_or(&cfg.bg, Color::BLACK);

    for (idx, (label, text)) in labels.iter().zip(&candidates).enumerate() {
        let label = label.clone();
        let text = text.clone();
        draw_cell(&mut canvas, &face, m, &cfg, idx, &label, &text);
    }

    assert!(
        paint::pixels_differing_from(&canvas, bg) > 0,
        "labels and candidates must both reach the canvas"
    );
    assert!(
        m.cell_w > layout::measure(&[], &candidates, &cfg).cell_w,
        "labelled cells must be wider than unlabelled ones"
    );
}

#[test]
fn cjk_measure_fits_inside_its_own_canvas() {
    // A mismatch here means the canvas is allocated smaller than the text we
    // are about to draw into it — the popup would clip its own candidates.
    let cfg = Config::default_built().render;
    let candidates = vec!["你好".into(), "泥".into(), "拟".into()];
    let m = layout::measure(&[], &candidates, &cfg);
    let canvas = paint::make_canvas(m).expect("canvas allocation");
    assert!(
        canvas.width() >= m.width && canvas.height() >= m.height,
        "canvas {}x{} smaller than metrics {}x{}",
        canvas.width(),
        canvas.height(),
        m.width,
        m.height
    );
}

#[test]
fn drawing_cjk_changes_pixels_and_advances_the_cursor() {
    let Some(face) = font() else { return };
    let cfg = Config::default_built().render;
    let candidates = vec!["你好".into(), "世界".into()];
    let m = layout::measure(&[], &candidates, &cfg);

    let mut canvas = paint::make_canvas(m).expect("canvas");
    paint::clear_bg(&mut canvas, &cfg);
    let bg = paint::color_or(&cfg.bg, Color::BLACK);
    let before = paint::pixels_differing_from(&canvas, bg);
    assert_eq!(before, 0, "a cleared canvas must be uniform");

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
    assert!(after > 0, "drawing text must mark pixels");
    assert!(advance > 0.0, "two glyphs must advance the cursor");
    assert!(
        after > before,
        "painting must leave more marks than the cleared canvas"
    );
}

#[test]
fn font_reports_a_plausible_upem() {
    let Some(face) = font() else { return };
    let upem = face.upem();
    assert!(
        (16.0..=4096.0).contains(&upem),
        "implausible upem {upem}; a bad units-per-em breaks every glyph size"
    );
}
