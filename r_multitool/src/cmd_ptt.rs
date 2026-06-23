use crate::utils;

fn parse_hex_color(hex: &str) -> String {
    match hex.len() {
        3 => format!("0xFF{0}{0}{1}{1}{2}{2}", &hex[0..1], &hex[1..2], &hex[2..3]),
        6 => format!("0xFF{}", hex),
        8 => format!("0x{}", hex),
        _ => format!("0x{}", hex),
    }
}

pub fn run(state: &str, custom_color: Option<String>) -> anyhow::Result<()> {
    let (vol, default_outline) = match state {
        "1" => ("0.4", "0xFFFFFFFF"),
        "0" => ("0.0", "0x00000000"),
        _ => return Ok(()),
    };
    let outline_color = match custom_color {
        Some(c) => parse_hex_color(&c),
        None => default_outline.to_string(),
    };
    utils::exec_silent("wpctl", &["set-volume", "@DEFAULT_AUDIO_SOURCE@", vol]);
    let command = utils::OverlayCommand {
        layer: None,
        timeout_ms: None,
        operations: vec![utils::DrawOperation::Rectangle {
            x1: 0,
            y1: 0,
            x2: 1920,
            y2: 1080,
            fill_color: "0x00000000".to_string(),
            outline_width: 5.0,
            outline_color,
        }],
    };
    utils::send_overlay_command(&command);
    Ok(())
}
