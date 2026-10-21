pub type WindowId = u32;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub display_text: String,
    pub keycode: u8,
    pub modifiers: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub app_id: String,
    pub minimized: bool,
    pub active: bool,
}

impl WindowInfo {
    pub fn state_label(&self) -> &'static str {
        if self.active {
            "aktiv"
        } else if self.minimized {
            "minimiert"
        } else {
            "offen"
        }
    }
}
