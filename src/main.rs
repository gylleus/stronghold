//! Myrstack binary: sets up logging and launches the eframe window.

use myrstack::{APP_NAME, MyrstackApp};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(result) = myrstack::claude::hook::run_cli(&args) {
        if let Err(e) = result {
            eprintln!("myrstack: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(MyrstackApp::new(cc)))),
    )
}
