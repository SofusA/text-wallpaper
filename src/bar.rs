use chrono::Local;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const HEIGHT: u32 = 20;

pub fn duration_until_next_minute() -> Duration {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let remaining = 60 - elapsed.as_secs() % 60;

    Duration::from_secs(remaining)
}

pub fn draw(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    font_system: &mut FontSystem,
    cache: &mut SwashCache,
    font_family: &str,
) {
    let bar_height = HEIGHT.min(height);

    let text = Local::now().format("%d %b %Y | %H:%M").to_string();
    let metrics = Metrics::new(14.0, 14.0);
    let mut buffer = Buffer::new(font_system, metrics);

    let mut borrowed = buffer.borrow_with(font_system);

    borrowed.set_size(Some(width as f32), Some(height as f32));
    borrowed.set_wrap(Wrap::None);
    borrowed.set_text(
        &text,
        &Attrs::new().family(if font_family.eq_ignore_ascii_case("monospace") {
            Family::Monospace
        } else {
            Family::Name(font_family)
        }),
        Shaping::Advanced,
        None,
    );
    borrowed.shape_until_scroll(true);

    let content_width = buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0_f32, f32::max);

    let right_padding = 8.0;
    let top_padding = 4;

    let x_offset = (width as f32 - content_width - right_padding).max(0.0) as i32;
    let y_offset = top_padding;

    buffer.borrow_with(font_system).draw(
        cache,
        Color::rgb(255, 255, 255),
        |x, y, glyph_width, glyph_height, color| {
            let alpha = color.a() as u16;
            let red = (color.r() as u16 * alpha / 255) as u8;
            let green = (color.g() as u16 * alpha / 255) as u8;
            let blue = (color.b() as u16 * alpha / 255) as u8;

            for row in 0..glyph_height as i32 {
                for column in 0..glyph_width as i32 {
                    let px = x + x_offset + column;
                    let py = y + y_offset + row;

                    if px < 0 || py < 0 || px >= width as i32 || py >= bar_height as i32 {
                        continue;
                    }

                    let offset = py as usize * stride as usize + px as usize * 4;

                    canvas[offset] = blue;
                    canvas[offset + 1] = green;
                    canvas[offset + 2] = red;
                    canvas[offset + 3] = color.a();
                }
            }
        },
    );
}
