//! Volume step + vertical pill, served by an on-demand daemon.
//!
//! The first tick in a burst becomes the daemon: it binds
//! `$XDG_RUNTIME_DIR/r_vol.sock`, shows the pill, and applies every later
//! tick in the same frame it arrives in. Followers only connect, send one
//! line, and read one ack (~ms). ~1.1s after the last command the daemon
//! exits and unlinks the socket. No stamp files, no locks, no polling:
//! the daemon is the volume authority while visible.
use crate::utils::{self, PwNode};
use anyhow::Context;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    shell::WaylandSurface,
    shell::wlr_layer::{
        Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
        LayerSurfaceConfigure,
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::io::{BufRead, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Child;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, PathBuilder, Pixmap, Rect, Stroke, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_region::WlRegion, wl_surface},
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 384;
/// Daemon exits this long after the last command.
const DISPLAY: Duration = Duration::from_millis(1100);
/// Backstop so a stuck daemon can never linger forever.
const MAX_LIFE: Duration = Duration::from_secs(10);

fn runtime_dir() -> std::path::PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

fn sock_path() -> std::path::PathBuf {
    runtime_dir().join("r_vol.sock")
}

/// Unlinks the socket on every exit path. Kill -9 still leaves a stale file;
/// the next tick unlinks it on ECONNREFUSED.
struct SockGuard(std::path::PathBuf);
impl Drop for SockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Physical sink ids, cached for SINKS_TTL so bursts skip `pw-dump`.
/// Topology changes rarely; a failed `wpctl` drops the cache for instant heal.
const SINKS_TTL: Duration = Duration::from_secs(30);

fn parse_sink_cache(s: &str) -> Option<Vec<u32>> {
    let ids: Vec<u32> = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    (!ids.is_empty()).then_some(ids)
}

fn physical_sink_ids() -> anyhow::Result<Vec<u32>> {
    let cache = runtime_dir().join("r_vol.sinks");
    if let Ok(mtime) = std::fs::metadata(&cache).and_then(|m| m.modified())
        && let Ok(age) = std::time::SystemTime::now().duration_since(mtime)
        && age <= SINKS_TTL
        && let Ok(text) = std::fs::read_to_string(&cache)
        && let Some(ids) = parse_sink_cache(&text)
    {
        return Ok(ids);
    }
    let nodes = utils::get_all_sinks().context("Failed to get sinks")?;
    let ids = get_physical_sink_ids(&nodes);
    if ids.is_empty() {
        anyhow::bail!("No physical sinks found");
    }
    let _ = std::fs::write(&cache, ids.iter().map(u32::to_string).collect::<Vec<_>>().join(" "));
    Ok(ids)
}

fn drop_sink_cache() {
    let _ = std::fs::remove_file(runtime_dir().join("r_vol.sinks"));
}

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

/// Accepts `wpctl set-volume` shapes plus a leading-sign convenience form:
/// `2%+`, `2%-`, `+2%`, `-2%`, `2%`, `0.5`, and `toggle`/`mute`.
fn normalize_arg(arg: &str) -> anyhow::Result<String> {
    let a = arg.trim();
    if a == "toggle" || a == "mute" {
        return Ok("toggle".to_string());
    }
    if a.is_empty() || a.len() > 16 || !a.bytes().all(|b| b.is_ascii_digit() || b"%.+-".contains(&b)) {
        anyhow::bail!("bad volume arg: {arg}");
    }
    // `+2%` -> `2%+` (wpctl wants the sign trailing).
    if (a.starts_with('+') || a.starts_with('-')) && !(a.ends_with('+') || a.ends_with('-')) {
        let (sign, rest) = (a.as_bytes()[0] as char, &a[1..]);
        return Ok(format!("{rest}{sign}"));
    }
    Ok(a.to_string())
}

/// Absolute-sync math: apply a normalized step to the current level.
fn step(current: f32, norm: &str) -> anyhow::Result<f32> {
    let operator = if norm.ends_with('+') {
        "+"
    } else if norm.ends_with('-') {
        "-"
    } else {
        ""
    };
    let without_op = norm.strip_suffix(['+', '-']).unwrap_or(norm);
    let is_pct = without_op.ends_with('%');
    let number = without_op.strip_suffix('%').unwrap_or(without_op);
    let amount: f32 = number.parse().context("Failed to parse volume amount")?;
    let delta = if is_pct { amount / 100.0 } else { amount };
    Ok(match operator {
        "+" => current + delta,
        "-" => current - delta,
        _ => delta,
    }
    .max(0.0)
    .min(1.5))
}

fn read_level(sink: &str) -> (f32, bool) {
    let out = utils::exec_output("wpctl", &["get-volume", sink]).unwrap_or_default();
    let level = out
        .split_whitespace()
        .filter_map(|s| s.parse::<f32>().ok())
        .next()
        .unwrap_or(0.0);
    (level, out.contains("MUTED"))
}

/// Spawn-only half: the daemon fires the batch and reaps on later ticks so
/// the bar draws the same frame the command arrives in.
fn wpctl_spawn(ids: &[u32], op: &str, val: &str) -> std::io::Result<Vec<Child>> {
    ids.iter()
        .map(|id| {
            std::process::Command::new("wpctl")
                .args([op, &id.to_string(), val])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        })
        .collect()
}

/// One round trip for all sinks: spawn every `wpctl`, then reap.
fn wpctl_all(ids: &[u32], op: &str, val: &str) -> anyhow::Result<()> {
    for mut child in
        wpctl_spawn(ids, op, val).context("failed to run wpctl — is wireplumber installed?")?
    {
        if !child.wait().context("wpctl failed")?.success() {
            drop_sink_cache();
            anyhow::bail!("wpctl {op} failed");
        }
    }
    Ok(())
}

fn ack_line(ok: bool, level: f32, muted: bool) -> String {
    if ok {
        format!("OK {level:.2} {}\n", muted as u8)
    } else {
        "ERR\n".to_string()
    }
}

enum Send {
    Acked,
    NoDaemon,
    Broken,
}

/// Hot tick: hand the command to the live daemon.
fn try_send(norm: &str) -> Send {
    let path = sock_path();
    let stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(_) => {
            // Dead or never-started daemon; clear the way for a new bind.
            let _ = std::fs::remove_file(&path);
            return Send::NoDaemon;
        }
    };
    if stream.set_read_timeout(Some(Duration::from_millis(500))).is_err()
        || stream.set_write_timeout(Some(Duration::from_millis(500))).is_err()
    {
        return Send::Broken;
    }
    let mut stream = stream;
    if write!(stream, "{norm}\n").is_err() {
        return Send::Broken;
    }
    let mut line = String::new();
    match std::io::BufReader::new(&stream).read_line(&mut line) {
        Ok(_) if line.starts_with("OK") => Send::Acked,
        _ => Send::Broken,
    }
}

/// Synchronous fallback: adjusts without any pill. Used headless and when
/// the daemon socket is unusable.
fn direct_one_shot(norm: &str) -> anyhow::Result<()> {
    let ids = physical_sink_ids()?;
    if norm == "toggle" {
        wpctl_all(&ids, "set-mute", "toggle")
    } else {
        let (current, _) = read_level(&ids[0].to_string());
        wpctl_all(&ids, "set-volume", &format!("{:.2}", step(current, norm)?))
    }
}

pub fn run(arg: &str) -> anyhow::Result<()> {
    let norm = normalize_arg(arg)?;
    match try_send(&norm) {
        // Hot tick: daemon applied it, ~ms.
        Send::Acked => return Ok(()),
        Send::NoDaemon => {}
        // Daemon died mid-flight; run it directly (freak double-apply risk).
        Send::Broken => return direct_one_shot(&norm),
    }
    // Cold tick: become the daemon.
    let ids = physical_sink_ids()?;
    let primary = ids[0].to_string();
    let (level, muted) = read_level(&primary);
    match UnixListener::bind(sock_path()) {
        Ok(listener) => {
            let guard = SockGuard(sock_path());
            if run_daemon(listener, guard, primary, ids, level, muted, &norm).is_err() {
                // Setup failed before the inaugural command applied.
                return direct_one_shot(&norm);
            }
            Ok(())
        }
        Err(_) => {
            // Lost the bind race: winner serves, or the file is stale.
            std::thread::sleep(Duration::from_millis(50));
            match try_send(&norm) {
                Send::Acked => Ok(()),
                _ => direct_one_shot(&norm),
            }
        }
    }
}

// --- daemon ---

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    #[allow(dead_code)]
    compositor_state: CompositorState,
    #[allow(dead_code)]
    layer_shell: LayerShell,
    shm: Shm,
    #[allow(dead_code)]
    layer_surface: Option<LayerSurface>,
    surface: Option<wl_surface::WlSurface>,
    pixmap: Pixmap,
    slot_pool: Option<SlotPool>,
    configured: bool,
    start: Instant,
    done: bool,
    primary: String,
    ids: Vec<u32>,
    level: f32,
    muted: bool,
    last_cmd: Instant,
    children: Vec<Child>,
    sinks_dirty: bool,
    poll_tick: u8,
}

/// Applies one command line to the live state and fires the `wpctl` batch
/// async (reaped on later ticks). Returns whether to ack OK.
fn apply(app: &mut App, cmd: &str) -> bool {
    let norm = match normalize_arg(cmd) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let (op, val) = if norm == "toggle" {
        app.muted = !app.muted;
        ("set-mute".to_string(), "toggle".to_string())
    } else {
        match step(app.level, &norm) {
            Ok(v) => app.level = v,
            Err(_) => return false,
        }
        ("set-volume".to_string(), format!("{:.2}", app.level))
    };
    match wpctl_spawn(&app.ids, &op, &val) {
        Ok(children) => app.children.extend(children),
        Err(_) => return false,
    }
    app.last_cmd = Instant::now();
    true
}

// --- daemon socket: blocking accept thread, instant wake via channel ---

/// Follower conns arrive here; each carries one command line plus its stream
/// for the ack. The loop thread applies + redraws the same wakeup.
fn socket_thread(listener: UnixListener, tx: calloop::channel::Sender<(String, UnixStream)>) {
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { break };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut data = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    data.extend_from_slice(&buf[..n]);
                    if data.len() > 256 || data.contains(&b'\n') {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let cmd = String::from_utf8_lossy(&data);
        let cmd = cmd.lines().next().unwrap_or("").trim().to_string();
        if tx.send((cmd, stream)).is_err() {
            break; // daemon gone; process is exiting anyway
        }
    }
}

fn redraw(app: &mut App) {
    if app.configured {
        draw(&mut app.pixmap, app.level, app.muted);
        utils::upload_rgba(&app.pixmap, &mut app.slot_pool, &app.surface);
    }
}

fn run_daemon(
    listener: UnixListener,
    _guard: SockGuard,
    primary: String,
    ids: Vec<u32>,
    level: f32,
    muted: bool,
    inaugural: &str,
) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (globals, queue) = registry_queue_init(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let compositor_state = CompositorState::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;

    let surface = compositor_state.create_surface(&qh);
    let region = compositor_state.wl_compositor().create_region(&qh, ());
    surface.set_input_region(Some(&region));
    let layer_surface =
        layer_shell.create_layer_surface(&qh, surface.clone(), Layer::Overlay, Some("r_multitool-vol"), None);
    layer_surface.set_anchor(Anchor::RIGHT | Anchor::BOTTOM);
    layer_surface.set_size(WIDTH, HEIGHT);
    layer_surface.set_margin(0, 24, 48, 0);
    layer_surface.set_exclusive_zone(0);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.commit();

    let slot_pool =
        SlotPool::new(WIDTH as usize * HEIGHT as usize * 4, &shm).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut app = App {
        registry_state,
        output_state,
        compositor_state,
        layer_shell,
        shm,
        layer_surface: Some(layer_surface),
        surface: Some(surface),
        pixmap: Pixmap::new(WIDTH, HEIGHT).ok_or_else(|| anyhow::anyhow!("pixmap"))?,
        slot_pool: Some(slot_pool),
        configured: false,
        start: Instant::now(),
        done: false,
        primary,
        ids,
        level,
        muted,
        last_cmd: Instant::now(),
        children: Vec::new(),
        sinks_dirty: false,
        poll_tick: 0,
    };
    if !apply(&mut app, inaugural) {
        anyhow::bail!("inaugural vol command failed");
    }

    let mut event_loop: calloop::EventLoop<App> =
        calloop::EventLoop::try_new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let wayland_source = calloop_wayland_source::WaylandSource::new(conn, queue);
    event_loop
        .handle()
        .insert_source(wayland_source, |_, queue, app| {
            queue.dispatch_pending(app).map(|_| 0).or(Ok(0))
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Socket commands wake the loop instantly (no frame quantization).
    let (cmd_tx, cmd_rx) = calloop::channel::channel::<(String, UnixStream)>();
    event_loop
        .handle()
        .insert_source(cmd_rx, |event, _, app| {
            if let calloop::channel::Event::Msg((cmd, mut stream)) = event {
                let ok = apply(app, &cmd);
                let _ = stream.write_all(ack_line(ok, app.level, app.muted).as_bytes());
                redraw(app);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::thread::spawn(move || socket_thread(listener, cmd_tx));
    let timer = calloop::timer::Timer::from_duration(Duration::from_millis(33));
    event_loop
        .handle()
        .insert_source(timer, |_, _, app| {
            if app.last_cmd.elapsed() > DISPLAY || app.start.elapsed() > MAX_LIFE {
                app.done = true;
                return calloop::timer::TimeoutAction::Drop;
            }
            // Reap async wpctl batches; a failure means topology changed.
            let mut failed = false;
            app.children.retain_mut(|c| match c.try_wait() {
                Ok(Some(st)) => {
                    failed |= !st.success();
                    false
                }
                Ok(None) => true,
                Err(_) => true,
            });
            if failed {
                app.sinks_dirty = true;
            }
            if app.sinks_dirty {
                app.sinks_dirty = false;
                drop_sink_cache();
                match physical_sink_ids() {
                    Ok(ids) => {
                        app.primary = ids[0].to_string();
                        app.ids = ids;
                    }
                    Err(_) => {
                        app.done = true;
                        return calloop::timer::TimeoutAction::Drop;
                    }
                }
            }
            // Slow re-read corrects drift from external changes; skipped
            // while a batch is in flight so the bar never snaps back mid-apply.
            app.poll_tick = app.poll_tick.wrapping_add(1);
            if app.poll_tick % 30 == 0 && app.children.is_empty() {
                (app.level, app.muted) = read_level(&app.primary);
            }
            if app.configured {
                redraw(app);
            }
            calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(33))
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    loop {
        if app.done || app.start.elapsed() > MAX_LIFE {
            break;
        }
        if event_loop.dispatch(Some(Duration::from_millis(100)), &mut app).is_err() {
            break;
        }
    }
    Ok(())
}

// --- drawing ---

const TRACK_X: f32 = 26.0;
const TRACK_W: f32 = 12.0;
const TRACK_TOP: f32 = 28.0;
const TRACK_BOT: f32 = 356.0;
const TICKS: [f32; 4] = [0.2, 0.4, 0.6, 0.8];

fn draw(pixmap: &mut Pixmap, level: f32, muted: bool) {
    pixmap.fill(Color::TRANSPARENT);
    // Same normal-card theme as the ptt badge: black@50% + white@25% ring.
    // 2px ring on integer coords: full coverage along the whole ring, so the
    // caps can't form isolated bright dots the way a 1px hairline does.
    if let Some(path) = utils::stadium(8.0, 8.0, 48.0, 368.0, 24.0) {
        let mut fill = utils::paint_rgba(0, 0, 0, 128);
        fill.anti_alias = true;
        pixmap.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
        let edge = utils::paint_rgba(255, 255, 255, 64);
        let mut stroke = Stroke::default();
        stroke.width = 2.0;
        pixmap.stroke_path(&path, &edge, &stroke, Transform::identity(), None);
    }
    let track_h = TRACK_BOT - TRACK_TOP;
    let track = utils::paint_rgba(255, 255, 255, 28);
    if let Some(r) = Rect::from_xywh(TRACK_X, TRACK_TOP, TRACK_W, track_h) {
        pixmap.fill_rect(r, &track, Transform::identity(), None);
    }
    let lvl = level.clamp(0.0, 1.5);
    let fill_h = track_h * lvl.min(1.0);
    if fill_h > 0.5 {
        // ponytail: over-100% tints warm so the clamp is visible, not silent.
        let (fr, fg, fb) = if muted {
            (130, 130, 130)
        } else if lvl > 1.0 {
            (255, 150, 120)
        } else {
            (255, 255, 255)
        };
        let fill = utils::paint_rgba(fr, fg, fb, 255);
        if let Some(r) = Rect::from_xywh(TRACK_X, TRACK_BOT - fill_h, TRACK_W, fill_h) {
            pixmap.fill_rect(r, &fill, Transform::identity(), None);
        }
    }
    // Tick at f sits over fill iff f <= lvl: dark-on-fill, light-on-track.
    for f in TICKS {
        let y = TRACK_BOT - track_h * f;
        let over_fill = !muted && f <= lvl.min(1.0) && fill_h > 0.5;
        let tick = if over_fill {
            utils::paint_rgba(0, 0, 0, 170)
        } else {
            utils::paint_rgba(255, 255, 255, 110)
        };
        if let Some(r) = Rect::from_xywh(TRACK_X - 6.0, y - 1.0, TRACK_W + 12.0, 2.0) {
            pixmap.fill_rect(r, &tick, Transform::identity(), None);
        }
    }
    if muted {
        let cross = utils::paint_rgba(255, 120, 120, 255);
        let mut stroke = Stroke::default();
        stroke.width = 4.0;
        stroke.line_cap = tiny_skia::LineCap::Round;
        for (x0, y0, x1, y1) in [(16.0, 176.0, 48.0, 208.0), (48.0, 176.0, 16.0, 208.0)] {
            let mut pb = PathBuilder::new();
            pb.move_to(x0, y0);
            pb.line_to(x1, y1);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &cross, &stroke, Transform::identity(), None);
            }
        }
    }
}

// --- wayland boilerplate ---

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {}
    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.configured = true;
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    fn runtime_add_global(&mut self, _: &Connection, _: &QueueHandle<Self>, _: u32, _: &str, _: u32) {}
    fn runtime_remove_global(&mut self, _: &Connection, _: &QueueHandle<Self>, _: u32, _: &str) {}
}

impl Dispatch<WlRegion, ()> for App {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegion,
        _event: <WlRegion as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(pix: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let d = pix.data();
        let o = ((y * pix.width() + x) * 4) as usize;
        (d[o], d[o + 1], d[o + 2], d[o + 3])
    }

    #[test]
    fn arg_shapes() {
        assert_eq!(normalize_arg("2%+").unwrap(), "2%+");
        assert_eq!(normalize_arg("+2%").unwrap(), "2%+");
        assert_eq!(normalize_arg("-2%").unwrap(), "2%-");
        assert_eq!(normalize_arg("toggle").unwrap(), "toggle");
        assert_eq!(normalize_arg("0.5").unwrap(), "0.5");
        assert!(normalize_arg("2%; rm -rf ~").is_err());
        assert!(normalize_arg("").is_err());
    }

    #[test]
    fn step_math() {
        assert!((step(0.40, "2%+").unwrap() - 0.42).abs() < 1e-6);
        assert!((step(0.40, "2%-").unwrap() - 0.38).abs() < 1e-6);
        assert!((step(0.40, "0.5").unwrap() - 0.5).abs() < 1e-6);
        assert!((step(1.49, "2%+").unwrap() - 1.5).abs() < 1e-6);
        assert_eq!(step(0.01, "2%-").unwrap(), 0.0);
        assert!(step(0.4, "bogus").is_err());
    }

    #[test]
    fn ack_shapes() {
        assert_eq!(ack_line(true, 0.42, false), "OK 0.42 0\n");
        assert_eq!(ack_line(true, 1.0, true), "OK 1.00 1\n");
        assert_eq!(ack_line(false, 0.0, false), "ERR\n");
    }

    #[test]
    fn sink_cache_shapes() {
        assert_eq!(parse_sink_cache("41 42"), Some(vec![41, 42]));
        assert_eq!(parse_sink_cache(""), None);
        assert_eq!(parse_sink_cache("41 xx"), None);
    }

    #[test]
    fn pill_mirror_symmetric() {
        // Pill axis is x=32: mirrored pixels must match. Guards the stadium
        // cubics (a wrong bottom-right control point once made that cap
        // square while the others stayed round).
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, 0.65, false);
        for (l, r) in [
            ((8, 192), (55, 192)),
            ((15, 369), (48, 369)),
            ((8, 352), (55, 352)),
            ((15, 15), (48, 15)),
        ] {
            let la = pixel(&pix, l.0, l.1).3 as i16;
            let ra = pixel(&pix, r.0, r.1).3 as i16;
            assert!((la - ra).abs() < 25, "cap asymmetric {l:?}={la} vs {r:?}={ra}");
        }
    }

    #[test]
    fn pill_border_uniform() {
        // 2px ring: solid along straights and caps, nothing beyond it.
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, 0.5, false);
        assert_eq!(pixel(&pix, 6, 192).3, 0, "fringe outside 2px ring");
        let straight = pixel(&pix, 8, 192).3 as i16;
        let arc = pixel(&pix, 15, 15).3 as i16;
        assert!((straight - arc).abs() < 30, "ring uneven {straight} vs {arc}");
    }

    #[test]
    fn pill_fill_and_ticks() {
        let track_h = TRACK_BOT - TRACK_TOP;
        // Empty: track dim, tick light.
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, 0.0, false);
        let (r, g, b, _) = pixel(&pix, 32, (TRACK_BOT - 4.0) as u32);
        assert!(r < 60 && g < 60 && b < 60, "empty track dim {r},{g},{b}");
        // Half: fill bright below 50%, tick at 40% dark-on-fill.
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, 0.5, false);
        let y_fill = (TRACK_BOT - track_h * 0.3) as u32;
        let (r, g, b, _) = pixel(&pix, 32, y_fill);
        assert!(r > 200 && g > 200 && b > 200, "fill bright {r},{g},{b}");
        let y_tick = (TRACK_BOT - track_h * 0.4) as u32;
        let (r, g, b, _) = pixel(&pix, 32, y_tick);
        assert!(r < 100 && g < 100 && b < 100, "tick dark on fill {r},{g},{b}");
        let y_empty = (TRACK_BOT - track_h * 0.8) as u32;
        let y_plain = (TRACK_BOT - track_h * 0.7) as u32;
        let (tr, tg, tb, _) = pixel(&pix, 32, y_empty);
        let (pr, pg, pb, _) = pixel(&pix, 32, y_plain);
        assert!(tr > pr + 40 && tg > pg + 40 && tb > pb + 40, "tick lighter than track {tr},{tg},{tb} vs {pr},{pg},{pb}");
        // Muted: fill gray + red cross at center.
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, 0.5, true);
        let (r, g, b, _) = pixel(&pix, 32, 192);
        assert!(r > 200 && g <= 130 && b <= 130, "mute cross red {r},{g},{b}");
    }
}
