use crossbeam_channel::Sender;
use crate::backend::{BackendError, BackendEvent, WindowBackend};
use crate::models::{Shortcut, WindowId, WindowInfo};

pub struct KdeWaylandBackend;

impl KdeWaylandBackend {
	pub fn new() -> Result<Self, BackendError> {
		Err(BackendError::Unsupported(
			"KDE Wayland Backend ist noch in Entwicklung".into()
		))
	}
}

impl WindowBackend for KdeWaylandBackend {
	fn name(&self) -> &str {
		"KDE Plasma Wayland (Platzhalter)"
	}

	fn list_windows(&self) -> Result<Vec<WindowInfo>, BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn activate_window(&self, _id: WindowId) -> Result<(), BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn minimize_window(&self, _id: WindowId) -> Result<(), BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn create_shortcut(&self, _display_text: String, _keycode: u8, _modifiers: u16) -> Result<Shortcut, BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn grab_hotkey(&self, _shortcut: &Shortcut) -> Result<(), BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn ungrab_hotkey(&self, _shortcut: &Shortcut) -> Result<(), BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}

	fn start_events(&self, _tx: Sender<BackendEvent>) -> Result<(), BackendError> {
		Err(BackendError::Unsupported("Noch nicht implementiert".into()))
	}
}
