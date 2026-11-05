use ratatui::style::{Color, Modifier, Style};
use ratatui_textarea::TextArea;

const fn hex(rgb: u32) -> Color {
    Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

// Catppuccin Mocha
pub const ROSEWATER: Color = hex(0xf5e0dc);
pub const PINK: Color = hex(0xf5c2e7);
pub const MAUVE: Color = hex(0xcba6f7);
pub const RED: Color = hex(0xf38ba8);
pub const PEACH: Color = hex(0xfab387);
pub const YELLOW: Color = hex(0xf9e2af);
pub const GREEN: Color = hex(0xa6e3a1);
pub const TEAL: Color = hex(0x94e2d5);
pub const SKY: Color = hex(0x89dceb);
pub const BLUE: Color = hex(0x89b4fa);
pub const LAVENDER: Color = hex(0xb4befe);
pub const TEXT: Color = hex(0xcdd6f4);
pub const SUBTEXT0: Color = hex(0xa6adc8);
pub const OVERLAY1: Color = hex(0x7f849c);
pub const OVERLAY0: Color = hex(0x6c7086);
pub const SURFACE1: Color = hex(0x45475a);
pub const SURFACE0: Color = hex(0x313244);
pub const BASE: Color = hex(0x1e1e2e);
pub const MANTLE: Color = hex(0x181825);
pub const CRUST: Color = hex(0x11111b);

const HUE_BANDS: [(f32, Color); 10] = [
    (15.0, RED),
    (40.0, PEACH),
    (70.0, YELLOW),
    (155.0, GREEN),
    (185.0, TEAL),
    (205.0, SKY),
    (228.0, BLUE),
    (250.0, LAVENDER),
    (290.0, MAUVE),
    (345.0, PINK),
];
const STATUS_GREYS: [Color; 3] = [TEXT, SUBTEXT0, OVERLAY1];

/// Linear lets teams pick any status color. Mocha is pastel, so match colored
/// statuses by hue band and grey ones by brightness rather than by RGB distance.
pub fn status_color(linear_hex: &str) -> Color {
    let digits = linear_hex.trim_start_matches('#');
    let rgb = match u32::from_str_radix(digits, 16) {
        Ok(rgb) if digits.len() == 6 => rgb,
        _ => return OVERLAY1,
    };
    let (hue, saturation, value) = hsv(hex(rgb));
    if saturation < 0.2 {
        return nearest(&STATUS_GREYS, |color| (hsv(color).2 - value).abs());
    }
    HUE_BANDS
        .iter()
        .find(|(upper, _)| hue < *upper)
        .map_or(RED, |(_, color)| *color)
}

fn nearest(palette: &[Color], distance: impl Fn(Color) -> f32) -> Color {
    palette
        .iter()
        .copied()
        .min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
        .unwrap_or(OVERLAY1)
}

fn hsv(color: Color) -> (f32, f32, f32) {
    let Color::Rgb(r, g, b) = color else {
        return (0.0, 0.0, 0.0);
    };
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let saturation = if max == 0.0 { 0.0 } else { delta / max };
    (hue, saturation, max)
}

pub fn editor(placeholder: &str) -> TextArea<'static> {
    let mut editor = TextArea::default();
    editor.set_style(Style::new().fg(TEXT));
    editor.set_cursor_line_style(Style::new());
    editor.set_cursor_style(Style::new().fg(BASE).bg(ROSEWATER));
    editor.set_selection_style(Style::new().bg(SURFACE1));
    editor.set_placeholder_style(Style::new().fg(OVERLAY0).add_modifier(Modifier::ITALIC));
    editor.set_placeholder_text(placeholder);
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_linear_status_colors_to_mocha() {
        assert_eq!(status_color("#f2c94c"), YELLOW);
        assert_eq!(status_color("#5e6ad2"), LAVENDER);
        assert_eq!(status_color("#bec2c8"), SUBTEXT0);
        assert_eq!(status_color("#e2e2e2"), TEXT);
        assert_eq!(status_color("#6b6f76"), OVERLAY1);
        assert_eq!(status_color("#26b5ce"), SKY);
        assert_eq!(status_color("#eb5757"), RED);
        assert_eq!(status_color("#0f783c"), GREEN);
        assert_eq!(status_color("#4cb782"), GREEN);
        assert_eq!(status_color("#fc7840"), PEACH);
        assert_eq!(status_color("#f43f5e"), RED);
        assert_eq!(status_color("not a color"), OVERLAY1);
    }
}
