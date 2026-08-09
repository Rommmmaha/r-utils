use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use lamco_pipewire::{PipeWireThreadCommand, PipeWireThreadManager, StreamConfig};
use std::num::NonZeroU32;
use std::os::fd::IntoRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
const MAX_CROP: f32 = 0.9;
const MIN_SELECT: f64 = 4.0;
fn mode_title(mode: Mode) -> &'static str {
    match mode {
        Mode::Stretch => "r_peek [Stretch]",
        Mode::Fit => "r_peek [Fit]",
    }
}
fn order(a: f64, b: f64) -> (f64, f64) {
    if a < b { (a, b) } else { (b, a) }
}
struct Frame {
    w: u32,
    h: u32,
    stride: u32,
    data: Arc<Vec<u8>>,
}
struct Source<'a> {
    data: &'a [u8],
    w: u32,
    h: u32,
    stride: u32,
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Stretch,
    Fit,
}
struct App {
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    proxy: EventLoopProxy<()>,
    frame: Arc<Mutex<Option<Frame>>>,
    requesting: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    cursor: (f64, f64),
    selecting: Option<(f64, f64)>,
    crop: [f32; 4],
    mode: Mode,
}
impl App {
    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (self.window.as_ref(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (w, h) = (size.width, size.height);
        if w == 0 || h == 0 {
            return;
        }
        let nw = NonZeroU32::new(w).unwrap();
        let nh = NonZeroU32::new(h).unwrap();
        surface.resize(nw, nh).unwrap();
        let mut buffer = surface.buffer_mut().unwrap();
        buffer.fill(0);
        if let Some(f) = self.frame.lock().unwrap().as_ref() {
            let crop = crop_region(f.w, f.h, self.crop);
            let dest = dest_rect(w, h, crop.2, crop.3, self.mode);
            let src = Source {
                data: &f.data,
                w: f.w,
                h: f.h,
                stride: f.stride,
            };
            draw_stretch(&mut buffer, w, dest, src, crop);
        }
        if let Some(s) = self.selecting {
            draw_select(&mut buffer, w, h, s, self.cursor);
        }
        buffer.present().unwrap();
    }
}
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let attrs = Window::default_attributes()
                .with_title(mode_title(Mode::Stretch))
                .with_inner_size(winit::dpi::LogicalSize::new(640.0, 480.0));
            let window = Arc::new(event_loop.create_window(attrs).unwrap());
            let context = softbuffer::Context::new(window.clone()).unwrap();
            let surface = softbuffer::Surface::new(&context, window.clone()).unwrap();
            self.window = Some(window);
            self.surface = Some(surface);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if self.selecting.is_some() {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => match event.physical_key {
                PhysicalKey::Code(KeyCode::Escape) => {
                    self.stop.store(true, Ordering::SeqCst);
                    if self.streaming.swap(false, Ordering::SeqCst) {
                        *self.frame.lock().unwrap() = None;
                    }
                    self.crop = [0.0; 4];
                    window.request_redraw();
                }
                PhysicalKey::Code(KeyCode::Space) => {
                    self.crop = [0.0; 4];
                    window.request_redraw();
                }
                PhysicalKey::Code(KeyCode::KeyS) => {
                    self.mode = Mode::Stretch;
                    window.set_title(mode_title(self.mode));
                    window.request_redraw();
                }
                PhysicalKey::Code(KeyCode::KeyF) => {
                    self.mode = Mode::Fit;
                    window.set_title(mode_title(self.mode));
                    window.request_redraw();
                }
                _ => {}
            },
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if !self.streaming.load(Ordering::SeqCst) {
                            if !self.requesting.swap(true, Ordering::SeqCst) {
                                start_portal(
                                    self.frame.clone(),
                                    self.requesting.clone(),
                                    self.streaming.clone(),
                                    self.stop.clone(),
                                    self.proxy.clone(),
                                );
                            }
                        } else {
                            self.selecting = Some(self.cursor);
                        }
                    }
                    ElementState::Released => {
                        if let Some(start) = self.selecting.take() {
                            if let Some(f) = self.frame.lock().unwrap().as_ref() {
                                let size = window.inner_size();
                                self.crop = zoom_crop(
                                    start,
                                    self.cursor,
                                    (size.width, size.height),
                                    f.w,
                                    f.h,
                                    self.crop,
                                );
                            }
                            window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::Resized(_) => window.request_redraw(),
            WindowEvent::RedrawRequested => self.redraw(),
            _ => (),
        }
    }
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}
fn crop_region(sw: u32, sh: u32, crop: [f32; 4]) -> (f32, f32, f32, f32) {
    let mut l = crop[0] * sw as f32;
    let mut r = crop[2] * sw as f32;
    let mut t = crop[1] * sh as f32;
    let mut b = crop[3] * sh as f32;
    let ml = sw as f32 * MAX_CROP;
    l = l.clamp(0.0, ml);
    r = r.clamp(0.0, ml);
    if l + r > ml {
        let s = ml / (l + r);
        l *= s;
        r *= s;
    }
    let mh = sh as f32 * MAX_CROP;
    t = t.clamp(0.0, mh);
    b = b.clamp(0.0, mh);
    if t + b > mh {
        let s = mh / (t + b);
        t *= s;
        b *= s;
    }
    (l, t, sw as f32 - l - r, sh as f32 - t - b)
}
fn dest_rect(w: u32, h: u32, cw: f32, ch: f32, mode: Mode) -> (u32, u32, u32, u32) {
    match mode {
        Mode::Stretch => (0, 0, w, h),
        Mode::Fit => {
            let sa = cw / ch;
            let wa = w as f32 / h as f32;
            let (dw, dh) = if sa > wa {
                (w as f32, w as f32 / sa)
            } else {
                (h as f32 * sa, h as f32)
            };
            let dw = dw.min(w as f32) as u32;
            let dh = dh.min(h as f32) as u32;
            ((w - dw) / 2, (h - dh) / 2, dw, dh)
        }
    }
}
fn zoom_crop(
    start: (f64, f64),
    end: (f64, f64),
    win: (u32, u32),
    sw: u32,
    sh: u32,
    crop: [f32; 4],
) -> [f32; 4] {
    let (w, h) = (win.0.max(1) as f64, win.1.max(1) as f64);
    let (x0, x1) = order(start.0, end.0);
    let (y0, y1) = order(start.1, end.1);
    let x0 = x0.clamp(0.0, w);
    let y0 = y0.clamp(0.0, h);
    let x1 = x1.clamp(0.0, w);
    let y1 = y1.clamp(0.0, h);
    if x1 - x0 < MIN_SELECT || y1 - y0 < MIN_SELECT {
        return crop;
    }
    let (l, t, cw, ch) = crop_region(sw, sh, crop);
    let sx0 = (l as f64 + (x0 / w) * cw as f64) as f32;
    let sy0 = (t as f64 + (y0 / h) * ch as f64) as f32;
    let sx1 = (l as f64 + (x1 / w) * cw as f64) as f32;
    let sy1 = (t as f64 + (y1 / h) * ch as f64) as f32;
    [
        (sx0 / sw as f32).clamp(0.0, MAX_CROP),
        (sy0 / sh as f32).clamp(0.0, MAX_CROP),
        ((sw as f32 - sx1) / sw as f32).clamp(0.0, MAX_CROP),
        ((sh as f32 - sy1) / sh as f32).clamp(0.0, MAX_CROP),
    ]
}
fn draw_select(dst: &mut [u32], w: u32, h: u32, start: (f64, f64), cursor: (f64, f64)) {
    let (x0, x1) = order(start.0, cursor.0);
    let (y0, y1) = order(start.1, cursor.1);
    let x0 = x0.clamp(0.0, w as f64) as u32;
    let y0 = y0.clamp(0.0, h as f64) as u32;
    let x1 = x1.clamp(0.0, w as f64).max(x0 as f64 + 1.0) as u32;
    let y1 = y1.clamp(0.0, h as f64).max(y0 as f64 + 1.0) as u32;
    let x1 = x1.min(w);
    let y1 = y1.min(h);
    for py in 0..h {
        let row = (py * w) as usize;
        for px in 0..w {
            if px < x0 || px >= x1 || py < y0 || py >= y1 {
                let c = dst[row + px as usize];
                dst[row + px as usize] = dim(c);
            }
        }
    }
    for px in x0..x1 {
        dst[(y0 * w + px) as usize] = 0xFFFFFF;
        dst[((y1 - 1) * w + px) as usize] = 0xFFFFFF;
    }
    for py in y0..y1 {
        dst[(py * w + x0) as usize] = 0xFFFFFF;
        dst[(py * w + x1 - 1) as usize] = 0xFFFFFF;
    }
}
#[inline]
fn dim(c: u32) -> u32 {
    let f = |v: u32| (v as f32 * 0.35) as u32;
    (f((c >> 16) & 0xff) << 16) | (f((c >> 8) & 0xff) << 8) | f(c & 0xff)
}
fn draw_stretch(
    dst: &mut [u32],
    buf_w: u32,
    dest: (u32, u32, u32, u32),
    src: Source,
    crop: (f32, f32, f32, f32),
) {
    let (dx, dy, dw, dh) = dest;
    let (ox, oy, cw, ch) = crop;
    let (sw, sh, stride) = (src.w, src.h, src.stride);
    for y in 0..dh {
        let sy = oy + (y as f32 + 0.5) * ch / dh as f32;
        let y0 = sy as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f32;
        for x in 0..dw {
            let sx = ox + (x as f32 + 0.5) * cw / dw as f32;
            let x0 = sx as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f32;
            let r0 = (y0 * stride) as usize;
            let r1 = (y1 * stride) as usize;
            let p0 = (x0 * 4) as usize;
            let p1 = (x1 * 4) as usize;
            let c00 = bgra(&src.data[r0 + p0..r0 + p0 + 4]);
            let c10 = bgra(&src.data[r0 + p1..r0 + p1 + 4]);
            let c01 = bgra(&src.data[r1 + p0..r1 + p0 + 4]);
            let c11 = bgra(&src.data[r1 + p1..r1 + p1 + 4]);
            dst[((dy + y) * buf_w + dx + x) as usize] = bilerp(c00, c10, c01, c11, fx, fy);
        }
    }
}
#[inline]
fn bgra(p: &[u8]) -> u32 {
    (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32
}
fn bilerp(c00: u32, c10: u32, c01: u32, c11: u32, fx: f32, fy: f32) -> u32 {
    let blend = |a: u32, b: u32, t: f32| (a as f32 * (1.0 - t) + b as f32 * t) as u32;
    let top = (
        blend((c00 >> 16) & 0xff, (c10 >> 16) & 0xff, fx),
        blend((c00 >> 8) & 0xff, (c10 >> 8) & 0xff, fx),
        blend(c00 & 0xff, c10 & 0xff, fx),
    );
    let bot = (
        blend((c01 >> 16) & 0xff, (c11 >> 16) & 0xff, fx),
        blend((c01 >> 8) & 0xff, (c11 >> 8) & 0xff, fx),
        blend(c01 & 0xff, c11 & 0xff, fx),
    );
    let r = blend(top.0, bot.0, fy);
    let g = blend(top.1, bot.1, fy);
    let b = blend(top.2, bot.2, fy);
    (r << 16) | (g << 8) | b
}
fn start_portal(
    frame: Arc<Mutex<Option<Frame>>>,
    requesting: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    proxy: EventLoopProxy<()>,
) {
    std::thread::spawn(move || {
        stop.store(false, Ordering::SeqCst);
        let rt = match tokio::runtime::Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("tokio runtime failed: {e}");
                requesting.store(false, Ordering::SeqCst);
                return;
            }
        };
        let result = rt.block_on(async {
            let portal = Screencast::new().await?;
            let session = portal.create_session(Default::default()).await?;
            portal
                .select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(CursorMode::Embedded)
                        .set_sources(SourceType::Monitor | SourceType::Window | SourceType::Virtual)
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::DoNot),
                )
                .await?;
            let response = portal
                .start(&session, None, Default::default())
                .await?
                .response()?;
            let stream = response
                .streams()
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no stream selected"))?;
            let node_id = stream.pipe_wire_node_id();
            let size = stream.size().unwrap_or((0, 0));
            let fd = portal
                .open_pipe_wire_remote(&session, Default::default())
                .await?;
            Ok::<_, anyhow::Error>((node_id, size, fd, portal, session))
        });
        match result {
            Ok((node_id, size, fd, portal, session)) => {
                streaming.store(true, Ordering::SeqCst);
                requesting.store(false, Ordering::SeqCst);
                let _keep_alive = (rt, portal, session);
                run_capture(node_id, size, fd, proxy, frame, stop);
            }
            Err(e) => {
                eprintln!("capture failed: {e}");
                requesting.store(false, Ordering::SeqCst);
            }
        }
    });
}
fn run_capture(
    node_id: u32,
    size: (i32, i32),
    fd: std::os::fd::OwnedFd,
    proxy: EventLoopProxy<()>,
    frame: Arc<Mutex<Option<Frame>>>,
    stop: Arc<AtomicBool>,
) {
    let fd_raw = fd.into_raw_fd();
    let mut tm = match PipeWireThreadManager::new(fd_raw) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pipewire thread failed: {e}");
            return;
        }
    };
    let (w, h) = (size.0.max(0) as u32, size.1.max(0) as u32);
    let config = StreamConfig::new("r_peek")
        .with_resolution(w, h)
        .with_dmabuf(true)
        .with_buffer_count(3);
    let (response_tx, response_rx) = std::sync::mpsc::sync_channel(1);
    if let Err(e) = tm.send_command(PipeWireThreadCommand::CreateStream {
        stream_id: 0,
        node_id,
        config,
        response_tx,
    }) {
        eprintln!("pipewire create_stream failed: {e}");
        return;
    }
    match response_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("pipewire stream failed: {e}");
            return;
        }
        Err(_) => {
            eprintln!("pipewire thread died");
            return;
        }
    }
    eprintln!("streaming node {node_id} (fd={fd_raw})");
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Some(mut f) = tm.recv_frame_timeout(Duration::from_millis(16)) else {
            continue;
        };
        while let Some(nf) = tm.try_recv_frame() {
            f = nf;
        }
        let Some(data) = f.data().cloned() else {
            continue;
        };
        let (w, h, stride) = (f.width, f.height, f.stride);
        if let Ok(mut f) = frame.lock() {
            *f = Some(Frame { w, h, stride, data });
        }
        proxy.send_event(()).ok();
    }
    let _ = tm.shutdown();
}
fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        window: None,
        surface: None,
        proxy: event_loop.create_proxy(),
        frame: Arc::new(Mutex::new(None)),
        requesting: Arc::new(AtomicBool::new(false)),
        streaming: Arc::new(AtomicBool::new(false)),
        stop: Arc::new(AtomicBool::new(false)),
        cursor: (0.0, 0.0),
        selecting: None,
        crop: [0.0; 4],
        mode: Mode::Stretch,
    };
    event_loop.run_app(&mut app).unwrap();
}
