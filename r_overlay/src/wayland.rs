use crate::draw::Renderer;
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
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_region::WlRegion, wl_shm, wl_surface},
};
pub struct WaylandApp {
    pub registry_state: RegistryState,
    pub output_state: OutputState,
    pub compositor_state: CompositorState,
    pub layer_shell: LayerShell,
    pub shm: Shm,
    pub layer_surface: Option<LayerSurface>,
    pub surface: Option<wl_surface::WlSurface>,
    pub renderer: Renderer,
    pub width: u32,
    pub height: u32,
    pub slot_pool: Option<SlotPool>,
    pub configured: bool,
    pub needs_render: bool,
}
impl WaylandApp {
    pub fn new(width: u32, height: u32) -> (Self, EventQueue<Self>, Connection) {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, event_queue) = registry_queue_init(&conn).unwrap();
        let qh = event_queue.handle();
        let registry_state = RegistryState::new(&globals);
        let output_state = OutputState::new(&globals, &qh);
        let compositor_state = CompositorState::bind(&globals, &qh).unwrap();
        let layer_shell = LayerShell::bind(&globals, &qh).unwrap();
        let shm = Shm::bind(&globals, &qh).unwrap();
        let renderer = Renderer::new(width, height);
        (
            Self {
                registry_state,
                output_state,
                compositor_state,
                layer_shell,
                shm,
                layer_surface: None,
                surface: None,
                renderer,
                width,
                height,
                slot_pool: None,
                configured: false,
                needs_render: false,
            },
            event_queue,
            conn,
        )
    }
    pub fn create_layer_surface(&mut self, qh: &QueueHandle<Self>) {
        let surface = self.compositor_state.create_surface(qh);
        let compositor = self.compositor_state.wl_compositor();
        let region = compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        self.surface = Some(surface.clone());
        let layer_surface = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("wayland-overlay"),
            None,
        );
        layer_surface.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.commit();
        self.layer_surface = Some(layer_surface);
        self.slot_pool =
            Some(SlotPool::new(self.width as usize * self.height as usize * 4, &self.shm).unwrap());
    }
    pub fn render_if_configured(&mut self) {
        if !self.configured {
            return;
        }
        if let Some(surface) = &self.surface {
            let width = self.renderer.pixmap.width() as i32;
            let height = self.renderer.pixmap.height() as i32;
            let stride = width * 4;
            if let Some(pool) = &mut self.slot_pool {
                let size = (stride * height) as usize;
                if pool.len() < size {
                    if let Err(e) = pool.resize(size) {
                        log::error!("Failed to resize pool: {}", e);
                        return;
                    }
                }
                match pool.create_buffer(width, height, stride, wl_shm::Format::Argb8888) {
                    Ok((buffer, canvas)) => {
                        let data = self.renderer.pixmap.data();
                        for i in 0..(data.len() / 4) {
                            let r = data[i * 4];
                            let g = data[i * 4 + 1];
                            let b = data[i * 4 + 2];
                            let a = data[i * 4 + 3];
                            canvas[i * 4] = b; // B
                            canvas[i * 4 + 1] = g; // G
                            canvas[i * 4 + 2] = r; // R
                            canvas[i * 4 + 3] = a; // A
                        }
                        surface.attach(Some(buffer.wl_buffer()), 0, 0);
                        surface.damage_buffer(0, 0, width, height);
                        surface.commit();
                    }
                    Err(e) => {
                        log::error!("Failed to create buffer: {}", e);
                    }
                }
            }
        }
    }
}
impl CompositorHandler for WaylandApp {
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
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
impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {}
    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        log::info!(
            "Configure event received: size=({}, {})",
            configure.new_size.0,
            configure.new_size.1
        );
        let (w, h) = configure.new_size;
        if w > 0 && h > 0 {
            self.width = w;
            self.height = h;
            self.renderer = Renderer::new(w, h);
            self.slot_pool = Some(SlotPool::new(w as usize * h as usize * 4, &self.shm).unwrap());
            log::info!("Resized renderer to {}x{}", w, h);
        }
        self.configured = true;
        self.needs_render = true;
        log::info!("Surface configured and ready to render");
    }
}
impl ShmHandler for WaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
delegate_compositor!(WaylandApp);
delegate_output!(WaylandApp);
delegate_shm!(WaylandApp);
delegate_layer!(WaylandApp);
delegate_registry!(WaylandApp);
impl ProvidesRegistryState for WaylandApp {
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
impl Dispatch<WlRegion, ()> for WaylandApp {
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
