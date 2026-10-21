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
    KeyPress(u8, u16), // keycode, modifiers (nur X11-Backend)
    /// Phase 3a: KDE-Wayland-Backend löst Hotkeys über KWin GlobalShortcuts
    /// aus; die zugeordnete Fenster-ID wird hierdurch gemeldet.
    ShortcutTriggered(WindowId),
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

    /// Grab mit bekannter Ziel-Fenster-ID. Backends, die Auslösungen selbst
    /// mappen müssen (KDE-Wayland via KWin GlobalShortcuts), überschreiben
    /// diese Methode; X11 ignoriert die ID und nutzt das KeyPress-Event.
    fn grab_hotkey_for_window(&self, shortcut: &Shortcut, _window_id: WindowId) -> Result<(), BackendError> {
        self.grab_hotkey(shortcut)
    }

    /// true wenn das Backend Hotkey-Events über `BackendEvent::KeyPress`
    /// (X11-Keycode + Modifier-Maske) liefert. false z.B. beim KDE-Wayland-
    /// Backend, das stattdessen benannte Shortcuts über KWin GlobalShortcuts
    /// nutzt und Auslösungen als `BackendEvent::ShortcutTriggered(id)` meldet.
    fn uses_x11_keycodes(&self) -> bool { true }
}
