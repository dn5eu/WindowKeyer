//! Phase 3a – natives KDE Plasma Wayland Backend.
//!
//! Das offizielle `org_kde_plasma_window_management`-Wayland-Protokoll ist
//! seit Plasma 5 für Drittanbieter gesperrt; dieses Backend nutzt daher die
//! offiziellen KWin-D-Bus-Schnittstellen:
//!
//! * **Fensterliste / Aktivieren / Minimieren:** `org.kde.KWin` →
//!   `windowsList()` liefert Fensterpfade (`/Windows/<n>`), Metadaten via
//!   `org.freedesktop.DBus.Properties.GetAll` auf Interface
//!   `org.kde.KWin.Windows`.
//! * **Globale Hotkeys:** KWin GlobalShortcuts
//!   (`org.kde.GlobalShortcutManager2`, Plasma >= 6.1). Eine temporäre
//!   Action-Collection wird beim Start angelegt und im Drop wieder gelöscht.
//!   Bei Auslösung sendet KWin ein `activated(...)`-Signal an unsere Action,
//!   das wir als `BackendEvent::ShortcutTriggered(window_id)` weiterreichen —
//!   das Mapping Shortcut→Fenster liegt komplett in diesem Backend.
//! * **WindowClosed-Erkennung:** Polling-Thread (~2 s) vergleicht die
//!   Fensterpfad-Menge mit dem vorherigen Stand.
//!
//! Auf Nicht-KDE-Systemen oder wenn KWin nicht antwortet schlägt
//! `is_available()` fehl; main.rs fällt dann auf das XWayland-X11-Backend zurück.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::Sender;
use dbus::arg::{RefArg, Variant};
use dbus::blocking::{Connection, Proxy};

use crate::backend::{BackendError, BackendEvent, WindowBackend};
use crate::models::{Shortcut, WindowId, WindowInfo};

const KWIN_DEST: &str = "org.kde.KWin";
const TIMEOUT: Duration = Duration::from_millis(800);

fn variant_str(v: Option<&Variant<Box<dyn RefArg>>>) -> String {
    v.and_then(|v| v.0.as_str().map(str::to_string)).unwrap_or_default()
}
fn variant_bool(v: Option<&Variant<Box<dyn RefArg>>>) -> bool {
    v.and_then(|v| v.0.get(true)).unwrap_or(false)
}

/// Ein bei KWin registrierter globaler Shortcut (temporär, nur im RAM).
struct RegisteredShortcut {
    name: String,       // eindeutiger Name in unserer Collection
    shortcut: Shortcut, // zum Matching in ungrab_hotkey
    window_id: WindowId,
}

pub struct KdeWaylandBackend {
    conn: Connection,
    wm_name: String,
    collection: String, // ObjectPath der temporären Action-Collection
    shortcuts: Mutex<HashMap<String, RegisteredShortcut>>, // key: action_path
    running: Arc<AtomicBool>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
}

impl KdeWaylandBackend {
    /// Prüft, ob ein erreichbares KWin unter Wayland läuft
    /// (D-Bus-Antwort auf `/KWin` → Property `compositor`).
    pub fn is_available() -> bool {
        let Ok(conn) = Connection::new_session() else { return false };
        Self::compositor_kind(&conn).as_deref() == Some("wayland-general")
    }

    fn compositor_kind(conn: &Connection) -> Option<String> {
        let p = Proxy::new(KWIN_DEST, "/KWin", TIMEOUT, conn);
        let r: Result<(Variant<Box<dyn RefArg>>,), _> = p.method_call(
            "org.freedesktop.DBus.Properties", "Get", ("org.kde.KWin", "compositor"));
        match r {
            Ok((v,)) => v.0.as_str().map(str::to_string),
            Err(e) => { debugln!("[KWinDBus] compositor-Abfrage fehlgeschlagen: {e}"); None }
        }
    }

    pub fn new() -> Result<Self, BackendError> {
        let conn = Connection::new_session().map_err(|e| BackendError::Connection(e.to_string()))?;
        let kind = Self::compositor_kind(&conn)
            .ok_or_else(|| BackendError::Connection("Kein D-Bus-Zugriff auf org.kde.KWin".into()))?;
        if kind != "wayland-general" {
            return Err(BackendError::Unsupported(format!(
                "KWin meldet Compositor-Typ '{kind}' — natives Wayland-Backend nur unter Plasma Wayland"
            )));
        }
        let collection = Self::create_collection(&conn)?;
        debugln!("[KWinDBus] Hotkey-Collection angelegt: {collection}");
        Ok(Self {
            conn,
            wm_name: format!("KWin ({kind})"),
            collection,
            shortcuts: Mutex::new(HashMap::new()),
            running: Arc::new(AtomicBool::new(false)),
            event_thread: Mutex::new(None),
        })
    }

    fn create_collection(conn: &Connection) -> Result<String, BackendError> {
        let mgr = Proxy::new(KWIN_DEST, "/GlobalShortcutManager", TIMEOUT, conn);
        let r: Result<(dbus::arg::OwnedObjectPath,), _> = mgr.method_call(
            "org.kde.GlobalShortcutManager2", "newCollection", ("windowkeyer-temp", true));
        match r {
            Ok((path,)) => Ok(path.0.to_string()),
            Err(e) => Err(BackendError::Unsupported(format!(
                "KWin erlaubt keine temporäre Shortcut-Collection (Plasma < 6.1?): {e}"
            ))),
        }
    }

    /// Fensterpfade von KWin (`windowsList` → Array of ObjectPath).
    fn window_paths(&self) -> Vec<String> {
        let p = Proxy::new(KWIN_DEST, "/KWin", TIMEOUT, &self.conn);
        let reply: Result<(Vec<dbus::arg::OwnedObjectPath>,), _> =
            p.method_call("org.kde.KWin", "windowsList", ());
        match reply {
            Ok(paths) => paths.0.into_iter().map(|p| p.0.to_string()).collect(),
            Err(e) => { debugln!("[KWinDBus] windowsList fehlgeschlagen: {e}"); Vec::new() }
        }
    }

    /// Deterministische Pseudo-ID aus dem Fensterpfad (Wayland hat keine
    /// X11-Window-IDs). Bit 31 gesetzt → kollidiert nie mit echten X-IDs.
    fn wid_from_path(path: &str) -> WindowId {
        let mut hash: u32 = 2166136261; // FNV-1a 32bit
        for b in path.bytes() { hash ^= b as u32; hash = hash.wrapping_mul(16777619); }
        (hash & 0x7FFF_FFFF) | 0x8000_0000
    }

    fn find_path(&self, id: WindowId) -> Option<String> {
        self.window_paths().into_iter().find(|p| Self::wid_from_path(p) == id)
    }

    fn client_props(&self, path: &str) -> Option<HashMap<String, Variant<Box<dyn RefArg>>>> {
        let p = Proxy::new(KWIN_DEST, path, TIMEOUT, &self.conn);
        let r: Result<(HashMap<String, Variant<Box<dyn RefArg>>>,), _> = p.method_call(
            "org.freedesktop.DBus.Properties", "GetAll", ("org.kde.KWin.Windows",));
        match r { Ok(m) => Some(m.0), Err(_) => None }
    }

    fn info_for(&self, path: &str) -> Option<WindowInfo> {
        let props = self.client_props(path)?;
        let title = {
            let t = variant_str(props.get("caption"));
            if t.is_empty() { format!("Wayland-Fenster {}", path.rsplit('/').next().unwrap_or("?")) } else { t }
        };
        let app_id = variant_str(props.get("appName"));
        let app_id = if app_id.is_empty() { variant_str(props.get("resourceClass")) } else { app_id };
        Some(WindowInfo {
            id: Self::wid_from_path(path),
            title,
            app_id,
            minimized: variant_bool(props.get("minimized")),
            active: variant_bool(props.get("active")),
        })
    }

    /// Übersetzt GTK-Keycode + Modifier-Maske in eine KWin-KeySequence wie
    /// "Meta+Ctrl+f".
    fn key_sequence(s: &Shortcut) -> String {
        let mut parts: Vec<String> = Vec::new();
        if s.modifiers & 0x0001 != 0 { parts.push("Shift".into()); }
        if s.modifiers & 0x0004 != 0 { parts.push("Ctrl".into()); }
        if s.modifiers & 0x0008 != 0 { parts.push("Alt".into()); }
        if s.modifiers & 0x0040 != 0 { parts.push("Meta".into()); }

        match keycode_to_keysym(s.keycode).and_then(keysym_name) {
            Some(k) => { parts.push(k); parts.join("+") }
            None => String::new(),
        }
    }

    fn register_shortcut(&self, shortcut: &Shortcut, window_id: WindowId) -> Result<(), BackendError> {
        let seq = Self::key_sequence(shortcut);
        if seq.is_empty() {
            return Err(BackendError::Unsupported(format!(
                "Taste (Keycode {}) konnte nicht in eine KWin-Sequenz übersetzt werden", shortcut.keycode)));
        }
        // Eindeutiger, stabiler Name pro (Fenster, Keycode, Modifikatoren).
        let name = format!("wk_{window_id:x}_{:x}_{:x}", shortcut.keycode, shortcut.modifiers);
        debugln!("[KWinDBus] registriere Hotkey '{seq}' -> Fenster {window_id:#x}");

        let coll = Proxy::new(KWIN_DEST, &self.collection, TIMEOUT, &self.conn);
        let action_path = match coll.method_call::<(dbus::arg::OwnedObjectPath,)>(
            "org.kde.GlobalShortcutManager2", "addAction", (&name, "", "", ""))
        {
            Ok((path,)) => path.0.to_string(),
            Err(e) => {
                debugln!("[KWinDBus] addAction-Rückgabe unlesbar ({e}), nutze Pfad-Konvention");
                format!("{}/{}", self.collection.trim_end_matches('/'), name)
            }
        };

        let act = Proxy::new(KWIN_DEST, &action_path, TIMEOUT, &self.conn);
        let r: Result<(), _> = act.method_call("org.kde.Action", "setGlobalShortcut", (&seq,));
        if r.is_err() {
            let r2: Result<(), _> = act.method_call("org.kde.Action", "setCustomShortcut", (&seq,));
            r2.map_err(|e| BackendError::Unsupported(format!("GlobalShortcut '{seq}' setzen: {e}")))?;
        }

        let mut guard = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
        // Idempotenz: gleiche Zuweisung erneut → alte Registrierung zuerst entfernen.
        let old = guard.iter().find(|(_, r)| r.name == name).map(|(p, _)| p.clone());
        if let Some(old) = old {
            drop(guard);
            self.unregister_action(&old);
            guard = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
        }
        guard.insert(action_path, RegisteredShortcut { name, shortcut: shortcut.clone(), window_id });
        Ok(())
    }

    fn unregister_action(&self, action_path: &str) {
        let entry = {
            let mut g = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
            g.remove(action_path)
        };
        let Some(entry) = entry else { return };
        let coll = Proxy::new(KWIN_DEST, &self.collection, TIMEOUT, &self.conn);
        let _ = coll.method_call::<()>("org.kde.GlobalShortcutManager2", "removeAction", (&entry.name,));
    }

    /// Event-/Polling-Loop:
    /// 1. D-Bus-Signale `activated()`/`activate()` unserer Actions → Trigger.
    /// 2. Alle ~2 s Fensterpfade vergleichen → WindowClosed / WindowsChanged.
    fn spawn_event_loop(self: &Arc<Self>, tx: Sender<BackendEvent>) -> Result<JoinHandle<()>, BackendError> {
        let backend = Arc::clone(self);
        let running = Arc::clone(&self.running);
        let handle = std::thread::spawn(move || {
            let conn = match Connection::new_session() {
                Ok(c) => c,
                Err(e) => { let _ = tx.send(BackendEvent::BackendLost(format!("D-Bus Eventloop: {e}"))); return; }
            };
            // path_namespace deckt auch später hinzukommende Actions ab.
            let ns = backend.collection.trim_end_matches('/');
            let _ = conn.add_match(&format!("interface='org.kde.Action',member='activated',path_namespace='{ns}'"));
            let _ = conn.add_match(&format!("interface='org.kde.Action',member='activate',path_namespace='{ns}'"));

            let mut last_paths: Option<HashSet<String>> = None;
            while running.load(Ordering::Relaxed) {
                // Signale einsammeln (max ~1.9 s pro Runde blockierend).
                let deadline = std::time::Instant::now() + Duration::from_millis(1900);
                while std::time::Instant::now() < deadline {
                    match conn.receive_with_reply(Duration::from_millis(100)) {
                        Ok(msg) => {
                            let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
                            if member != "activated" && member != "activate" { continue; }
                            let path = msg.path().to_string_lossy().to_string();
                            let guard = backend.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(entry) = guard.get(&path) {
                                debugln!("[KWinDBus] Hotkey ausgelöst: {} -> {:#x}", entry.name, entry.window_id);
                                let _ = tx.send(BackendEvent::ShortcutTriggered(entry.window_id));
                            }
                        }
                        Err(_) => break, // Timeout o. ä. → weiter mit Polling
                    }
                }

                // Fensterliste pollen.
                let paths: HashSet<String> = backend.window_paths().into_iter().collect();
                if let Some(prev) = &last_paths {
                    for gone in prev.difference(&paths) {
                        let id = KdeWaylandBackend::wid_from_path(gone);
                        debugln!("[KWinDBus] Fenster geschlossen: {gone} (ID {id:#x})");
                        let _ = tx.send(BackendEvent::WindowClosed(id));
                    }
                    if paths != *prev { let _ = tx.send(BackendEvent::WindowsChanged); }
                }
                last_paths = Some(paths);
            }
        });
        Ok(handle)
    }
}

/// Keycode → Keysym: versucht eine X-Verbindung (XWayland läuft unter
/// Plasma-Wayland fast immer), sonst Fallback auf eine statische
/// Standard-XKB-QWERTY-US-Keymap.
fn keycode_to_keysym(keycode: u8) -> Option<u32> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    if std::env::var_os("DISPLAY").is_some() {
        if let Ok((conn, _)) = x11rb::connect(None) {
            if let Ok(kbd) = conn.key_mapping().and_then(|c| c.reply()) {
                let idx = keycode.checked_sub(kbd.first_keycode)? as usize;
                if let Some(row) = kbd.keysyms.get(idx) {
                    if let Some(sym) = row.get(0).copied() {
                        if sym != 0 { return Some(sym); }
                    }
                }
            }
        }
    }
    FALLBACK_KEYMAP.get(keycode as usize).copied().filter(|s| *s != 0)
}

/// Standard-XKB-QWERTY-US-Keymap ab KeyCode 8 (Ausschnitt bis KeyCode 134).
const FALLBACK_KEYMAP: &[u32] = &[
    /* 0..8 */  0, 0, 0, 0, 0, 0, 0, 0,
    /* 8 */     0xff08, 0xff09, 0xff0d, 0xff13,             // BackSpace Tab Return Pause
    /* 12 */    0, 0xff50, 0xff80, 0xff81, 0xff82, 0xff83, 0xff84, 0xff85, 0xff86, 0xff87,
    /* 22 */    0xff08, 0xff09, 0x71, 0x77, 0x65, 0x72, 0x74, 0x79, 0x75, 0x69,
    /* 32 */    0x6f, 0x70, 0xff0b, 0xff0d, 0xffe3, 0x61, 0x73, 0x64, 0x66, 0x67,
    /* 42 */    0x68, 0x6a, 0x6b, 0x6c, 0x3b, 0x27, 0x60, 0xffe1, 0x5c, 0x7a,
    /* 52 */    0x78, 0x63, 0x76, 0x62, 0x6e, 0x6d, 0x2c, 0x2e, 0x2f, 0xffe2,
    /* 62 */    0xffaa, 0xffea, 0x20, 0xffe5, 0xffbe, 0xffbf, 0xffc0, 0xffc1, 0xffc2, 0xffc3,
    /* 72 */    0xffc4, 0xffc5, 0xffc6, 0xffc7, 0xffc8, 0xffc9, 0xffca, 0xffcb, 0xffcc, 0xffcd, 0xffce, 0xffcf,
    /* 84 */    0xffd0, 0xffd1, 0xffd2, 0xffd3, 0xffd4, 0xffd5,
];

/// Keysym → KWin-Name. Druckbare ASCII-Symbole entsprechen ihrem Codepoint.
fn keysym_name(sym: u32) -> Option<String> {
    if (0x21..=0x7e).contains(&sym) {
        return Some((sym as u8 as char).to_ascii_lowercase());
    }
    const SPECIAL: &[(u32, &str)] = &[
        (0xff31, "Home"), (0xff32, "Prior"), (0xff33, "Next"), (0xff34, "End"),
        (0xff36, "Insert"), (0xff37, "Delete"), (0xff08, "Backspace"),
        (0xff09, "Tab"), (0xff0d, "Return"), (0xff13, "Pause"), (0xff1b, "Escape"),
        (0xffbe, "F1"), (0xffbf, "F2"), (0xffc0, "F3"), (0xffc1, "F4"), (0xffc2, "F5"),
        (0xffc3, "F6"), (0xffc4, "F7"), (0xffc5, "F8"), (0xffc6, "F9"), (0xffc7, "F10"),
        (0xffc8, "F11"), (0xffc9, "F12"), (0xffca, "F13"), (0xffcb, "F14"), (0xffcc, "F15"),
        (0xffcd, "F16"), (0xffce, "F17"), (0xffcf, "F18"), (0xffd0, "F19"), (0xffd1, "F20"),
        (0xffd2, "F21"), (0xffd3, "F22"), (0xffd4, "F23"), (0xffd5, "F24"),
        (0xff51, "Left"), (0xff52, "Up"), (0xff53, "Right"), (0xff54, "Down"),
        (0xffe1, "Shift"), (0xffe2, "Shift"), (0xffe3, "Control"), (0xffe4, "Control"),
        (0xffe7, "Meta"), (0xffe8, "Meta"), (0xffe9, "Alt"), (0xffea, "Alt"),
        (0xffe5, "Caps_Lock"), (0x20, "space"),
    ];
    SPECIAL.iter().find(|(s, _)| *s == sym).map(|(_, n)| (*n).to_string())
}

impl WindowBackend for KdeWaylandBackend {
    fn name(&self) -> &str { "KDE Plasma Wayland (KWin D-Bus)" }
    fn window_manager_name(&self) -> &str { &self.wm_name }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, BackendError> {
        Ok(self.window_paths().iter().filter_map(|p| self.info_for(p)).collect())
    }

    fn activate_window(&self, id: WindowId) -> Result<(), BackendError> {
        let path = self.find_path(id).ok_or(BackendError::NoSuchWindow(id))?;
        let p = Proxy::new(KWIN_DEST, &path, TIMEOUT, &self.conn);
        let r: Result<(), _> = p.method_call("org.kde.KWin", "activateWindow", ());
        if r.is_ok() { return Ok(()); }
        // Fallback: Property active=true setzen.
        let r2: Result<(), _> = p.method_call("org.freedesktop.DBus.Properties", "Set",
            ("org.kde.KWin.Windows", "active", Variant(Box::new(true) as Box<dyn RefArg>)));
        r2.map_err(|e| BackendError::X11(format!("Aktivieren Fenster {id:#x}: {e}")))
    }

    fn minimize_window(&self, id: WindowId) -> Result<(), BackendError> {
        let path = self.find_path(id).ok_or(BackendError::NoSuchWindow(id))?;
        let p = Proxy::new(KWIN_DEST, &path, TIMEOUT, &self.conn);
        let r: Result<(), _> = p.method_call("org.kde.KWin", "requestMinimize", ());
        if r.is_ok() { return Ok(()); }
        let r2: Result<(), _> = p.method_call("org.freedesktop.DBus.Properties", "Set",
            ("org.kde.KWin.Windows", "minimized", Variant(Box::new(true) as Box<dyn RefArg>)));
        r2.map_err(|e| BackendError::X11(format!("Minimieren Fenster {id:#x}: {e}")))
    }

    fn create_shortcut(&self, display_text: String, keycode: u8, modifiers: u16) -> Result<Shortcut, BackendError> {
        if keycode == 0 { return Err(BackendError::Unsupported("Ungültiger Keycode".into())); }
        const LOCK_MASK: u16 = 0x8002;                 // CapsLock | NumLock | ScrollLock
        const X11_MASK: u16 = 0x80 | 0x4000 | 0x8000;  // Dial | AltGr | extra Super-Bit
        Ok(Shortcut { display_text, keycode, modifiers: modifiers & !(LOCK_MASK | X11_MASK) })
    }

    fn grab_hotkey(&self, _shortcut: &Shortcut) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "KDE Wayland: Hotkeys werden über grab_hotkey_for_window registriert".into()))
    }

    fn grab_hotkey_for_window(&self, shortcut: &Shortcut, window_id: WindowId) -> Result<(), BackendError> {
        self.register_shortcut(shortcut, window_id)
    }

    fn ungrab_hotkey(&self, shortcut: &Shortcut) -> Result<(), BackendError> {
        let targets: Vec<String> = {
            let g = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
            g.iter().filter(|(_, r)| r.shortcut == *shortcut).map(|(p, _)| p.clone()).collect()
        };
        for t in targets { self.unregister_action(&t); }
        Ok(())
    }

    fn start_events(&self, tx: Sender<BackendEvent>) -> Result<(), BackendError> {
        let mut guard = self.event_thread.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() { return Ok(()); }
        self.running.store(true, Ordering::Relaxed);
        let handle = self.spawn_event_loop(tx)?;
        *guard = Some(handle);
        Ok(())
    }

    fn stop_events(&self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.event_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }

    fn uses_x11_keycodes(&self) -> bool { false }
}

impl Drop for KdeWaylandBackend {
    fn drop(&mut self) {
        self.stop_events();
        // Temporäre Collection entfernen → alle Hotkeys verschwinden aus KWin.
        let mgr = Proxy::new(KWIN_DEST, "/GlobalShortcutManager", TIMEOUT, &self.conn);
        let _ = mgr.method_call::<()>("org.kde.GlobalShortcutManager2", "deleteCollection", (&self.collection,));
        debugln!("[KWinDBus] Hotkey-Collection entfernt: {}", self.collection);
    }
}
