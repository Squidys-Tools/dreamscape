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
//! Pointer and wheel come from one `listen_with` subscription that forwards into
//! the same `AppState::pointer` and `AppState::wheel` the browser host calls, so
//! there is one seam and two callers rather than two input paths.
//!
//! iced reports positions in logical pixels and the canvas draws in device
//! pixels, so `update` scales through `Shared::scale_factor`, which `prepare`
//! takes from iced. Nothing here assumes the two match.

mod canvas_widget;

use std::sync::{Arc, Mutex};

use canvas_app::{AppState, Config, Motion};
use canvas_core::{PointerEvent, PointerPhase, Vec2, WheelEvent};
use iced::mouse::{self, Button, ScrollDelta};
use iced::{Element, Event, Fill, Point, Subscription, Task, Theme};

use canvas_widget::{CanvasProgram, Shared};

/// The canvas renders this many items, matching the headless benchmark.
const ITEMS: u32 = 8_000;

/// One wheel notch, in the pixel units `AppState::wheel` expects.
///
/// `canvas_core` scales zoom by pixels per notch, so this is the conversion that
/// makes a notch feel the same here as it does in the browser. It is the number
/// that has to agree between the two hosts, which is why it is named rather than
/// inlined at the use site.
const NOTCH_PIXELS: f32 = 100.0;

struct App {
    shared: Arc<Mutex<Shared>>,
    /// Last cursor position in logical pixels.
    ///
    /// iced's wheel event carries a delta but no position, and the canvas zooms
    /// towards a point, so without this the wheel has nothing to zoom towards.
    cursor: Option<Point>,
}

#[derive(Debug, Clone)]
enum Msg {
    /// The cursor moved, in logical pixels.
    ///
    /// Recorded rather than acted on, because iced's button and wheel events
    /// carry no position of their own and the canvas needs a point to pan or
    /// zoom towards.
    Cursor(Point),
    /// The left button went down or came up.
    Pressed(bool),
    /// A wheel delta in logical pixels, applied at `App::cursor`.
    Wheel(Vec2),
}

/// Every pointer and wheel event on the window, in one place.
///
/// A free function because iced takes `&App`, not `&mut App`.
///
/// The status argument is deliberately ignored, so this sees events the canvas
/// widget has already captured as well as the ones it has not. The widget is
/// `opaque`, so filtering to unconsumed events would deliver nothing at all. The
/// cost is that the widget stops seeing them too, which only costs it a cursor
/// shape.
fn subscription(_app: &App) -> Subscription<Msg> {
    iced::event::listen_with(|event, _status, _id| match event {
        Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Msg::Cursor(position)),
        Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => Some(Msg::Pressed(true)),
        Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => Some(Msg::Pressed(false)),
        Event::Mouse(mouse::Event::WheelScrolled { delta }) => Some(Msg::Wheel(match delta {
            ScrollDelta::Pixels { x, y } => Vec2::new(x, y),
            // Windows reports the wheel in notches, not pixels: winit turns
            // `WHEEL_DELTA` into `LineDelta`, so this is the branch a native host
            // actually takes and the other one is the browser's. Handling only
            // pixels left zoom dead on Windows while looking correct in review.
            ScrollDelta::Lines { x, y } => Vec2::new(x * NOTCH_PIXELS, y * NOTCH_PIXELS),
        })),
        _ => None,
    })
}

/// iced 0.14 dropped the `Application` trait: `iced::application` takes plain
/// functions, so these are inherent methods.
impl App {
    fn new() -> Self {
        let app = AppState::new(Config {
            items: ITEMS,
            textures: ITEMS,
            // No scripted camera. `Motion::default` is the benchmark's pan and
            // zoom sweep, and `AppState` advances it on every drawn frame, so
            // leaving it on meant this host dragged the camera 18 world units
            // every frame underneath the pointer. A gesture and the script both
            // moved the camera at once, which is also why the first attempt to
            // check whether input arrived was unreadable: the camera was never
            // still. Only the benchmark drives the camera itself.
            motion: Motion {
                pan_x: 0.0,
                pan_y: 0.0,
                zoom_sweep: 0.0,
            },
            ..Default::default()
        });
        Self {
            shared: Arc::new(Mutex::new(Shared::new(app))),
            cursor: None,
        }
    }

    fn update(&mut self, message: Msg) -> Task<Msg> {
        match message {
            Msg::Cursor(at) => {
                self.cursor = Some(at);
                // A move while the button is down is a drag; a move with nothing
                // down is just the cursor arriving, and forwarding it would pan
                // the board every time the mouse crossed the window.
                if self.is_dragging() {
                    self.forward(|app, size, scale| {
                        app.pointer(
                            PointerEvent {
                                pos: point_device(at, scale),
                                phase: PointerPhase::Move,
                            },
                            size,
                        )
                    });
                }
            }
            Msg::Pressed(down) => {
                let phase = if down {
                    PointerPhase::Down
                } else {
                    PointerPhase::Up
                };
                let at = self.cursor.unwrap_or(Point::ORIGIN);
                self.forward(|app, size, scale| {
                    app.pointer(
                        PointerEvent {
                            pos: point_device(at, scale),
                            phase,
                        },
                        size,
                    )
                });
            }
            Msg::Wheel(delta) => {
                let at = self.cursor.unwrap_or(Point::ORIGIN);
                let scale = scale_of(self.shared.as_ref());
                self.forward(|app, size, _| {
                    app.wheel(
                        WheelEvent {
                            pos: point_device(at, scale),
                            delta: delta_device(delta, scale),
                        },
                        size,
                    )
                });
            }
        }
        Task::none()
    }

    fn is_dragging(&self) -> bool {
        self.shared
            .lock()
            .expect("canvas state poisoned")
            .app
            .dragging()
    }

    /// Locks the shared state once, reads the scale factor and canvas size out of
    /// it, and lets the caller hand the event to the app in device pixels.
    fn forward(&self, f: impl FnOnce(&mut AppState, Vec2, f32)) {
        let mut guard = self.shared.lock().expect("canvas state poisoned");
        let Shared {
            app,
            scale_factor,
            last_size,
            ..
        } = &mut *guard;
        f(
            app,
            Vec2::new(last_size.0 as f32, last_size.1 as f32),
            *scale_factor,
        );
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

/// A point in logical pixels to device pixels.
fn point_device(at: Point, scale: f32) -> Vec2 {
    Vec2::new(at.x * scale, at.y * scale)
}

/// A delta in logical pixels to device pixels.
fn delta_device(d: Vec2, scale: f32) -> Vec2 {
    Vec2::new(d.x * scale, d.y * scale)
}

/// Reads the scale factor out of the shared state. Used where the caller cannot
/// already hold the guard.
fn scale_of(shared: &Mutex<Shared>) -> f32 {
    shared.lock().expect("canvas state poisoned").scale_factor
}

fn main() -> iced::Result {
    // iced_wgpu reports surface and swapchain problems through `log`. Without a
    // logger installed every one of those diagnostics is silently dropped, which
    // makes a window that never draws look like a silent success.
    env_logger::init();
    iced::application::<App, Msg, Theme, iced_wgpu::Renderer>(App::new, App::update, App::view)
        .subscription(subscription)
        .title("dreamscape spike - host-iced")
        .antialiasing(true)
        .run()
}
