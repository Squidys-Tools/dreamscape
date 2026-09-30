//! iced host. The reference configuration.
//!
//! iced 0.14's wgpu renderer is wgpu 27, the same version as the canvas, so the
//! canvas shares one device with the chrome and nothing is copied through the
//! CPU. That is the property being tested, and it is a first-party guarantee
//! rather than something a fork has to arrange.
//!
//! Version matters more than it looks. iced 0.13 is on wgpu 0.19 and therefore
//! cannot share a device with a modern wgpu canvas at all.
//!
//! # This used to have an Explorer and an Inspector
//!
//! It was windowed chrome around the canvas, and it made the window unusable.
//! 1,000 button widgets were rebuilt and laid out on every frame, because iced's
//! `column` is not virtualised, so the chrome cost more than the canvas it was
//! framing. That is fatal for the one thing this host exists to answer, which is
//! whether the canvas draws at an interactive frame rate in a native window.
//!
//! So it is the canvas and the window, nothing else. The per-frame numbers go to
//! stdout from the render path, which costs nothing to produce and is not rebuilt
//! sixty times a second. Frame times for the canvas come from
//! `scripts/bench.ps1`; a panel duplicating them was always going to disagree.
//!
//! Pointer and wheel are not wired up here yet. The browser host already proves
//! the seam carries them, and doing it again in iced means a second event path to
//! keep in step. It is the obvious next step for this host and it is not a small
//! one.

mod canvas_widget;

use std::sync::{Arc, Mutex};

use canvas_app::{AppState, Config};
use iced::{Element, Fill, Task, Theme};

use canvas_widget::{CanvasProgram, Shared};

/// The canvas renders this many items, matching the headless benchmark.
const ITEMS: u32 = 8_000;

struct App {
    shared: Arc<Mutex<Shared>>,
}

/// There is no message, because there is no chrome to send one. iced still wants
/// a type, and an uninhabited one is the honest answer.
#[derive(Debug, Clone)]
enum Msg {}

/// iced 0.14 dropped the `Application` trait: `iced::application` takes plain
/// functions, so these are inherent methods.
impl App {
    fn new() -> Self {
        let app = AppState::new(Config {
            items: ITEMS,
            textures: ITEMS,
            ..Default::default()
        });
        Self {
            shared: Arc::new(Mutex::new(Shared::new(app))),
        }
    }

    fn update(&mut self, message: Msg) -> Task<Msg> {
        match message {}
    }

    fn view(&self) -> Element<'_, Msg, Theme, iced_wgpu::Renderer> {
        // `opaque` so the widget is the event target for the area it fills,
        // rather than events falling through to whatever is behind it.
        iced::widget::opaque(
            iced::widget::Shader::<Msg, _>::new(CanvasProgram::new(self.shared.clone()))
                .width(Fill)
                .height(Fill),
        )
    }
}

fn main() -> iced::Result {
    // iced_wgpu reports surface and swapchain problems through `log`. Without a
    // logger installed every one of those diagnostics is silently dropped, which
    // makes a window that never draws look like a silent success.
    env_logger::init();
    iced::application::<App, Msg, Theme, iced_wgpu::Renderer>(App::new, App::update, App::view)
        .title("dreamscape spike - host-iced")
        .antialiasing(true)
        .run()
}
