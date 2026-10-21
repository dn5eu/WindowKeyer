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

		// KORREKTUR: Idempotenz — gleicher Shortcut + gleiches Fenster erneut
		// zugewiesen erzeugt sonst doppelte Einträge in der Rückwärtsliste und
		// redundante Grabs (Grab-Leak bei mehrfachem "X"-Entfernen/Neuzuweisen).
		if let Some(existing) = self.window_bindings.get(&window_id) {
			if existing.contains(&shortcut) {
				return Ok(None);
			}
		}

		if let Some(old_window_id) = self.bindings.insert(shortcut.clone(), window_id) {
			if old_window_id != window_id {
				self.remove_from_reverse_map(old_window_id, &shortcut);
				let _ = backend.ungrab_hotkey(&shortcut);
				overwritten_shortcut = Some(shortcut.clone());
			}
		}

		backend.grab_hotkey_for_window(&shortcut, window_id)?;

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
		// KORREKTUR: !18 war eine decimal-Bitmaske (0b10010) und hat dadurch
		// Mod1=Alt (0x0008) und Mod4=Super (0x0040) mit maskiert — Alt-/Super-
		// Hotkeys wurden ignoriert bzw. falsch gematcht. Richtig sind die Lock-Bits:
		// ShiftLock/CapsLock (0x0002), Lock/NumLock (0x0010), Mod5/ScrollLock (0x2000).
		const LOCK_MASK: u16 = 0x0002 | 0x0010 | 0x2000;
		for (shortcut, window_id) in &self.bindings {
			if shortcut.keycode == keycode {
				let event_base = modifiers & !LOCK_MASK;
				let shortcut_base = shortcut.modifiers & !LOCK_MASK;
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
