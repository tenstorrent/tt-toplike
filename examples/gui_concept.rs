// Example GUI structure using iced
// This shows how to integrate the existing backend with a native GUI

use iced::{
    widget::{canvas, column, container, row, text},
    Application, Command, Element, Length, Settings, Theme,
};

// Your existing backend trait works perfectly!
use tt_toplike_rs::backend::TelemetryBackend;
use tt_toplike_rs::models::{Device, Telemetry};

struct TTTopGUI {
    backend: Box<dyn TelemetryBackend>,
    devices: Vec<Device>,
    selected_device: usize,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,           // Periodic update
    SelectDevice(usize),
    ToggleVisualization,
}

impl Application for TTTopGUI {
    type Message = Message;
    type Theme = Theme;
    type Executor = iced::executor::Default;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        // Use your existing backend selection logic
        let mut backend = create_backend_from_cli();
        backend.init().expect("Backend init failed");
        let devices = backend.devices().to_vec();

        (
            Self {
                backend,
                devices,
                selected_device: 0,
            },
            Command::none(),
        )
    }

    fn title(&self) -> String {
        "TT-Toplike - Hardware Monitor".to_string()
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Tick => {
                // Your existing update logic works!
                self.backend.update().ok();
                self.devices = self.backend.devices().to_vec();
            }
            Message::SelectDevice(idx) => {
                self.selected_device = idx;
            }
            Message::ToggleVisualization => {
                // Toggle between table view and psychedelic viz
            }
        }
        Command::none()
    }

    fn view(&self) -> Element<Message> {
        let device = &self.devices[self.selected_device];
        let telemetry = self.backend.telemetry(device.index);

        // Device selector tabs
        let device_tabs = row(
            self.devices
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    button(text(format!("Device {}", i)))
                        .on_press(Message::SelectDevice(i))
                })
                .collect::<Vec<_>>()
        );

        // Telemetry display (styled like your TUI)
        let telemetry_view = if let Some(telem) = telemetry {
            column![
                text(format!("Power: {:.1}W", telem.power.unwrap_or(0.0)))
                    .size(24)
                    .style(power_color(telem.power)),
                text(format!("Temp: {:.1}°C", telem.asic_temperature.unwrap_or(0.0)))
                    .size(24)
                    .style(temp_color(telem.asic_temperature)),
                text(format!("Current: {:.1}A", telem.current.unwrap_or(0.0)))
                    .size(20),
                // Psychedelic visualization canvas
                canvas(StarfieldCanvas::new(telem, device))
                    .height(Length::Fill)
                    .width(Length::Fill),
            ]
        } else {
            column![text("No telemetry available")]
        };

        container(
            column![
                device_tabs,
                telemetry_view,
            ]
        )
        .into()
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        // Periodic updates at your configured interval
        iced::time::every(std::time::Duration::from_millis(100))
            .map(|_| Message::Tick)
    }
}

// Helper to create backend (reuse your existing logic)
fn create_backend_from_cli() -> Box<dyn TelemetryBackend> {
    // Your existing backend selection from cli.rs
    unimplemented!("Use your BackendType::Auto logic here")
}

// Reuse your color functions!
fn power_color(power: Option<f32>) -> iced::Color {
    let p = power.unwrap_or(0.0);
    if p > 150.0 {
        iced::Color::from_rgb8(255, 100, 100) // Red
    } else if p > 100.0 {
        iced::Color::from_rgb8(255, 180, 100) // Orange
    } else if p > 50.0 {
        iced::Color::from_rgb8(100, 180, 255) // Blue
    } else {
        iced::Color::from_rgb8(80, 220, 200) // Teal
    }
}

fn temp_color(temp: Option<f32>) -> iced::Color {
    let t = temp.unwrap_or(0.0);
    if t > 80.0 {
        iced::Color::from_rgb8(255, 100, 100) // Red
    } else if t > 65.0 {
        iced::Color::from_rgb8(255, 180, 100) // Orange
    } else if t > 45.0 {
        iced::Color::from_rgb8(150, 220, 100) // Green-yellow
    } else {
        iced::Color::from_rgb8(80, 220, 220) // Cyan
    }
}

// Your psychedelic visualizations can be adapted to Canvas!
struct StarfieldCanvas {
    // Reuse your Starfield struct
}

impl canvas::Program<Message> for StarfieldCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: iced::Rectangle,
        _cursor: canvas::Cursor,
    ) -> Vec<canvas::Geometry> {
        // Render your starfield using canvas primitives
        // Your existing animation logic works here!
        vec![]
    }
}

fn main() -> iced::Result {
    TTTopGUI::run(Settings::default())
}
