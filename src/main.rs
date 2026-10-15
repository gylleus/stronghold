//! Stronghold binary: sets up logging (stderr and the log file) and launches the eframe window.

use stronghold::{APP_NAME, StrongholdApp};

fn main() -> eframe::Result {
    stronghold::legacy::carry_over_data_dir();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = stronghold::claude::hook::run_cli(&args)
        .or_else(|| stronghold::control::client::run_cli(&args))
        .or_else(|| stronghold::mcp::run_cli(&args))
        .or_else(|| stronghold::mcp::standup::run_cli(&args))
        .or_else(|| stronghold::playbook::run_cli(&args))
        .or_else(|| stronghold::notify::run_cli(&args))
        .or_else(|| stronghold::voice::tts::run_cli(&args))
        .or_else(|| stronghold::review::run_cli(&args));
    if let Some(result) = cli {
        if let Err(e) = result {
            eprintln!("stronghold: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

    stronghold::logfile::init();
    stronghold::notify::init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        // A test instance with its own data dir keeps its UI state there too.
        persistence_path: stronghold::legacy::env_os("STRONGHOLD_DATA_DIR")
            .filter(|d| !d.is_empty())
            .map(|d| std::path::PathBuf::from(d).join("app.ron")),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(StrongholdApp::new(cc)))),
    )
}
