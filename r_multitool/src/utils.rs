use serde::Deserialize;
use smithay_client_toolkit::shm::slot::SlotPool;
use std::io::Write;
use std::process::{Command, Stdio};
use tiny_skia::{Paint, Path, PathBuilder, Pixmap};
use wayland_client::protocol::{wl_shm, wl_surface};
pub const VIRTUAL_SINK_TO_CYCLE: &str = "X1";
pub const IGNORED_SINKS: &[&str] = &["X1", "X2"];
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
// --- shared pill drawing (tiny-skia layershell OSDs) ---
pub const KAPPA: f32 = 0.5522847498;
pub fn stadium(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + KAPPA * r, y, x + w, y + r - KAPPA * r, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + KAPPA * r, x + w - r + KAPPA * r, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - KAPPA * r, y + h, x, y + h - r + KAPPA * r, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - KAPPA * r, x + r - KAPPA * r, y, x + r, y);
    pb.close();
    pb.finish()
}
pub fn paint_rgba(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}
/// Blits a pixmap into the layer surface (RGBA -> ARGB shuffle).
pub fn upload_rgba(
    pixmap: &Pixmap,
    pool: &mut Option<SlotPool>,
    surface: &Option<wl_surface::WlSurface>,
) {
    let Some(surface) = surface else { return };
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let stride = width * 4;
    let Some(pool) = pool else { return };
    let size = (stride * height) as usize;
    if pool.len() < size && pool.resize(size).is_err() {
        return;
    }
    if let Ok((buffer, canvas)) = pool.create_buffer(width, height, stride, wl_shm::Format::Argb8888) {
        for (pixel, out) in pixmap.data().chunks_exact(4).zip(canvas.chunks_exact_mut(4)) {
            out[0] = pixel[2];
            out[1] = pixel[1];
            out[2] = pixel[0];
            out[3] = pixel[3];
        }
        surface.attach(Some(buffer.wl_buffer()), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
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
