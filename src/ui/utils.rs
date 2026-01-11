use crossterm::style::Color;

/// Format bytes
pub fn format_bytes(bytes: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    if bytes < KB {
        format!("{:.0} B", bytes)
    } else if bytes < MB {
        format!("{:.2} KiB", bytes / KB)
    } else if bytes < GB {
        format!("{:.2} MiB", bytes / MB)
    } else if bytes < TB {
        format!("{:.2} GiB", bytes / GB)
    } else {
        format!("{:.2} TiB", bytes / TB)
    }
}

/// Format duration
pub fn format_eta(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}

pub fn parse_hex_color(hex: &str) -> Color {
    if hex.starts_with('#') && hex.len() == 7 {
        let r = u8::from_str_radix(&hex[1..3], 16).unwrap_or(255);
        let g = u8::from_str_radix(&hex[3..5], 16).unwrap_or(255);
        let b = u8::from_str_radix(&hex[5..7], 16).unwrap_or(255);
        Color::Rgb { r, g, b }
    } else {
        match hex.to_lowercase().as_str() {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "reset" => Color::Reset,
            _ => Color::White,
        }
    }
}

pub fn interpolate_color(c1: Color, c2: Color, t: f32) -> Color {
    match (c1, c2) {
        (
            Color::Rgb {
                r: r1,
                g: g1,
                b: b1,
            },
            Color::Rgb {
                r: r2,
                g: g2,
                b: b2,
            },
        ) => {
            let r = (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8;
            let g = (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8;
            let b = (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8;
            Color::Rgb { r, g, b }
        }
        _ => c1,
    }
}

pub fn get_gradient_color(colors: &[String], progress: f32) -> Color {
    if colors.is_empty() {
        return Color::White;
    }
    if colors.len() == 1 {
        return parse_hex_color(&colors[0]);
    }

    let segments = colors.len() - 1;
    let segment_len = 1.0 / segments as f32;

    let segment_idx = (progress / segment_len).floor() as usize;
    let segment_idx = segment_idx.min(segments - 1);

    let t = (progress - (segment_idx as f32 * segment_len)) / segment_len;

    let c1 = parse_hex_color(&colors[segment_idx]);
    let c2 = parse_hex_color(&colors[segment_idx + 1]);

    interpolate_color(c1, c2, t)
}
