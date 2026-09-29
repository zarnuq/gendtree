mod app;
mod atom;
mod db;
mod depstr;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("gendtree")
            .with_app_id("gendtree")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("gendtree", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
