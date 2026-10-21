#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::X11 => "X11",
            Self::Wayland => "Wayland",
            Self::Unknown => "unbekannt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopKind {
    KDE,
    GNOME,
    Xfce,
    Sway,
    Hyprland,
    Other(String),
    Unknown,
}

impl DesktopKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::KDE => "KDE Plasma",
            Self::GNOME => "GNOME",
            Self::Xfce => "Xfce",
            Self::Sway => "Sway",
            Self::Hyprland => "Hyprland",
            Self::Other(name) => name.as_str(),
            Self::Unknown => "unbekannter Desktop",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub kind: SessionKind,
    pub desktop: DesktopKind,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
}

impl SessionInfo {
    pub fn summary(&self) -> String {
        format!(
            "{} / {} (WAYLAND_DISPLAY={}, DISPLAY={})",
            self.kind.as_str(),
            self.desktop.as_str(),
            self.wayland_display.as_deref().unwrap_or("-"),
            self.display.as_deref().unwrap_or("-"),
        )
    }

    /// Ehrliche Warnung je nach Session.
    pub fn warning(&self) -> Option<&'static str> {
        match self.kind {
			SessionKind::Wayland => Some(
				"Wayland: natives Backend nur unter KDE Plasma verfügbar (KWin D-Bus, GlobalShortcuts); andernfalls läuft Windowkeyer über XWayland und sieht nur X11-/XWayland-Fenster.",
			),
			_ => None,
		}
    }
}

fn env_str(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn detect_session() -> SessionInfo {
    let session_type = env_str("XDG_SESSION_TYPE")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();

    let wayland_display = env_str("WAYLAND_DISPLAY");
    let display = env_str("DISPLAY");

    let kind = if session_type == "wayland" || wayland_display.is_some() {
        SessionKind::Wayland
    } else if session_type == "x11" || display.is_some() {
        SessionKind::X11
    } else {
        SessionKind::Unknown
    };

    let desktop_raw = env_str("XDG_CURRENT_DESKTOP")
        .unwrap_or_else(|| env_str("DESKTOP_SESSION").unwrap_or_default())
        .to_lowercase();

    let desktop = if desktop_raw.contains("kde") || desktop_raw.contains("plasma") {
        DesktopKind::KDE
    } else if desktop_raw.contains("gnome") {
        DesktopKind::GNOME
    } else if desktop_raw.contains("xfce") {
        DesktopKind::Xfce
    } else if desktop_raw.contains("sway") {
        DesktopKind::Sway
    } else if desktop_raw.contains("hyprland") {
        DesktopKind::Hyprland
    } else if desktop_raw.is_empty() {
        DesktopKind::Unknown
    } else {
        DesktopKind::Other(env_str("XDG_CURRENT_DESKTOP").unwrap_or(desktop_raw))
    };

    SessionInfo {
        kind,
        desktop,
        display,
        wayland_display,
    }
}

pub fn select_backend_name(session: &SessionInfo) -> &'static str {
    match session.kind {
        SessionKind::Wayland => {
            match session.desktop {
                DesktopKind::KDE => "kde_wayland",
                DesktopKind::GNOME => "gnome_wayland",
                DesktopKind::Sway | DesktopKind::Hyprland => "wlroots",
                _ => "x11", // Fallback auf XWayland
            }
        }
        SessionKind::X11 => "x11",
        SessionKind::Unknown => "x11",
    }
}
