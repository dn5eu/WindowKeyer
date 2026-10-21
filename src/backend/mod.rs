pub mod detection;
pub mod x11;
pub mod kde_wayland; // Platzhalter, erstellen wir gleich

use crate::models::{Shortcut, WindowId, WindowInfo};
use crossbeam_channel::Sender;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("Verbindung zum Window Server fehlgeschlagen: {0}")]
    Connection(String),
    #[error("X11-Anfrage fehlgeschlagen: {0}")]
    X11(String),
    #[error("Kein EWMH-kompatibler Window Manager gefunden.")]
    NoEwmh,
    #[error("Fenster {0} existiert nicht mehr.")]
    NoSuchWindow(WindowId),
    #[error("Nicht unterstützt: {0}")]
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    WindowsChanged,
    WindowClosed(WindowId),
    BackendLost(String),
    KeyPress(u8, u16), // keycode, modifiers
}

pub trait WindowBackend: Send + Sync {
    fn name(&self) -> &str;
    fn window_manager_name(&self) -> &str { "unbekannt" }
    fn list_windows(&self) -> Result<Vec<WindowInfo>, BackendError>;
    fn activate_window(&self, id: WindowId) -> Result<(), BackendError>;
    fn minimize_window(&self, id: WindowId) -> Result<(), BackendError>;

    fn start_events(&self, _tx: Sender<BackendEvent>) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("Events nicht unterstützt".into()))
    }
    fn stop_events(&self) {}

    fn create_shortcut(&self, display_text: String, keycode: u8, modifiers: u16) -> Result<Shortcut, BackendError>;
    fn grab_hotkey(&self, shortcut: &Shortcut) -> Result<(), BackendError>;
    fn ungrab_hotkey(&self, shortcut: &Shortcut) -> Result<(), BackendError>;
}
