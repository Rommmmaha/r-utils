use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::UdpSocket;
use std::process::{Command, Stdio};
pub const VIRTUAL_SINK_TO_CYCLE: &str = "X1";
pub const IGNORED_SINKS: &[&str] = &["X1", "X2"];
pub const PTT_UDP_ADDR: &str = "127.0.0.1:7435";
pub const MIC_OPEN_VOL: &str = "0.4";
const MIC_OPEN_LVL: f32 = 0.4;
const VOL_EPS: f32 = 0.01;
/// First holder wins; the guard must stay alive to keep the lock.
pub fn take_singleton(name: &str) -> Option<nix::fcntl::Flock<std::fs::File>> {
    let path = format!("/tmp/{name}.lock");
    let f = std::fs::File::create(&path).ok()?;
    nix::fcntl::Flock::lock(f, nix::fcntl::FlockArg::LockExclusiveNonblock).ok()
}
pub const SESSION_PID_FILE: &str = "/tmp/r_multitool_rec.pid";
/// Claim the recorder session: lock + advertise pid for `mute` to signal.
/// Stale pid files are harmless (mute verifies cmdline, ESRCH-safe).
pub fn claim_session() -> Option<nix::fcntl::Flock<std::fs::File>> {
    let lock = take_singleton("r_multitool_rec")?;
    let _ = std::fs::write(SESSION_PID_FILE, std::process::id().to_string());
    Some(lock)
}
pub const NOTIFY_WAV: &[u8] = include_bytes!("../assets/notify.wav");
#[derive(Serialize)]
pub struct OverlayCommand {
    pub layer: Option<i32>,
    pub timeout_ms: Option<u64>,
    pub operations: Vec<DrawOperation>,
}
#[derive(Serialize)]
#[serde(tag = "type")]
pub enum DrawOperation {
    Rectangle {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        fill_color: String,
        outline_width: f32,
        outline_color: String,
    },
    Line {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        width: f32,
        side: LineSide,
        color: String,
    },
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
pub fn mic_volume() -> Option<f32> {
    let out = exec_output("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"])?;
    // "Volume: 0.40" (optionally followed by " [MUTED]")
    out.split_whitespace().nth(1)?.parse().ok()
}
pub fn set_mic_volume(vol: &str) {
    exec_silent("wpctl", &["set-volume", "@DEFAULT_AUDIO_SOURCE@", vol]);
}
/// SIGTERM a spawned child (timeout(1) forwards it) and reap it.
pub async fn stop_child(child: &mut tokio::process::Child) {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    if let Some(id) = child.id() {
        let _ = kill(Pid::from_raw(id as i32), Signal::SIGTERM);
    }
    let _ = child.wait().await;
}
/// Resolves once the mic leaves the recording level after having reached it.
/// The latch avoids tripping before our own unmute has applied. No baseline
/// sampling, so there is nothing to race with a mute still in flight.
pub async fn wait_recording_end() {
    let mut armed = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if let Some(v) = mic_volume() {
            if (v - MIC_OPEN_LVL).abs() <= VOL_EPS {
                armed = true;
            } else if armed {
                return;
            }
        }
    }
}
