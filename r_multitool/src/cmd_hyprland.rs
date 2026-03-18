use crate::utils;
use serde::Deserialize;
#[derive(Deserialize)]
struct HyprWorkspace {
    id: i32,
}
pub fn run(target_str: &str) {
    let target_id = match target_str.parse::<i32>() {
        Ok(i) => i,
        Err(_) => return,
    };
    let json = utils::exec_output("hyprctl", &["activeworkspace", "-j"]);
    if let Some(j) = json {
        if let Ok(ws) = serde_json::from_str::<HyprWorkspace>(&j) {
            if ws.id == target_id {
                utils::exec_silent("hyprctl", &["dispatch", "workspace", "previous"]);
            } else {
                utils::exec_silent("hyprctl", &["dispatch", "workspace", target_str]);
            }
        }
    }
}
