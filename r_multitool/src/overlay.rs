//! Shared bottom-pill layershell widget (whisprflow-style) for `stt` and `ptt`.
//! Runs its own Wayland event loop on a background thread; all failures are
//! silent so callers keep working headless / without a compositor.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_region::WlRegion, wl_shm, wl_surface},
};

pub const AUDIO_PATH: &str = "/tmp/r_stt.wav";
pub const PTT_METER_PATH: &str = "/tmp/r_ptt.wav";

const WIDTH: u32 = 272;
const HEIGHT: u32 = 52;
const BARS: usize = 60;
const PTT_BARS: usize = 23;
const PILL_Y: f32 = 8.0;
const PILL_H: f32 = 36.0;
const CY: f32 = 26.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Record = 0,
    Transcribing = 1,
    MicOn = 2,
}

impl Phase {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::Transcribing,
            2 => Phase::MicOn,
            _ => Phase::Record,
        }
    }
}

struct State {
    phase: AtomicU8,
    done: AtomicBool,
    start: Instant,
    meter_path: Option<String>,
}

pub struct Overlay {
    state: Arc<State>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Overlay {
    pub fn spawn(phase: Phase, meter_path: Option<&str>) -> Self {
        let state = Arc::new(State {
            phase: AtomicU8::new(phase as u8),
            done: AtomicBool::new(false),
            start: Instant::now(),
            meter_path: meter_path.map(str::to_string),
        });
        let thread_state = state.clone();
        let handle = std::thread::spawn(move || run(thread_state));
        Self {
            state,
            handle: Some(handle),
        }
    }
    pub fn set_phase(&self, phase: Phase) {
        self.state.phase.store(phase as u8, Ordering::Relaxed);
    }
    pub fn finish(&mut self) {
        self.state.done.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.finish();
    }
}

fn run(state: Arc<State>) {
    if let Err(e) = run_wayland(&state) {
        eprintln!("r_multitool overlay: {e}");
    }
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    // Held for ownership only (dropping layer_surface destroys it).
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
    last_len: u64,
    last_progress: Instant,
    levels: std::collections::VecDeque<f32>,
}

struct Data {
    app: App,
    state: Arc<State>,
}

fn run_wayland(state: &Arc<State>) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let (globals, queue) = registry_queue_init(&conn).map_err(|e| anyhow::anyhow!("{e}"))?;
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let compositor_state =
        CompositorState::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| anyhow::anyhow!("{e}"))?;

    let surface = compositor_state.create_surface(&qh);
    // Empty input region = click-through.
    let region = compositor_state.wl_compositor().create_region(&qh, ());
    surface.set_input_region(Some(&region));
    let layer_surface =
        layer_shell.create_layer_surface(&qh, surface.clone(), Layer::Overlay, Some("r_multitool-pill"), None);
    layer_surface.set_anchor(Anchor::BOTTOM);
    layer_surface.set_size(WIDTH, HEIGHT);
    layer_surface.set_margin(0, 0, 32, 0);
    layer_surface.set_exclusive_zone(0);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.commit();

    let slot_pool =
        SlotPool::new(WIDTH as usize * HEIGHT as usize * 4, &shm).map_err(|e| anyhow::anyhow!("{e}"))?;
    let app = App {
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
        last_len: 0,
        last_progress: Instant::now(),
        levels: std::collections::VecDeque::from([0.0; BARS]),
    };
    let mut data = Data {
        app,
        state: state.clone(),
    };

    let mut event_loop: calloop::EventLoop<Data> =
        calloop::EventLoop::try_new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let wayland_source = calloop_wayland_source::WaylandSource::new(conn, queue);
    event_loop
        .handle()
        .insert_source(wayland_source, |_, queue, data| {
            match queue.dispatch_pending(&mut data.app) {
                Ok(n) => Ok(n),
                Err(e) => {
                    eprintln!("r_multitool overlay: wayland dispatch: {e}");
                    Ok(0)
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let timer = calloop::timer::Timer::from_duration(Duration::from_millis(33));
    event_loop
        .handle()
        .insert_source(timer, |_, _, data| {
            tick(data);
            calloop::timer::TimeoutAction::ToDuration(Duration::from_millis(33))
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    loop {
        if data.state.done.load(Ordering::Relaxed) {
            break;
        }
        if event_loop
            .dispatch(Some(Duration::from_millis(100)), &mut data)
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

// Wayland events flow through the loop's source callback; the timer draws.
fn tick(data: &mut Data) {
    if !data.app.configured {
        return;
    }
    let phase = Phase::from_u8(data.state.phase.load(Ordering::Relaxed));
    let elapsed = data.state.start.elapsed();
    let meter: Option<&str> = data.state.meter_path.as_deref();
    let metered = matches!(phase, Phase::Record | Phase::MicOn) && meter.is_some();
    let stalled = if metered {
        let len = std::fs::metadata(meter.unwrap_or(AUDIO_PATH))
            .map(|m| m.len())
            .unwrap_or(0);
        let now = Instant::now();
        if len > data.app.last_len {
            data.app.last_len = len;
            data.app.last_progress = now;
        }
        elapsed > Duration::from_secs(1) && now - data.app.last_progress > Duration::from_millis(1500)
    } else {
        false
    };
    if metered {
        data.app.levels.push_back(sample_level(meter.unwrap_or(AUDIO_PATH)));
        while data.app.levels.len() > BARS {
            data.app.levels.pop_front();
        }
    }
    draw(&mut data.app.pixmap, phase, stalled, &data.app.levels);
    upload(&mut data.app);
}

fn upload(app: &mut App) {
    let Some(surface) = &app.surface else { return };
    let width = app.pixmap.width() as i32;
    let height = app.pixmap.height() as i32;
    let stride = width * 4;
    let Some(pool) = &mut app.slot_pool else { return };
    let size = (stride * height) as usize;
    if pool.len() < size && pool.resize(size).is_err() {
        return;
    }
    if let Ok((buffer, canvas)) = pool.create_buffer(width, height, stride, wl_shm::Format::Argb8888) {
        for (pixel, out) in app
            .pixmap
            .data()
            .chunks_exact(4)
            .zip(canvas.chunks_exact_mut(4))
        {
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

// --- mic level sampler: tails the wav pw-record is writing ---

struct WavSpec {
    data_start: u64,
    bytes_per_frame: u64,
    sample_rate: u32,
    format: u16, // 1 = int PCM, 3 = float
    bits: u16,
}

fn read_at(file: &mut std::fs::File, off: u64, len: usize) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(off)).ok()?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn u16le(b: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(0..2)?.try_into().ok()?))
}

fn u32le(b: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(0..4)?.try_into().ok()?))
}

fn parse_wav_spec(head: &[u8]) -> Option<WavSpec> {
    if head.len() < 44 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut off = 12;
    while off + 8 <= head.len() {
        let size = u32le(head.get(off + 4..off + 8)?)? as usize;
        if &head[off..off + 4] == b"fmt " && size >= 16 && off + 24 <= head.len() {
            let c = &head[off + 8..];
            fmt = Some((
                u16le(&c[0..])?,
                u16le(&c[2..])?,
                u32le(&c[4..])?,
                u16le(&c[14..])?,
            ));
        }
        if &head[off..off + 4] == b"data" {
            let (format, channels, rate, bits) = fmt?;
            let bytes_per_frame = channels as u64 * (bits as u64 / 8);
            if bytes_per_frame == 0 {
                return None;
            }
            return Some(WavSpec {
                data_start: (off + 8) as u64,
                bytes_per_frame,
                sample_rate: rate,
                format,
                bits,
            });
        }
        off += 8 + size + (size & 1);
    }
    None
}

/// Peak level of the trailing ~80ms of the wav, 0.0..1.0. Never panics.
fn sample_level(path: &str) -> f32 {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0.0,
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return 0.0,
    };
    let head = match read_at(&mut file, 0, len.min(4096) as usize) {
        Some(h) => h,
        None => return 0.0,
    };
    let spec = match parse_wav_spec(&head) {
        Some(s) => s,
        None => return 0.0,
    };
    if !matches!(spec.format, 1 | 3) || !matches!(spec.bits, 8 | 16 | 24 | 32) {
        return 0.0;
    }
    let frames_avail = len.saturating_sub(spec.data_start) / spec.bytes_per_frame;
    if frames_avail == 0 {
        return 0.0;
    }
    let want = (spec.sample_rate as u64 * 80 / 1000)
        .max(1)
        .min(frames_avail);
    let bytes = match read_at(
        &mut file,
        len - want * spec.bytes_per_frame,
        (want * spec.bytes_per_frame) as usize,
    ) {
        Some(b) => b,
        None => return 0.0,
    };
    let bytes_per_sample = (spec.bits / 8) as usize;
    let channels = (spec.bytes_per_frame / bytes_per_sample as u64) as usize;
    let mut peak = 0.0f32;
    for frame in bytes.chunks_exact(spec.bytes_per_frame as usize) {
        for ch in 0..channels {
            let s = &frame[ch * bytes_per_sample..(ch + 1) * bytes_per_sample];
            let v = match (spec.format, spec.bits) {
                (3, 32) => f32::from_le_bytes(s.try_into().ok().unwrap_or([0; 4])).abs(),
                (_, 8) => ((s[0] as f32) - 128.0).abs() / 128.0,
                (_, 16) => (i16::from_le_bytes([s[0], s[1]]) as f32).abs() / 32768.0,
                (_, 24) => {
                    let x = ((s[2] as i8 as i32) << 16) | ((s[1] as i32) << 8) | (s[0] as i32);
                    (x as f32).abs() / 8388608.0
                }
                (_, 32) => (i32::from_le_bytes(s.try_into().ok().unwrap_or([0; 4])) as f32).abs()
                    / 2147483648.0,
                _ => return 0.0,
            };
            if v > peak {
                peak = v;
            }
        }
    }
    peak.clamp(0.0, 1.0)
}

// --- drawing ---

const KAPPA: f32 = 0.5522847498;

fn stadium(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + KAPPA * r, y, x + w, y + r - KAPPA * r, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(
        x + w,
        y + h - r + KAPPA * r,
        x + w - r + KAPPA * r,
        y + h,
        x + w - r,
        y + h,
    );
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - KAPPA * r, y + h, x, y + h - r + KAPPA * r, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - KAPPA * r, x + r - KAPPA * r, y, x + r, y);
    pb.close();
    pb.finish()
}

fn paint_rgba(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}

fn draw_pill(
    pixmap: &mut Pixmap,
    x: f32,
    w: f32,
    fill: (u8, u8, u8, u8),
    edge: (u8, u8, u8, u8),
) {
    if let Some(path) = stadium(x, PILL_Y, w, PILL_H, PILL_H / 2.0) {
        let mut fill_paint = paint_rgba(fill.0, fill.1, fill.2, fill.3);
        fill_paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &fill_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        let edge_paint = paint_rgba(edge.0, edge.1, edge.2, edge.3);
        let mut stroke = Stroke::default();
        stroke.width = 1.0;
        pixmap.stroke_path(&path, &edge_paint, &stroke, Transform::identity(), None);
    }
}

// Mirrors quickshell Theme.qml: black@50% cards, white@25% hairlines,
// accentCritical red while recording, accentLow blue while transcribing.
fn phase_theme(phase: Phase) -> ((u8, u8, u8, u8), (u8, u8, u8, u8)) {
    match phase {
        Phase::Record => ((0, 0, 0, 128), (255, 107, 107, 255)),
        Phase::Transcribing => ((0, 0, 0, 128), (90, 169, 230, 255)),
        Phase::MicOn => ((0, 0, 0, 128), (255, 255, 255, 64)),
    }
}

fn draw(
    pixmap: &mut Pixmap,
    phase: Phase,
    stalled: bool,
    levels: &std::collections::VecDeque<f32>,
) {
    pixmap.fill(Color::TRANSPARENT);
    let cy = CY;

    // ptt: compact bars-only badge showing the live meter, newest on the right.
    if phase == Phase::MicOn {
        let (fill, edge) = phase_theme(phase);
        draw_pill(pixmap, 78.0, 116.0, fill, edge);
        let (br, bg, bb) = if stalled {
            (153, 153, 153)
        } else {
            (255, 255, 255)
        };
        let bar_paint = paint_rgba(br, bg, bb, 255);
        let fresh: Vec<f32> = levels
            .iter()
            .skip(levels.len().saturating_sub(PTT_BARS))
            .copied()
            .collect();
        for (i, level) in fresh.iter().enumerate() {
            let h = if stalled {
                4.0
            } else {
                4.0 + 22.0 * level.clamp(0.0, 1.0)
            };
            let x = 90.0 + i as f32 * 4.0;
            if let Some(r) = Rect::from_xywh(x, cy - h / 2.0, 2.0, h) {
                pixmap.fill_rect(r, &bar_paint, Transform::identity(), None);
            }
        }
        return;
    }

    // stt: the pill itself is the recording icon — critical-red hairline
    // while recording, low-blue while transcribing.
    let (fill, edge) = phase_theme(phase);
    draw_pill(pixmap, 4.0, 264.0, fill, edge);

    // Level history, newest on the right.
    let (br, bg, bb) = if stalled {
        (153, 153, 153)
    } else {
        (255, 255, 255)
    };
    let bar_paint = paint_rgba(br, bg, bb, 255);
    for (i, level) in levels.iter().take(BARS).enumerate() {
        let h = if stalled {
            4.0
        } else {
            4.0 + 26.0 * level.clamp(0.0, 1.0)
        };
        let x = 16.0 + i as f32 * 4.0;
        if let Some(r) = Rect::from_xywh(x, cy - h / 2.0, 2.0, h) {
            pixmap.fill_rect(r, &bar_paint, Transform::identity(), None);
        }
    }
}

// --- wayland boilerplate ---

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
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
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        if w > 0 && h > 0 && (w != WIDTH || h != HEIGHT) {
            // Compositor insisted on another size; keep our canvas, it anchors fine.
        }
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
    fn runtime_add_global(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: u32,
        _: &str,
        _: u32,
    ) {
    }
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

    fn synth_wav(format: u16, channels: u16, rate: u32, bits: u16, frames: &[f32]) -> Vec<u8> {
        let bps = (bits / 8) as usize;
        let mut data = Vec::with_capacity(frames.len() * channels as usize * bps);
        for &v in frames {
            for _ in 0..channels {
                match (format, bits) {
                    (3, 32) => data.extend_from_slice(&v.to_le_bytes()),
                    (_, 16) => data.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes()),
                    _ => unimplemented!(),
                }
            }
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&format.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * channels as u32 * bps as u32).to_le_bytes());
        wav.extend_from_slice(&((channels * bps as u16).to_le_bytes()));
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        wav
    }

    fn sine_wav(path: &std::path::Path, amp: f32) {
        let frames: Vec<f32> = (0..800)
            .map(|i| amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 8000.0).sin())
            .collect();
        std::fs::write(path, synth_wav(1, 1, 8000, 16, &frames)).unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("r_overlay_test_{name}.wav"))
    }

    fn pixel(pix: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let d = pix.data();
        let o = ((y * pix.width() + x) * 4) as usize;
        (d[o], d[o + 1], d[o + 2], d[o + 3])
    }

    #[test]
    fn stt_pill_themed() {
        // Mirrors quickshell Theme.qml tokens.
        assert_eq!(
            phase_theme(Phase::Record),
            ((0, 0, 0, 128), (255, 107, 107, 255))
        );
        assert_eq!(
            phase_theme(Phase::Transcribing),
            ((0, 0, 0, 128), (90, 169, 230, 255))
        );
        assert_eq!(
            phase_theme(Phase::MicOn),
            ((0, 0, 0, 128), (255, 255, 255, 64))
        );

        let levels = std::collections::VecDeque::from([1.0; BARS]);
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, Phase::Record, false, &levels);
        let (r, g, b, a) = pixel(&pix, 10, 26);
        assert!(a > 100 && a < 160 && r < 30 && g < 30 && b < 30, "dim card {r},{g},{b},{a}");
        let (r, g, b, _) = pixel(&pix, 17, 26);
        assert!(r > 200 && g > 200 && b > 200, "bright bar {r},{g},{b}");

        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, Phase::Transcribing, false, &levels);
        let (r, g, b, a) = pixel(&pix, 10, 26);
        assert!(a > 100 && r < 30 && g < 30 && b < 30, "dim card {r},{g},{b},{a}");

        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, Phase::MicOn, false, &levels);
        assert_eq!(pixel(&pix, 30, 26).3, 0, "no full-width pill for ptt");
        let (r, g, b, _) = pixel(&pix, 91, 26);
        assert!(r > 200 && g > 200 && b > 200, "white badge bar {r},{g},{b}");

        // Badge maps levels to bar heights: quiet left, loud right.
        let ramp: std::collections::VecDeque<f32> =
            (0..BARS as u32).map(|i| i as f32 / (BARS as f32 - 1.0)).collect();
        let mut pix = Pixmap::new(WIDTH, HEIGHT).unwrap();
        draw(&mut pix, Phase::MicOn, false, &ramp);
        let (r, g, b, _) = pixel(&pix, 91, 14);
        assert!(r < 40 && g < 40 && b < 40, "quiet bar stays short {r},{g},{b}");
        let (r, g, b, _) = pixel(&pix, 179, 14);
        assert!(r > 200 && g > 200 && b > 200, "loud bar reaches up {r},{g},{b}");
    }

    #[test]
    fn sampler_reads_tone() {
        let p = tmp("tone");
        sine_wav(&p, 0.5);
        let level = sample_level(p.to_str().unwrap());
        assert!((0.45..=0.55).contains(&level), "level={level}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn sampler_reads_float_stereo() {
        let p = tmp("float");
        let frames: Vec<f32> = (0..800)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 8000.0).sin())
            .collect();
        std::fs::write(&p, synth_wav(3, 2, 8000, 32, &frames)).unwrap();
        let level = sample_level(p.to_str().unwrap());
        assert!((0.45..=0.55).contains(&level), "level={level}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn sampler_silence_and_garbage() {
        let p = tmp("silence");
        std::fs::write(&p, synth_wav(1, 1, 8000, 16, &[0.0; 800])).unwrap();
        assert_eq!(sample_level(p.to_str().unwrap()), 0.0);
        std::fs::remove_file(&p).ok();
        let g = tmp("garbage");
        std::fs::write(&g, b"hello world, not a wav").unwrap();
        assert_eq!(sample_level(g.to_str().unwrap()), 0.0);
        std::fs::remove_file(&g).ok();
        assert_eq!(sample_level("/nonexistent/r_overlay_test.wav"), 0.0);
    }
}
