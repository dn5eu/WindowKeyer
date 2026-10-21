use std::collections::HashMap;

use crate::backend::{BackendError, WindowBackend};
use crate::models::{Shortcut, WindowId};

pub struct HotkeyManager {
	bindings: HashMap<Shortcut, WindowId>,
	window_bindings: HashMap<WindowId, Vec<Shortcut>>,
}

impl HotkeyManager {
	pub fn new() -> Self {
		Self {
			bindings: HashMap::new(),
			window_bindings: HashMap::new(),
		}
	}

	pub fn assign(
		&mut self,
		shortcut: Shortcut,
		window_id: WindowId,
		backend: &dyn WindowBackend,
	) -> Result<Option<Shortcut>, BackendError> {
		let mut overwritten_shortcut = None;

		if let Some(old_window_id) = self.bindings.insert(shortcut.clone(), window_id) {
			if old_window_id != window_id {
				self.remove_from_reverse_map(old_window_id, &shortcut);
				let _ = backend.ungrab_hotkey(&shortcut);
				overwritten_shortcut = Some(shortcut.clone());
			}
		}

		backend.grab_hotkey(&shortcut)?;

		self.window_bindings
		.entry(window_id)
		.or_default()
		.push(shortcut);

		Ok(overwritten_shortcut)
	}

	pub fn on_window_closed(&mut self, window_id: WindowId, backend: &dyn WindowBackend) {
		self.remove_all_for_window(window_id, backend);
	}

	pub fn remove_all_for_window(&mut self, window_id: WindowId, backend: &dyn WindowBackend) {
		if let Some(shortcuts) = self.window_bindings.remove(&window_id) {
			for shortcut in shortcuts {
				self.bindings.remove(&shortcut);
				let _ = backend.ungrab_hotkey(&shortcut);
			}
		}
	}

	pub fn find_shortcut(&self, keycode: u8, modifiers: u16) -> Option<WindowId> {
		for (shortcut, window_id) in &self.bindings {
			if shortcut.keycode == keycode {
				let event_base = modifiers & !18; // Ignoriere CapsLock (2) und NumLock (16)
				let shortcut_base = shortcut.modifiers & !18;
				if event_base == shortcut_base {
					return Some(*window_id);
				}
			}
		}
		None
	}

	pub fn get_shortcut_for_window(&self, window_id: WindowId) -> Option<String> {
		self.window_bindings
		.get(&window_id)
		.and_then(|shortcuts| shortcuts.first())
		.map(|s| s.display_text.clone())
	}

	fn remove_from_reverse_map(&mut self, window_id: WindowId, shortcut: &Shortcut) {
		if let Some(shortcuts) = self.window_bindings.get_mut(&window_id) {
			shortcuts.retain(|s| s != shortcut);
			if shortcuts.is_empty() {
				self.window_bindings.remove(&window_id);
			}
		}
	}
}
