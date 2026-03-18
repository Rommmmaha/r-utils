mod draw;
mod network;
mod wayland;
use crate::draw::CanvasState;
use crate::network::Command;
use crate::wayland::WaylandApp;
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use clap::Parser;
use crossbeam::channel::Receiver;
use tokio::runtime::Builder as RuntimeBuilder;
#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(long)]
    udp: Option<u16>,
    #[arg(long)]
    socket: Option<String>,
}
struct AppData {
    canvas: CanvasState,
    receiver: Receiver<Command>,
    app: WaylandApp,
    frame_count: u64,
    dirty: bool,
}
fn main() {
    env_logger::init();
    let args = Args::parse();
    if args.udp.is_none() && args.socket.is_none() {
        eprintln!("Error: You must provide at least one of --udp or --socket");
        std::process::exit(1);
    }
    log::info!("Starting wayland overlay server");
    let (mut app, event_queue, conn) = WaylandApp::new(1920, 1080);
    let qh = event_queue.handle();
    app.create_layer_surface(&qh);
    log::info!("Layer surface created");
    let (sender, receiver) = crossbeam::channel::unbounded();
    let canvas = CanvasState::new();
    let rt = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let udp = args.udp;
    let socket = args.socket.clone();
    log::info!(
        "Starting network listeners - UDP: {:?}, Socket: {:?}",
        udp,
        socket
    );
    rt.spawn(async move {
        if let Err(e) = crate::network::start_listeners(udp, socket.as_deref(), sender).await {
            eprintln!("Network error: {}", e);
        }
    });
    let mut data = AppData {
        canvas,
        receiver,
        app,
        frame_count: 0,
        dirty: false,
    };
    let mut event_loop: EventLoop<AppData> = EventLoop::try_new().unwrap();
    let wayland_source = WaylandSource::new(conn, event_queue);
    event_loop
        .handle()
        .insert_source(wayland_source, |_, queue, data| {
            let count = queue.dispatch_pending(&mut data.app).map_err(|e| {
                log::error!("Wayland dispatch error: {}", e);
                e
            })?;
            Ok(count)
        })
        .expect("Failed to insert wayland source");
    log::info!("Starting main event loop");
    let timer = calloop::timer::Timer::from_duration(std::time::Duration::from_millis(16));
    event_loop
        .handle()
        .insert_source(timer, |_, _, data| {
            if data.frame_count % 60 == 0 {
                log::debug!(
                    "Frame {}: configured={}, width={}, height={}",
                    data.frame_count,
                    data.app.configured,
                    data.app.width,
                    data.app.height
                );
            }
            let pruned = data.canvas.prune();
            if pruned {
                data.dirty = true;
            }
            let mut cmd_count = 0;
            while let Ok(cmd) = data.receiver.try_recv() {
                log::info!(
                    "Received command with {} operations on layer {:?}",
                    cmd.operations.len(),
                    cmd.layer
                );
                data.canvas.update(cmd);
                data.dirty = true;
                cmd_count += 1;
            }
            if cmd_count > 0 {
                log::info!("Processed {} commands", cmd_count);
            }
            if data.app.needs_render || cmd_count > 0 || pruned || data.dirty {
                data.canvas.render(&mut data.app.renderer);
                data.app.render_if_configured();
                data.app.needs_render = false;
                data.dirty = false;
            }
            data.frame_count += 1;
            calloop::timer::TimeoutAction::ToDuration(std::time::Duration::from_millis(16))
        })
        .expect("Failed to insert timer");
    event_loop
        .run(None, &mut data, |_| {})
        .expect("Failed to run event loop");
}
