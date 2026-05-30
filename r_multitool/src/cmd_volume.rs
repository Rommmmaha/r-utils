use crate::utils::{self, PwNode};
use anyhow::Context;
fn get_physical_sink_ids(nodes: &[PwNode]) -> Vec<u32> {
    let mut ids = Vec::new();
    for node in nodes {
        if let Some(info) = &node.info {
            if let Some(props) = &info.props {
                if props.media_class.as_deref() != Some("Audio/Sink") {
                    continue;
                }
                if props.factory_name.as_deref() == Some("support.null-audio-sink") {
                    continue;
                }
                if let Some(name) = &props.node_name {
                    if !utils::IGNORED_SINKS.contains(&name.as_str()) {
                        ids.push(node.id);
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}
pub fn run(arg: &str) -> anyhow::Result<()> {
    let arg = arg.trim();
    let operator = if arg.ends_with('+') {
        "+"
    } else if arg.ends_with('-') {
        "-"
    } else {
        ""
    };
    let without_op = if operator.is_empty() {
        arg
    } else {
        &arg[..arg.len() - 1]
    };
    let without_pct = without_op.strip_suffix('%').unwrap_or(without_op);
    let percent: f32 = without_pct
        .parse()
        .context("Failed to parse volume amount")?;
    let delta = percent / 100.0;
    let nodes = utils::get_all_sinks().context("Failed to get sinks")?;
    let ids = get_physical_sink_ids(&nodes);
    if ids.is_empty() {
        anyhow::bail!("No physical sinks found");
    }
    let primary = ids[0];
    let out = utils::exec_output("wpctl", &["get-volume", &primary.to_string()])
        .context("Failed to get current volume")?;
    let current_vol = out
        .split_whitespace()
        .filter_map(|s| s.parse::<f32>().ok())
        .next()
        .context("Failed to parse current volume from wpctl output")?;
    let new_vol = match operator {
        "+" => current_vol + delta,
        "-" => current_vol - delta,
        _ => delta,
    }
    .max(0.0)
    .min(1.5);
    let vol_str = format!("{:.2}", new_vol);
    for id in ids {
        utils::exec_silent("wpctl", &["set-volume", &id.to_string(), &vol_str]);
    }
    let display_vol = new_vol.min(1.0);
    let bar_height = (1080.0 * display_vol) as i32;
    let mut operations = vec![
        utils::DrawOperation::Rectangle {
            x1: 0,
            y1: 1080 - bar_height,
            x2: 10,
            y2: 1080,
            fill_color: "0xFFFFFFFF".to_string(),
            outline_width: 0.0,
            outline_color: "0x00000000".to_string(),
        },
        utils::DrawOperation::Rectangle {
            x1: 1910,
            y1: 1080 - bar_height,
            x2: 1920,
            y2: 1080,
            fill_color: "0xFFFFFFFF".to_string(),
            outline_width: 0.0,
            outline_color: "0x00000000".to_string(),
        },
    ];
    let segment_height = 1080 / 5;
    for i in 1..5 {
        let y = i * segment_height;
        operations.push(utils::DrawOperation::Line {
            x1: 0,
            y1: y,
            x2: 1920,
            y2: y,
            width: 1.0,
            side: utils::LineSide::Center,
            color: "0xFFFFFFFF".to_string(),
        });
    }
    let command = utils::OverlayCommand {
        layer: Some(1),
        timeout_ms: Some(1000),
        operations,
    };
    utils::send_overlay_command(&command);
    Ok(())
}
