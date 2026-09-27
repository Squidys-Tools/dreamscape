//! iced host. The reference configuration.
//!
//! iced 0.14's wgpu renderer is wgpu 27, the same version as the canvas, so the
//! canvas shares one device with the chrome and nothing is copied through the
//! CPU. That is the property being tested, and it is a first-party guarantee
//! rather than something a fork has to arrange.
//!
//! Version matters more than it looks. iced 0.13 is on wgpu 0.19 and therefore
//! cannot share a device with a modern wgpu canvas at all.

mod canvas_widget;

use std::sync::{Arc, Mutex};

use canvas_app::{AppState, Config};
use iced::{
    widget::{self, column, container, row, scrollable, text, text_input},
    Center, Element, Fill, Task, Theme,
};

use canvas_widget::{CanvasProgram, Shared};

/// The canvas renders this many items, matching the headless benchmark.
const ITEMS: u32 = 8_000;

/// The Explorer list size. iced's `column` is not virtualised, so it lays out
/// every row on every frame; 1000 is as many as it can carry at an interactive
/// frame rate. The same constant is used by the other two hosts so the chrome
/// cost stays comparable.
const ROWS: u32 = 1_000;

struct App {
    shared: Arc<Mutex<Shared>>,
    search: String,
    selected: Option<u32>,
    /// Labels, not elements: `iced::Element` is not `Clone`, so the row widgets
    /// are rebuilt each frame, which is the cost being measured.
    labels: Vec<String>,
}

#[derive(Debug, Clone)]
enum Msg {
    Search(String),
    Select(u32),
    Toggle,
}

/// iced 0.14 dropped the `Application` trait: `iced::application` takes plain
/// functions, so these are inherent methods.
impl App {
    fn new() -> Self {
        let app = AppState::new(Config {
            items: ITEMS,
            textures: ITEMS,
            ..Default::default()
        });
        let labels = (0..ROWS)
            .map(|i| format!("ref-{i:05}   {}", 140 + (i * 37 % 280)))
            .collect();
        Self {
            shared: Arc::new(Mutex::new(Shared::new(app))),
            labels,
            search: String::new(),
            selected: None,
        }
    }

    fn update(&mut self, message: Msg) -> Task<Msg> {
        match message {
            Msg::Search(s) => {
                self.search = s.clone();
                self.shared.lock().unwrap().app.search = s;
            }
            Msg::Select(i) => {
                self.selected = Some(i);
                let mut sh = self.shared.lock().unwrap();
                sh.app.selected.clear();
                sh.app.selected.insert(i);
            }
            Msg::Toggle => {
                let mut sh = self.shared.lock().unwrap();
                sh.app.animate = !sh.app.animate;
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Msg, Theme, iced_wgpu::Renderer> {
        // Read the status here rather than on a timer: the canvas animates, so
        // there is a fresh frame every time this runs anyway.
        let (animating, status) = {
            let sh = self.shared.lock().unwrap();
            let (mean, _p50, p99, max, over) = sh.metrics.percentiles(16.67);
            let visible = sh.metrics.total_visible().max(1);
            let (w, h) = sh.last_size;
            (
                sh.app.animate,
                format!(
                    "frames {}\nmean {:.2}ms   p99 {:.2}ms\nmax {:.2}ms   over budget {}\n\
                     peak visible {}\nplaceholders {:.2}%\nuploads {}   evictions {}\ncanvas {w}x{h}",
                    sh.metrics.len(),
                    mean,
                    p99,
                    max,
                    over,
                    sh.metrics.peak_visible,
                    100.0 * sh.metrics.total_placeholder() as f64 / visible as f64,
                    sh.last_uploads,
                    sh.last_evictions,
                ),
            )
        };

        let sidebar = container(
            column![
                text("Explorer").size(14),
                text_input("search", &self.search).on_input(Msg::Search),
                scrollable(column(
                    self.labels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            widget::button(text(l).size(12))
                                .width(Fill)
                                .on_press(Msg::Select(i as u32))
                                .into()
                        })
                        .collect::<Vec<_>>(),
                )),
            ]
            .spacing(6)
            .padding(8),
        )
        .width(280)
        .height(Fill);

        let inspector = container(
            column![
                text("Inspector").size(14),
                text(match self.selected {
                    Some(i) =>
                        format!("ref-{i:05}\n\nsize    140-420 px\nmip     0-6\nz       0-6"),
                    None => "nothing selected".into(),
                })
                .size(12),
                text(status).size(11),
            ]
            .spacing(8)
            .padding(8),
        )
        .width(300)
        .height(Fill);

        let toolbar = container(
            row![
                text("dreamscape").size(14),
                widget::button(if animating { "Pause" } else { "Play" }).on_press(Msg::Toggle),
                text(format!(
                    "  {ITEMS} items, {ITEMS} distinct  ·  iced 0.14 / wgpu 27  ·  shared device"
                ))
                .size(12),
            ]
            .spacing(10)
            .align_y(Center),
        )
        .padding(6)
        .width(Fill);

        let canvas = iced::widget::opaque(
            widget::Shader::<Msg, _>::new(CanvasProgram::new(self.shared.clone()))
                .width(Fill)
                .height(Fill),
        );

        container(column![
            toolbar,
            row![sidebar, canvas, inspector].height(Fill)
        ])
        .into()
    }
}

fn main() -> iced::Result {
    // iced_wgpu reports surface and swapchain problems through `log`. Without a
    // logger installed every one of those diagnostics is silently dropped, which
    // makes a window that never draws look like a silent success.
    env_logger::init();
    iced::application::<App, Msg, Theme, iced_wgpu::Renderer>(App::new, App::update, App::view)
        .title("dreamscape spike — host-iced")
        .antialiasing(true)
        .run()
}
