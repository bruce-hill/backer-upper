mod backup;
mod config;
mod drives;
mod ui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Backer-Upper")
            .with_inner_size([640.0, 480.0])
            .with_min_inner_size([480.0, 360.0])
            .with_decorations(false) // We draw our own title bar
            .with_transparent(false),
        ..Default::default()
    };

    eframe::run_native(
        "Backer-Upper",
        options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
}
