#[macro_use]
mod debug;
mod backend;
mod hotkey_manager;
mod models;
mod tray;
mod ui;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use backend::detection::{detect_session, select_backend_name};
use backend::x11::X11Backend;
use backend::WindowBackend;
use gtk4::prelude::*;
use hotkey_manager::HotkeyManager;
use tray::WindowkeyerTray;
use ui::UiCommand;

fn main() {
    let debug_enabled = std::env::args().any(|arg| arg == "--debug");
    debug::DEBUG.store(debug_enabled, std::sync::atomic::Ordering::Relaxed);

    let session = detect_session();
    debugln!("[windowkeyer] {}", session.summary());
    if let Some(warning) = session.warning() {
        debugln!("[windowkeyer] WARNUNG: {warning}");
    }

    let backend_result: Result<Arc<dyn WindowBackend>, String> = {
        let backend_name = select_backend_name(&session);

        match backend_name {
            "kde_wayland" => {
                debugln!("[windowkeyer] Versuche KDE Plasma Wayland Backend...");
                X11Backend::new()
                .map(|b| Arc::new(b) as Arc<dyn WindowBackend>)
                .map_err(|e| e.to_string())
            }
            "x11" => X11Backend::new()
            .map(|b| Arc::new(b) as Arc<dyn WindowBackend>)
            .map_err(|e| e.to_string()),
            _ => Err(format!("Backend '{}' wird noch nicht unterstützt", backend_name))
        }
    };

    let app = gtk4::Application::builder()
    .application_id("de.windowkeyer.Windowkeyer")
    .build();

    match backend_result {
        Ok(backend) => {
            let backend_for_ui = Arc::clone(&backend);
            let session_for_ui = session.clone();
            let hotkey_manager = Arc::new(Mutex::new(HotkeyManager::new()));

            let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<UiCommand>();

            app.connect_activate(move |app| {
                // 1. Hauptfenster erstellen
                let _window = ui::build_ui(
                    app,
                    Arc::clone(&backend_for_ui),
                                           session_for_ui.clone(),
                                           Arc::clone(&hotkey_manager),
                                           commands_tx.clone(),
                );

                // 2. Natives KDE Tray-Icon starten
                let tray = WindowkeyerTray {
                    show_tx: commands_tx.clone(),
                                 quit_tx: commands_tx.clone(),
                };
                let service = ksni::TrayService::new(tray);

                // Startet einen eigenen Hintergrund-Thread für D-Bus, blockiert hier nicht!
                service.spawn();
                debugln!("[windowkeyer] KDE Status Notifier Tray gestartet");

                // 3. Tray-Events im GLib Main Loop verarbeiten
                let commands_rx_clone = commands_rx.clone();
                let app_clone = app.clone();

                glib::timeout_add_local(Duration::from_millis(100), move || {
                    while let Ok(cmd) = commands_rx_clone.try_recv() {
                        match cmd {
                            UiCommand::ShowWindow => {
                                if let Some(win) = app_clone.active_window() {
                                    win.present();
                                }
                            }
                            UiCommand::Quit => {
                                app_clone.quit();
                            }
                            _ => {}
                        }
                    }
                    glib::ControlFlow::Continue
                });
            });

            app.run();
            backend.stop_events();
        }
        Err(message) => {
            app.connect_activate(move |app| {
                ui::show_error(app, &message);
            });
            app.run();
        }
    }
}
