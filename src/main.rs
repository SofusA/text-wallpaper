use clap::Parser;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use std::{
    io::{self, IsTerminal, Read},
    process,
};
use wayland_client::{
    Connection, QueueHandle, delegate_noop,
    globals::registry_queue_init,
    protocol::{wl_output, wl_region, wl_shm, wl_surface},
};

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    message: Option<String>,

    #[arg(long, default_value_t = 16.0)]
    font_size: f32,

    #[arg(long, default_value = "monospace")]
    font_family: String,

    /// Text color as a hexadecimal RGB value, for example:
    /// --color "#ffffff", --color "ffffff", or --color "ff0000"
    #[arg(long, default_value = "#ffffff", value_parser = parse_color)]
    color: Color,
}

fn parse_color(value: &str) -> Result<Color, String> {
    let value = value.strip_prefix('#').unwrap_or(value);

    if value.len() != 6 {
        return Err(format!(
            "invalid color '{value}': expected a 6-digit hexadecimal RGB value, such as #ffffff"
        ));
    }

    let red = u8::from_str_radix(&value[0..2], 16)
        .map_err(|_| format!("invalid color '{value}': invalid red component"))?;

    let green = u8::from_str_radix(&value[2..4], 16)
        .map_err(|_| format!("invalid color '{value}': invalid green component"))?;

    let blue = u8::from_str_radix(&value[4..6], 16)
        .map_err(|_| format!("invalid color '{value}': invalid blue component"))?;

    Ok(Color::rgb(red, green, blue))
}

struct Surface {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    width: u32,
    height: u32,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    surfaces: Vec<Surface>,
    message: String,
    color: Color,
    font_size: f32,
    font_family: String,
    font_system: FontSystem,
    cache: SwashCache,
}

impl App {
    fn create_surface(&mut self, output: wl_output::WlOutput, qh: &QueueHandle<Self>) {
        let wl_surface = self.compositor.create_surface(qh);
        let region = self.compositor.wl_compositor().create_region(qh, ());

        wl_surface.set_input_region(Some(&region));
        region.destroy();

        let layer = self.layer_shell.create_layer_surface(
            qh,
            wl_surface,
            Layer::Background,
            Some("text-wallpaper"),
            Some(&output),
        );

        layer.set_anchor(Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM | Anchor::LEFT);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.commit();

        self.surfaces.push(Surface {
            output,
            layer,
            width: 0,
            height: 0,
        });
    }

    fn draw(&mut self, index: usize) {
        let width = self.surfaces[index].width;
        let height = self.surfaces[index].height;

        if width == 0 || height == 0 {
            return;
        }

        let stride = width * 4;
        let size = (stride * height) as usize;

        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride as i32,
                wl_shm::Format::Argb8888,
            )
            .expect("failed to create shared-memory buffer");

        canvas[..size].fill(0);

        let metrics = Metrics::new(self.font_size, self.font_size * 1.25);
        let mut text_buffer = Buffer::new(&mut self.font_system, metrics);

        {
            let mut borrowed = text_buffer.borrow_with(&mut self.font_system);

            borrowed.set_size(Some(width as f32), Some(height as f32));
            borrowed.set_wrap(Wrap::None);
            borrowed.set_text(
                &self.message,
                &Attrs::new().family(if self.font_family.eq_ignore_ascii_case("monospace") {
                    Family::Monospace
                } else {
                    Family::Name(&self.font_family)
                }),
                Shaping::Advanced,
                None,
            );
            borrowed.shape_until_scroll(true);
        }

        let runs: Vec<_> = text_buffer.layout_runs().collect();

        let content_width = runs.iter().map(|run| run.line_w).fold(0.0_f32, f32::max);

        let content_height = runs
            .last()
            .map(|run| run.line_top + run.line_height)
            .unwrap_or(0.0);

        let x_offset = ((width as f32 - content_width) / 2.0).max(0.0) as i32;
        let y_offset = ((height as f32 - content_height) / 2.0).max(0.0) as i32;

        text_buffer.borrow_with(&mut self.font_system).draw(
            &mut self.cache,
            self.color,
            |x, y, w, h, color| {
                let alpha = color.a() as u16;
                let red = (color.r() as u16 * alpha / 255) as u8;
                let green = (color.g() as u16 * alpha / 255) as u8;
                let blue = (color.b() as u16 * alpha / 255) as u8;

                for row in 0..h as i32 {
                    for column in 0..w as i32 {
                        let px = x + x_offset + column;
                        let py = y + y_offset + row;

                        if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                            continue;
                        }

                        let offset = py as usize * stride as usize + px as usize * 4;

                        canvas[offset] = blue;
                        canvas[offset + 1] = green;
                        canvas[offset + 2] = red;
                        canvas[offset + 3] = color.a();
                    }
                }
            },
        );

        buffer
            .attach_to(self.surfaces[index].layer.wl_surface())
            .expect("failed to attach buffer");

        self.surfaces[index]
            .layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);

        self.surfaces[index].layer.commit();
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.create_surface(output, qh);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|surface| surface.output != output);
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.surfaces.retain(|surface| surface.layer != *layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.layer == *layer)
        {
            self.surfaces[index].width = configure.new_size.0;
            self.surfaces[index].height = configure.new_size.1;
            self.draw(index);
        }
    }
}

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
delegate_noop!(App: ignore wl_region::WlRegion);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

fn main() {
    let args = Args::parse();

    let message = args.message.unwrap_or_else(|| {
        if io::stdin().is_terminal() {
            eprintln!("provide --message or pipe text through stdin");
            process::exit(2);
        }

        let mut input = String::new();

        io::stdin()
            .read_to_string(&mut input)
            .expect("failed to read text from stdin");

        input.trim_end_matches(['\n', '\r']).to_owned()
    });

    let connection = Connection::connect_to_env().expect("failed to connect to Wayland");

    let (globals, mut event_queue) =
        registry_queue_init(&connection).expect("failed to initialize Wayland registry");

    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor is unavailable");

    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell is unavailable");

    let shm = Shm::bind(&globals, &qh).expect("wl_shm is unavailable");

    let pool = SlotPool::new(1024 * 1024, &shm).expect("failed to create shared-memory pool");

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        surfaces: Vec::new(),
        message,
        color: args.color,
        font_size: args.font_size,
        font_family: args.font_family,
        font_system: FontSystem::new(),
        cache: SwashCache::new(),
    };

    loop {
        event_queue
            .blocking_dispatch(&mut app)
            .expect("Wayland connection failed");
    }
}
