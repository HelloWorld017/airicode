use ratatui::style::{Color, Style};

pub const MESSAGE_MARGIN: u16 = 2;
pub const ITEM_GAP: u16 = 1;
pub const BOX_PADDING_X: u16 = 2;
pub const BOX_PADDING_TOP: u16 = 1;
pub const BOX_PADDING_BOTTOM: u16 = 1;
pub const COLLAPSE_AFTER: usize = 16;
pub const COLLAPSED_LINES: usize = 12;

pub fn primary_style() -> Style {
    Style::default().fg(Color::Gray)
}

pub fn secondary_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn alert_style() -> Style {
    Style::default().fg(Color::LightRed)
}

pub fn box_background() -> Color {
    Color::Rgb(34, 37, 44)
}

pub fn box_hover_background() -> Color {
    Color::Rgb(47, 51, 60)
}

pub fn alert_background() -> Color {
    Color::Rgb(58, 35, 40)
}

pub fn diff_background() -> Color {
    Color::Rgb(29, 39, 38)
}

pub fn diff_header_style() -> Style {
    Style::default().fg(Color::LightCyan)
}

pub fn diff_add_style() -> Style {
    Style::default().fg(Color::LightGreen)
}

pub fn diff_remove_style() -> Style {
    Style::default().fg(Color::LightRed)
}

pub fn diff_hunk_style() -> Style {
    Style::default().fg(Color::LightCyan)
}

pub fn mode_label(mode: &str) -> String {
    let mut characters = mode.chars();
    let Some(first) = characters.next() else {
        return "Mode".into();
    };
    first.to_uppercase().collect::<String>() + characters.as_str()
}

pub fn mode_color(mode: &str, modes: &[String]) -> Color {
    if mode.eq_ignore_ascii_case("build") {
        return Color::Blue;
    }
    if mode.eq_ignore_ascii_case("plan") {
        return Color::Yellow;
    }

    let mut names = modes
        .iter()
        .map(|mode| mode.to_ascii_lowercase())
        .filter(|mode| mode != "build" && mode != "plan")
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    let position = names
        .iter()
        .position(|name| name == &mode.to_ascii_lowercase())
        .unwrap_or(0);
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Green,
        Color::Red,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightGreen,
        Color::LightRed,
    ];
    PALETTE[position % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_palette_is_reserved_and_deterministic() {
        let modes = vec!["zeta".into(), "alpha".into(), "build".into(), "plan".into()];
        assert_eq!(mode_color("BUILD", &modes), Color::Blue);
        assert_eq!(mode_color("plan", &modes), Color::Yellow);
        assert_eq!(mode_color("alpha", &modes), Color::Cyan);
        assert_eq!(mode_color("zeta", &modes), Color::Magenta);
    }
}
