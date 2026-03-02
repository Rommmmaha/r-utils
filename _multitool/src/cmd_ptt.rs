use crate::utils;
pub fn run(state: &str) {
    let (vol, outline_color) = match state {
        "1" => ("0.4", "0xFFFFFFFF"),
        "0" => ("0.0", "0x00000000"),
        _ => return,
    };
    utils::exec_silent("wpctl", &["set-volume", "@DEFAULT_AUDIO_SOURCE@", vol]);
    let command = utils::OverlayCommand {
        layer: None,
        timeout_ms: None,
        operations: vec![utils::DrawOperation::Rectangle(utils::RectangleParams {
            x1: 0,
            y1: 0,
            x2: 1920,
            y2: 1080,
            fill_color: "0x00000000".to_string(),
            outline_width: 5.0,
            outline_color: outline_color.to_string(),
        })],
    };
    utils::send_overlay_command(&command);
}
