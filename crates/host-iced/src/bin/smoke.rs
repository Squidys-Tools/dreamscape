//! Minimal stock iced 0.14 app. Exists to tell "iced does not render on this
//! machine" apart from "the canvas host does not render". Not part of the
//! three-way comparison.

use iced::{widget, Element, Fill, Theme};

struct Smoke;

fn view(_: &Smoke) -> Element<'_, (), Theme, iced_wgpu::Renderer> {
    // A full-bleed fill rather than bare text: this tells "nothing is drawn at
    // all" apart from "only text failed to rasterise".
    Element::from(
        widget::container(widget::text("smoke").size(32))
            .width(Fill)
            .height(Fill),
    )
}

fn update(_: &mut Smoke, _: ()) -> iced::Task<()> {
    iced::Task::none()
}

fn new() -> Smoke {
    Smoke
}

fn main() -> iced::Result {
    env_logger::init();
    iced::application::<Smoke, (), Theme, iced_wgpu::Renderer>(new, update, view)
        .window_size((640.0, 400.0))
        .run()
}
