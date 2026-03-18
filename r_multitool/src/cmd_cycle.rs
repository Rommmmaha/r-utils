use crate::utils::{self, PwNode};
use std::collections::HashSet;
use std::process::{Command, Stdio};
fn get_physical_sink_names(nodes: &[PwNode]) -> Vec<String> {
    let mut sinks = Vec::new();
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
                        sinks.push(name.clone());
                    }
                }
            }
        }
    }
    sinks.sort();
    sinks
}
fn get_active_links(source: &str, valid_targets: &[String]) -> HashSet<String> {
    let output = utils::exec_output("pw-link", &["-l"]).unwrap_or_default();
    let mut active = HashSet::new();
    let mut current_section = String::new();
    let prefix = format!("{}:", source);
    for line in output.lines() {
        let trimmed = line.trim();
        if !line.starts_with(|c: char| c.is_whitespace()) {
            current_section = trimmed.to_string();
            continue;
        }
        if current_section.starts_with(&prefix) && line.contains("->") {
            if let Some(part) = line.split("->").nth(1) {
                let target_port = part.trim();
                let target_node = target_port.split(':').next().unwrap_or("");
                if valid_targets.contains(&target_node.to_string()) {
                    active.insert(target_node.to_string());
                }
            }
        }
    }
    active
}
fn set_link(source: &str, sink: &str, unlink: bool) {
    let mut args_fl = Vec::new();
    if unlink {
        args_fl.push("-d".to_string());
    }
    args_fl.push(format!("{}:monitor_FL", source));
    args_fl.push(format!("{}:playback_FL", sink));
    let mut args_fr = Vec::new();
    if unlink {
        args_fr.push("-d".to_string());
    }
    args_fr.push(format!("{}:monitor_FR", source));
    args_fr.push(format!("{}:playback_FR", sink));
    let _ = Command::new("pw-link")
        .args(&args_fl)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let _ = Command::new("pw-link")
        .args(&args_fr)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
pub fn run() {
    let nodes = match utils::get_all_sinks() {
        Some(n) => n,
        None => return,
    };
    let physical = get_physical_sink_names(&nodes);
    if physical.is_empty() {
        return;
    }
    let active_links = get_active_links(utils::VIRTUAL_SINK_TO_CYCLE, &physical);
    let mut next_index = 0;
    if !active_links.is_empty() {
        let last_active = active_links
            .iter()
            .max_by_key(|name| physical.iter().position(|r| r == *name))
            .unwrap();
        if let Some(idx) = physical.iter().position(|r| r == last_active) {
            next_index = (idx + 1) % physical.len();
        }
    }
    let next_sink = &physical[next_index];
    for old in active_links {
        if &old != next_sink {
            set_link(utils::VIRTUAL_SINK_TO_CYCLE, &old, true);
        }
    }
    set_link(utils::VIRTUAL_SINK_TO_CYCLE, next_sink, false);
    utils::exec_with_stdin("pw-play", &["-"], utils::NOTIFY_WAV);
}
