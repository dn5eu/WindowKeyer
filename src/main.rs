#[macro_use]
mod debug;
mod backend;
mod hotkey_manager;
mod icon_loader;
mod models;
mod tray;
mod ui;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use backend::detection::{detect_session, select_backend_name};
use backend::kde_wayland::KdeWaylandBackend;
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
                // Vorab-Check: ohne erreichbares KWin-Wayland gar nicht erst
                // eine Hotkey-Collection in KWin anlegen, sondern direkt
                // sauber auf X11/XWayland zurückfallen.
                if !KdeWaylandBackend::is_available() {
                    debugln!("[windowkeyer] Kein erreichbares KWin (Wayland) — Fallback auf X11/XWayland");
                    X11Backend::new()
                        .map(|b| Arc::new(b) as Arc<dyn WindowBackend>)
                        .map_err(|e| format!("KDE-Wayland-Backend nicht verfügbar; X11-Fallback fehlgeschlagen: {e}"))
                } else {
                    match KdeWaylandBackend::new() {
                        Ok(b) => Ok(Arc::new(b) as Arc<dyn WindowBackend>),
                        Err(e) => {
                            // Graceful Fallback: natives Backend nicht verfügbar
                            // (z.B. Plasma < mit GlobalShortcutManager oder kein
                            // D-Bus) → XWayland/X11-Backend verwenden.
                            debugln!("[windowkeyer] KDE-Wayland-Backend nicht verfügbar ({e}); fallback auf X11/XWayland");
                            X11Backend::new()
                                .map(|b| Arc::new(b) as Arc<dyn WindowBackend>)
                                .map_err(|e2| format!("{e} — X11-Fallback ebenfalls fehlgeschlagen: {e2}"))
                        }
                    }
                }
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
            // KORREKTUR: Tray-Timer/Receiver nur EINMAL einrichten. Vorher wurde bei
            // jeder Re-Aktivierung (Tray-Klick nach Fenster-Schließen) ein zweiter
            // Timer mit eigenem Receiver-Klon erzeugt, der mit dem ersten um die
            // Befehle konkurrierte. Die KSNI-TrayService-Instanz wird ebenfalls nur
            // einmal gestartet (identische D-Bus-Namen führen sonst zu Konflikten).
            // KORREKTUR: In glib 0.20 liefert timeout_add_local eine Closure, die
            // ControlFlow zurueckgibt — zum Entfernen des Sources dient
            // glib::Source::remove(&id) (id.destroy() existiert nicht mehr).
            let tray_source_id = std::rc::Rc::new(std::cell::RefCell::new(None::<glib::SourceId>));
            let tray_ready = std::rc::Rc::new(std::cell::Cell::new(false));

            app.connect_activate(move |app| {
                // 1. Hauptfenster erstellen
                let _window = ui::build_ui(
                    app,
                    Arc::clone(&backend_for_ui),
                                           session_for_ui.clone(),
                                           Arc::clone(&hotkey_manager),
                                           commands_tx.clone(),
                );

                // 3. Tray-Events im GLib Main Loop verarbeiten
                // KORREKTUR: commands_rx nur einmal bewegen und den Source an das
                // Application-Signal "shutdown" binden — bei mehrfachem activate
                // (Tray-Klick nach Fenster schließen) entsteht sonst ein zweiter
                // Receiver/Timer, der mit dem UI-Poller um die Befehle konkurriert.
                // Nur beim ersten Aktivieren einrichten: sonst konkurriert bei
                // Re-Aktivierung ein zweiter Timer um dieselben Befehle.
                if !tray_ready.get() {
                    tray_ready.set(true);
                    let tray_service = ksni::TrayService::new(WindowkeyerTray {
                        show_tx: commands_tx.clone(),
                        quit_tx: commands_tx.clone(),
                    });
                    tray_service.spawn();
                    debugln!("[windowkeyer] KDE Status Notifier Tray gestartet");

                    let commands_rx_cl = commands_rx.clone();
                    let app_clone = app.clone();
                    let source_slot = std::rc::Rc::clone(&tray_source_id);
                    app.connect_shutdown(move |_| {
                        if let Some(source_id) = source_slot.borrow_mut().take() {
                            source_id.remove();
                        }
                    });
                    *tray_source_id.borrow_mut() = Some(glib::timeout_add_local(
                        Duration::from_millis(100),
                        move || {
                            while let Ok(cmd) = commands_rx_cl.try_recv() {
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
                        },
                    ));
                }
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
