use crossbeam_channel::Sender;
use ksni::{Tray, ToolTip, Icon, menu::{StandardItem, MenuItem}};
use crate::ui::UiCommand;

pub struct WindowkeyerTray {
	pub show_tx: Sender<UiCommand>,
	pub quit_tx: Sender<UiCommand>,
}

impl Tray for WindowkeyerTray {
	fn id(&self) -> String {
		"windowkeyer".into()
	}

	fn title(&self) -> String {
		"Windowkeyer".into()
	}

	fn tool_tip(&self) -> ToolTip {
		ToolTip {
			title: "WindowKeyer".into(),
			//description: "Fenster per Hotkey steuern".into(),
			icon_name: "preferences-system-windows".into(),
			..Default::default()
		}
	}

	fn icon_name(&self) -> String {
		// Wenn eine eigene Icon-Datei gefunden wurde, liefert icon_pixmap()
		// den Pixelpuffer; das Theme-Fallback wird dann nicht benötigt.
		if crate::icon_loader::find_custom_icon_file().is_some() {
			return String::new();
		}
		// Fallback: freedesktop-Icon-Theme
		"preferences-system-windows".into()
	}

	/// Eigenes Tray-Icon als ARGB32-Pixelpuffer (wird von ksni via D-Bus
	/// an den StatusNotifier-Host, z. B. KDE Plasma, übergeben).
	fn icon_pixmap(&self) -> Vec<Icon> {
		match crate::icon_loader::load_custom_icon_pixmap() {
			Some((width, height, argb)) => vec![Icon { width, height, data: argb }],
			None => Vec::new(),
		}
	}

	// KORREKTUR: activate braucht zwei i32-Parameter (X/Y-Koordinaten des Klicks)
	// Wir ignorieren sie mit `_`, da wir sie nicht benötigen.
	fn activate(&mut self, _x: i32, _y: i32) {
		let _ = self.show_tx.send(UiCommand::ShowWindow);
	}

	fn menu(&self) -> Vec<MenuItem<Self>> {
		let show_tx = self.show_tx.clone();
		let quit_tx = self.quit_tx.clone();

		vec![
			MenuItem::Standard(StandardItem {
				label: "Fenster anzeigen".into(),
												 activate: Box::new(move |_tray: &mut WindowkeyerTray| {
													 let _ = show_tx.send(UiCommand::ShowWindow);
												 }),
												 ..Default::default()
			}),
			MenuItem::Separator,
			MenuItem::Standard(StandardItem {
				label: "Beenden".into(),
												 activate: Box::new(move |_tray: &mut WindowkeyerTray| {
													 let _ = quit_tx.send(UiCommand::Quit);
												 }),
												 ..Default::default()
			}),
		]
	}
}
