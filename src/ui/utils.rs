use crossterm::style::Color;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes_zero() {
        assert_eq!(format_bytes(0.0), "0 B");
    }

    #[test]
    fn test_format_bytes_bytes() {
        assert_eq!(format_bytes(512.0), "512 B");
    }

    #[test]
    fn test_format_bytes_kib() {
        assert_eq!(format_bytes(2048.0), "2.00 KiB");
    }

    #[test]
    fn test_format_bytes_mib() {
        assert_eq!(format_bytes(5.0 * 1024.0 * 1024.0), "5.00 MiB");
    }

    #[test]
    fn test_format_bytes_gib() {
        assert_eq!(format_bytes(2.5 * 1024.0 * 1024.0 * 1024.0), "2.50 GiB");
    }

    #[test]
    fn test_format_eta_seconds_only() {
        assert_eq!(format_eta(45), "00:45");
    }

    #[test]
    fn test_format_eta_minutes() {
        assert_eq!(format_eta(125), "02:05");
    }

    #[test]
    fn test_format_eta_hours() {
        assert_eq!(format_eta(3661), "01:01:01");
    }

    #[test]
    fn test_format_eta_zero() {
        assert_eq!(format_eta(0), "00:00");
    }

    #[test]
    fn test_parse_hex_color_valid() {
        match parse_hex_color("#FF0000") {
            Color::Rgb { r, g, b } => {
                assert_eq!(r, 255);
                assert_eq!(g, 0);
                assert_eq!(b, 0);
            }
            _ => panic!("Expected RGB color"),
        }
    }

    #[test]
    fn test_parse_hex_color_named() {
        assert!(matches!(parse_hex_color("red"), Color::Red));
        assert!(matches!(parse_hex_color("green"), Color::Green));
        assert!(matches!(parse_hex_color("reset"), Color::Reset));
    }

    #[test]
    fn test_parse_hex_color_unknown() {
        assert!(matches!(parse_hex_color("invalid"), Color::White));
    }

    #[test]
    fn test_get_gradient_color_empty() {
        assert!(matches!(get_gradient_color(&[], 0.5), Color::White));
    }

    #[test]
    fn test_get_gradient_color_single() {
        let colors = vec!["#FF0000".to_string()];
        match get_gradient_color(&colors, 0.5) {
            Color::Rgb { r, g, b } => {
                assert_eq!(r, 255);
                assert_eq!(g, 0);
                assert_eq!(b, 0);
            }
            _ => panic!("Expected RGB color"),
        }
    }

    #[test]
    fn test_get_gradient_color_two_colors_midpoint() {
        let colors = vec!["#000000".to_string(), "#FFFFFF".to_string()];
        match get_gradient_color(&colors, 0.5) {
            Color::Rgb { r, g, b } => {
                assert!(r > 100 && r < 200);
                assert_eq!(r, g);
                assert_eq!(g, b);
            }
            _ => panic!("Expected RGB color"),
        }
    }
}
