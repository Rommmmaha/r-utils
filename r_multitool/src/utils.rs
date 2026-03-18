use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::UdpSocket;
use std::process::{Command, Stdio};
pub const VIRTUAL_SINK_TO_CYCLE: &str = "X1";
pub const IGNORED_SINKS: &[&str] = &["X1", "X2"];
pub const PTT_UDP_ADDR: &str = "127.0.0.1:7435";
pub const NOTIFY_WAV: &[u8] = include_bytes!("../assets/notify.wav");
#[derive(Serialize)]
pub struct OverlayCommand {
    pub layer: Option<i32>,
    pub timeout_ms: Option<u64>,
    pub operations: Vec<DrawOperation>,
}
#[derive(Serialize)]
pub enum DrawOperation {
    Rectangle(RectangleParams),
    Line(LineParams),
}
#[derive(Serialize)]
pub struct RectangleParams {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub fill_color: String,
    pub outline_width: f32,
    pub outline_color: String,
}
#[derive(Serialize)]
pub struct LineParams {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub width: f32,
    pub side: LineSide,
    pub color: String,
}
#[derive(Serialize)]
#[allow(dead_code)]
pub enum LineSide {
    Left,
    Right,
    Center,
}
#[derive(Deserialize)]
pub struct PwNode {
    pub id: u32,
    pub info: Option<PwInfo>,
}
#[derive(Deserialize)]
pub struct PwInfo {
    pub props: Option<PwProps>,
}
#[derive(Deserialize)]
pub struct PwProps {
    #[serde(rename = "media.class")]
    pub media_class: Option<String>,
    #[serde(rename = "node.name")]
    pub node_name: Option<String>,
    #[serde(rename = "factory.name")]
    pub factory_name: Option<String>,
}
pub fn exec_silent(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
pub fn exec_with_stdin(cmd: &str, args: &[&str], data: &[u8]) {
    if let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }
    }
}
pub fn exec_output(cmd: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(cmd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}
pub fn get_all_sinks() -> Option<Vec<PwNode>> {
    let json_str = exec_output("pw-dump", &[])?;
    serde_json::from_str(&json_str).ok()
}
pub fn send_overlay_command(command: &OverlayCommand) {
    if let Ok(json) = serde_json::to_string(command) {
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            let _ = socket.send_to(json.as_bytes(), PTT_UDP_ADDR);
        }
    }
}
