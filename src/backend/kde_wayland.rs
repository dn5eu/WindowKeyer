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
//!   (`/GlobalShortcutManager`, Interface `org.kde.GlobalShortcutManager`).
//!   Eine temporäre Action-Collection wird beim Start angelegt und im Drop
//!   wieder gelöscht. Bei Auslösung sendet KWin ein `activated(...)`-Signal
//!   unserer Action; der Event-Loop liest den Shortcut-/Action-Namen aus dem
//!   Signal und meldet `BackendEvent::ShortcutTriggered(name)`. Die UI löst
//!   darüber über `resolve_shortcut_trigger` die Ziel-Fenster-ID auf.
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
use dbus::blocking::{Connection, Proxy, SyncConnection};
use dbus::message::{MatchRule, Message};

use crate::backend::{BackendError, BackendEvent, WindowBackend};
use crate::models::{Shortcut, WindowId, WindowInfo};

const KWIN_DEST: &str = "org.kde.KWin";
const GSM_PATH: &str = "/GlobalShortcutManager";
const GSM_IFACE: &str = "org.kde.GlobalShortcutManager";
const TIMEOUT: Duration = Duration::from_millis(800);

fn variant_str(v: Option<&Variant<Box<dyn RefArg>>>) -> String {
    v.and_then(|v| v.0.as_str().map(str::to_string)).unwrap_or_default()
}
fn variant_bool(v: Option<&Variant<Box<dyn RefArg>>>) -> bool {
    // dbus 0.9: `Get::get` erwartet einen Iter, nicht &dyn RefArg — für
    // RefArg-Werte sind die as_i64/as_u64-Reader der richtige Weg (dokumentiert
    // explizit für ArgType::Boolean unterstützt).
    v.and_then(|v| {
        let inner: &(dyn RefArg + 'static) = &*v.0;
        if inner.arg_type() == dbus::arg::ArgType::Boolean {
            inner.as_i64().or_else(|| inner.as_u64().map(|x| x as i64)).map(|x| x != 0)
        } else {
            None
        }
    })
    .unwrap_or(false)
}

/// Ein bei KWin registrierter globaler Shortcut (temporär, nur im RAM).
/// Der eindeutige Name ist zugleich der Schlüssel der HashMap, deshalb wird
/// er hier nicht zusätzlich als Feld gespeichert.
struct RegisteredShortcut {
    shortcut: Shortcut, // zum Matching in ungrab_hotkey
    window_id: WindowId,
}

pub struct KdeWaylandBackend {
    conn: Arc<SyncConnection>,
    wm_name: String,
    collection: String, // ObjectPath der temporären Action-Collection
    shortcuts: Mutex<HashMap<String, RegisteredShortcut>>, // key: Shortcut-Name
    running: Arc<AtomicBool>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
}

impl KdeWaylandBackend {
    /// Prüft, ob ein erreichbares KWin unter Wayland läuft.
    pub fn is_available() -> bool {
        let Ok(conn) = Connection::new_session() else { return false };
        Self::compositor_kind(&conn).as_deref() == Some("wayland")
    }

    /// Liefert "wayland" / "x11" / None.
    ///
    /// KWin exportiert die Property `compositor` je nach Version/Setup unter
    /// unterschiedlichen Interfaces am Objekt `/KWin`:
    /// * Plasma 5/6 Standard: Interface `org.kde.KWin`, Werte `"wayland"`/`"x11"`
    /// * manche Setups/Ältere: nur über ein anderes Interface sichtbar
    ///   (dann liefert der direkte `Get` einen Fehler wie
    ///   „Property org.kde.KWin.compositor was not found in object /KWin").
    ///
    /// Deshalb: erst direkter Get, danach Introspektion von `/KWin` und
    /// Get-Versuche auf allen gefundenen Interfaces; als letzten Anker die
    /// Environment-Variable XDG_SESSION_TYPE.
    fn compositor_kind<C: dbus::blocking::BlockingSender>(conn: &C) -> Option<String> {
        let p = Proxy::new(KWIN_DEST, "/KWin", TIMEOUT, conn);

        // 1) Direkter Get auf dem Standard-Interface
        if let Some(k) = Self::read_compositor_prop(&p, "org.kde.KWin") { return Some(k); }

        // 2) Introspection: alle Interfaces von /KWin abfragen und dort je den
        //    Get auf "compositor" versuchen.
        let introspected: Vec<String> = {
            let r: Result<(String,), _> = p.method_call(
                "org.freedesktop.DBus.Introspectable", "Introspect", ());
            match r {
                Ok((xml,)) => parse_introspection_interfaces(&xml),
                Err(e) => { debugln!("[KWinDBus] Introspection fehlgeschlagen: {e}"); Vec::new() }
            }
        };
        for iface in introspected {
            if iface == "org.kde.KWin" { continue; } // schon versucht
            if let Some(k) = Self::read_compositor_prop(&p, &iface) { return Some(k); }
        }

        // 3) Letzter Anker: Wenn XDG_SESSION_TYPE=wayland und wir hierher
        //    kommen, ist KWin zwar erreichbar, aber die Property nirgends
        //    lesbar → trotzdem als Wayland einstufen (sonst würde der Nutzer
        //    nie das native Backend bekommen).
        if std::env::var("XDG_SESSION_TYPE").map(|v| v.eq_ignore_ascii_case("wayland")).unwrap_or(false) {
            debugln!("[KWinDBus] compositor-Property nirgends gefunden, nutze XDG_SESSION_TYPE=wayland");
            return Some("wayland".into());
        }

        debugln!("[KWinDBus] compositor-Property nirgends gefunden");
        None
    }

    fn read_compositor_prop<T: dbus::blocking::BlockingSender, C: std::ops::Deref<Target = T>>(
        p: &Proxy<'_, C>, iface: &str) -> Option<String> {
        let r: Result<(Variant<Box<dyn RefArg>>,), _> = p.method_call(
            "org.freedesktop.DBus.Properties", "Get", (iface, "compositor"));
        match r {
            Ok((v,)) => v.0.as_str().map(|s| {
                let s = s.to_lowercase();
                // Normalisieren: "wayland"/"wayland-general" → wayland,
                // "x11"/"x11-general"/"X11" → x11.
                if s.contains("wayland") { "wayland".to_string() } else { s }
            }),
            Err(_) => None,
        }
    }

    pub fn new() -> Result<Self, BackendError> {
        let probe = Connection::new_session()
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        let kind = Self::compositor_kind(&probe)
            .ok_or_else(|| BackendError::Connection("Kein D-Bus-Zugriff auf org.kde.KWin".into()))?;
        if kind != "wayland" {
            return Err(BackendError::Unsupported(format!(
                "KWin meldet Compositor-Typ '{kind}' — natives Wayland-Backend nur unter Plasma Wayland"
            )));
        }
        drop(probe);
        // SyncConnection statt Connection: dbus::blocking::Connection enthält
        // ein RefCell und ist damit NICHT Sync — unser Backend muss aber
        // Send + Sync sein (Arc<dyn WindowBackend> lebt in mehreren Threads).
        let conn = Arc::new(SyncConnection::new_session()
            .map_err(|e| BackendError::Connection(e.to_string()))?);
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

    fn create_collection(conn: &SyncConnection) -> Result<String, BackendError> {
        let mgr = Proxy::new(KWIN_DEST, GSM_PATH, TIMEOUT, conn);
        // newCollection(s, b) → o (Pfad der neuen Collection). Weicht eine
        // KWin-Version vom Rückgabetyp ab, hilft die Pfad-Konvention weiter.
        let r: Result<(dbus::strings::Path<'static>,), _> = mgr.method_call(
            GSM_IFACE, "newCollection", ("windowkeyer-temp", true));
        match r {
            Ok((path,)) => Ok(path.to_string()),
            Err(e) => {
                debugln!("[KWinDBus] newCollection-Rückgabe unlesbar ({e}), nutze Pfad-Konvention");
                Ok(format!("{GSM_PATH}/windowkeyer-temp"))
            }
        }
    }

    /// Fensterpfade von KWin (`windowsList` → Array of ObjectPath).
    fn window_paths<C: dbus::blocking::BlockingSender>(&self, conn: &C) -> Vec<String> {
        let p = Proxy::new(KWIN_DEST, "/KWin", TIMEOUT, conn);
        let reply: Result<(Vec<dbus::strings::Path<'static>>,), _> =
            p.method_call("org.kde.KWin", "windowsList", ());
        match reply {
            Ok(paths) => paths.0.into_iter().map(|p| p.to_string()).collect(),
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
        self.window_paths(&*self.conn).into_iter().find(|p| Self::wid_from_path(p) == id)
    }

    fn client_props(&self, path: &str) -> Option<HashMap<String, Variant<Box<dyn RefArg>>>> {
        // Liefert die Client-Metadaten eines Fensters. KWin exportiert die
        // Fenster-Properties je nach Version unter unterschiedlichen Interfaces;
        // wir probieren daher systematisch:
        // 1. Bekannte Interface-Namen direkt per GetAll (schnellster Pfad).
        // 2. Fallback: Introspektion des Fensterpfads + GetAll auf jedem
        //    gefundenen org.kde.KWin*-Interface, bis "caption"/"active" vorkommt.
        let p = Proxy::new(KWIN_DEST, path, TIMEOUT, &*self.conn);
        for iface in ["org.kde.KWin.Windows", "org.kde.KWin", "org.kde.KWin.Tablex11Client", "org.kde.KWin.TabBoxClient"] {
            let r: Result<(HashMap<String, Variant<Box<dyn RefArg>>>,), _> = p.method_call(
                "org.freedesktop.DBus.Properties", "GetAll", (iface,));
            if let Ok(m) = r {
                let map = m.0;
                if map.contains_key("caption") || map.contains_key("active") || map.contains_key("minimized") {
                    return Some(map);
                }
            }
        }
        // 3. Letzter Versuch: Alle Interfaces von /Windows/<n> durchsuchen.
        let xml: String = {
            use dbus::blocking::stdintf::org_freedesktop_dbus::Introspectable as _;
            match p.introspect() { Ok(x) => x, Err(_) => return None }
        };
        for iface in parse_introspection_interfaces(&xml) {
            if !iface.starts_with("org.kde.KWin") { continue; }
            let r: Result<(HashMap<String, Variant<Box<dyn RefArg>>>,), _> = p.method_call(
                "org.freedesktop.DBus.Properties", "GetAll", (&iface,));
            if let Ok(m) = r {
                let map = m.0;
                if map.contains_key("caption") || map.contains_key("active") || map.contains_key("minimized") {
                    return Some(map);
                }
            }
        }
        None
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
    ///
    /// WICHTIG: Unter **Wayland** liefert GTK4 in `EventControllerKey` den
    /// **evdev-Hardware-Keycode** (z. B. 16 für 'q'), NICHT den X11-/XKB-Layout-
    /// keycode (unter X11 wäre das z. B. 24). Der evdev-Wert ist layout-unabhängig
    /// und muss daher erst über die aktive Tastatur in ein Keysym übersetzt
    /// werden. Das done wir mit der aktiven GdkKeymap (die unter Wayland das
    /// xkbcommon-Layout nutzt): `keycode_to_keysym` fragt dort zuerst nach,
    /// danach wird optional eine statische evdev-US-Fallback-Tabelle befragt.
    ///
    /// Modifier-Maske = X11-Bits, so wie die UI sie aus dem
    /// GTK-State des Key-Events übernimmt (`state.bits()`):
    /// Shift 0x1, CapsLock 0x2, Control 0x4, Mod1/Alt 0x8, NumLock 0x10,
    /// Mod3 0x20, Super/Mod4 0x40, Mod5 0x80. Locks werden vorab von
    /// `create_shortcut` herausgerechnet; falls doch eines durchrutscht,
    /// ignorieren wir es hier ebenfalls (KWin würde die Sequenz sonst als
    /// unauflösbar ablehnen).
    fn key_sequence(s: &Shortcut) -> String {
        const LOCK_MASK: u16 = 0x2 | 0x10 | 0x80; // CapsLock | NumLock | ScrollLock(Mod5)
        let m = s.modifiers & !LOCK_MASK;
        let mut parts: Vec<String> = Vec::new();
        if m & 0x0001 != 0 { parts.push("Shift".into()); }
        if m & 0x0004 != 0 { parts.push("Ctrl".into()); }
        if m & 0x0008 != 0 { parts.push("Alt".into()); }
        if m & 0x0040 != 0 { parts.push("Meta".into()); }

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

        let coll = Proxy::new(KWIN_DEST, &self.collection, TIMEOUT, &*self.conn);
        // addAction(name s, comment s, category s, icon s) → o (Action-Pfad).
        let r: Result<(dbus::strings::Path<'static>,), _> = coll.method_call(
            GSM_IFACE, "addAction", (&name, "Temporärer Windowkeyer-Hotkey", "", ""));
        let action_path = match r {
            Ok((path,)) => path.to_string(),
            Err(e) => {
                debugln!("[KWinDBus] addAction-Rückgabe unlesbar ({e}), nutze Pfad-Konvention");
                format!("{}/{}", self.collection.trim_end_matches('/'), name)
            }
        };

        let act = Proxy::new(KWIN_DEST, &action_path, TIMEOUT, &*self.conn);
        // setCustomShortcut(s): der Standardweg bei KWin GlobalShortcuts;
        // setGlobalShortcut existiert nur in älteren/anderen Versionen.
        let r2: Result<(), _> = act.method_call("org.kde.Action", "setCustomShortcut", (&seq,));
        if r2.is_err() {
            let r3: Result<(), _> = act.method_call("org.kde.Action", "setGlobalShortcut", (&seq,));
            r3.map_err(|e| BackendError::Unsupported(format!("GlobalShortcut '{seq}' setzen: {e}")))?;
        }

        let mut guard = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
        // Idempotenz: gleicher Name bereits registriert → alte Zuweisung ersetzen.
        if guard.contains_key(&name) {
            drop(guard);
            self.unregister_action(&name);
            guard = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
        }
        guard.insert(name, RegisteredShortcut { shortcut: shortcut.clone(), window_id });
        Ok(())
    }

    fn unregister_action(&self, name: &str) {
        let existed = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner()).remove(name).is_some();
        if !existed { return; }
        let coll = Proxy::new(KWIN_DEST, &self.collection, TIMEOUT, &*self.conn);
        let _: Result<(), _> = coll.method_call(GSM_IFACE, "removeAction", (name,));
    }

    /// Liest den Shortcut-/Action-Namen aus einem activated()-Signal.
    /// Je nach KWin-Version sind Signaturvarianten im Umlauf: String-Argument
    /// (Shortcut-Name), ObjectPath (Action) oder kein Argument (dann hilft
    /// der Absender-Name des Signals weiter, der die Action selbst ist).
    fn trigger_name_from_signal(msg: &Message) -> Option<String> {
        use dbus::arg::ReadAll;
        if let Ok((s,)) = <(String,) as ReadAll>::read(&mut msg.iter_init()) {
            if !s.is_empty() { return Some(s); }
        }
        if let Ok((p,)) = <(dbus::strings::Path<'static>,) as ReadAll>::read(&mut msg.iter_init()) {
            let s = p.to_string();
            // Action-Pfad → Name ist das letzte Pfadsegment.
            if let Some(last) = s.rsplit('/').next() {
                if !last.is_empty() { return Some(last.to_string()); }
            }
        }
        msg.sender().map(|b| b.to_string()).filter(|s| !s.is_empty())
    }

    /// Event-Loop in einem eigenen Thread mit eigener D-Bus-Verbindung:
    /// 1. Match auf `activated()`/`activate()`-Signale unserer Actions.
    /// 2. Alle ~2 s Fensterpfade vergleichen → WindowClosed / WindowsChanged.
    fn spawn_event_loop(
        collection: String,
        running: Arc<AtomicBool>,
        tx: Sender<BackendEvent>,
    ) -> Result<JoinHandle<()>, BackendError> {
        let conn = Arc::new(SyncConnection::new_session()
            .map_err(|e| BackendError::Connection(format!("D-Bus Eventloop: {e}")))?);

        // Match-Rule: interface org.kde.Action, member activated/activate,
        // path_namespace deckt auch später hinzukommende Actions ab.
        let ns = collection.trim_end_matches('/').to_string();
        for member in ["activated", "activate"] {
            // MatchRule hat ein privates Feld — nicht mit ..Default::default()
            // konstruieren, sondern Default nehmen und Felder einzeln setzen.
            let mut rule = MatchRule::<'static>::default();
            rule.msg_type = Some(dbus::MessageType::Signal);
            rule.interface = Some(dbus::strings::Interface::from("org.kde.Action"));
            rule.member = Some(dbus::strings::Member::from(member.to_string()));
            rule.path = Some(dbus::strings::Path::from(ns.clone()));
            rule.path_is_namespace = true;
            let tx2 = tx.clone();
            let _ = conn.add_match::<(), _>(rule, move |_args: (), _c: &SyncConnection, msg: &Message| {
                if let Some(name) = KdeWaylandBackend::trigger_name_from_signal(msg) {
                    debugln!("[KWinDBus] Hotkey-Signal '{member}' für '{name}' empfangen");
                    let _ = tx2.send(BackendEvent::ShortcutTriggered(name));
                }
                true
            });
        }

        let handle = std::thread::spawn(move || {
            let mut last_paths: Option<HashSet<String>> = None;
            while running.load(Ordering::Relaxed) {
                // Eingehende Signale verarbeiten (blockiert max. 2 s pro Runde
                // und ruft dabei die add_match-Callbacks auf).
                let _ = conn.process(Duration::from_millis(2000));

                // Fensterliste pollen.
                let paths: HashSet<String> = poll_window_paths(&*conn).into_iter().collect();
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

/// windowsList auf einer beliebigen BlockingSender-Verbindung (auch die
/// Event-Loop-Verbindung braucht das, ohne Zugriff auf die Backend-Instanz).
fn poll_window_paths<C: dbus::blocking::BlockingSender>(conn: &C) -> Vec<String> {
    let p = Proxy::new(KWIN_DEST, "/KWin", TIMEOUT, conn);
    let reply: Result<(Vec<dbus::strings::Path<'static>>,), _> = p.method_call("org.kde.KWin", "windowsList", ());
    match reply {
        Ok(paths) => paths.0.into_iter().map(|p| p.to_string()).collect(),
        Err(e) => { debugln!("[KWinDBus] windowsList (Poll) fehlgeschlagen: {e}"); Vec::new() }
    }
}

/// Hilfsfunktion: Extrahiere alle Interface-Namen aus einem Introspektions-XML.
/// Erwartetes Format: `<interface name="...">`.
fn parse_introspection_interfaces(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Suche nach interface name="
        if xml[i..].starts_with("<interface name=\"") {
            let start = i + "<interface name=\"".len();
            if let Some(end) = xml[start..].find('"') {
                let iface = &xml[start..start+end];
                if !iface.is_empty() && !out.iter().any(|s| s == iface) {
                    out.push(iface.to_string());
                }
                i = start + end;
                continue;
            }
        }
        i += 1;
    }
    out
}

impl WindowBackend for KdeWaylandBackend {
    fn name(&self) -> &str { "KDE Plasma Wayland (KWin D-Bus)" }
    fn window_manager_name(&self) -> &str { &self.wm_name }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, BackendError> {
        Ok(self.window_paths(&*self.conn).iter().filter_map(|p| self.info_for(p)).collect())
    }

    fn activate_window(&self, id: WindowId) -> Result<(), BackendError> {
        let path = self.find_path(id).ok_or(BackendError::NoSuchWindow(id))?;
        let p = Proxy::new(KWIN_DEST, &path, TIMEOUT, &*self.conn);
        let r: Result<(), _> = p.method_call("org.kde.KWin", "activateWindow", ());
        if r.is_ok() { return Ok(()); }
        // Fallback: Property active=true setzen.
        let r2: Result<(), _> = p.method_call("org.freedesktop.DBus.Properties", "Set",
            ("org.kde.KWin.Windows", "active", Variant(Box::new(true) as Box<dyn RefArg>)));
        r2.map_err(|e| BackendError::X11(format!("Aktivieren Fenster {id:#x}: {e}")))
    }

    fn minimize_window(&self, id: WindowId) -> Result<(), BackendError> {
        let path = self.find_path(id).ok_or(BackendError::NoSuchWindow(id))?;
        let p = Proxy::new(KWIN_DEST, &path, TIMEOUT, &*self.conn);
        let r: Result<(), _> = p.method_call("org.kde.KWin", "requestMinimize", ());
        if r.is_ok() { return Ok(()); }
        let r2: Result<(), _> = p.method_call("org.freedesktop.DBus.Properties", "Set",
            ("org.kde.KWin.Windows", "minimized", Variant(Box::new(true) as Box<dyn RefArg>)));
        r2.map_err(|e| BackendError::X11(format!("Minimieren Fenster {id:#x}: {e}")))
    }

    fn create_shortcut(&self, display_text: String, keycode: u8, modifiers: u16) -> Result<Shortcut, BackendError> {
        if keycode == 0 { return Err(BackendError::Unsupported("Ungültiger Keycode".into())); }
        // KDE-Wayland arbeitet mit evdev-Hardware-Keycodes (GTK4 unter Wayland
        // liefert genau die in EventControllerKey). evdev-Kernel-Codes sind
        // klein (< 256), aber nicht identisch mit den X11-/XKB-Layout-codes,
        // die das X11-Backend grabbt. Wir übersetzen deshalb mit libei/ei
        // nicht — stattdessen nutzen wir xkbcommon via gdk_keymap (s.u.) bzw.
        // eine eigene Lookup-Tabelle. Damit die UI und der Grab-Mechanismus
        // trotzdem funktionieren, übernehmen wir die raw codes unverändert und
        // mappen sie später beim Registrieren auf Keysyms.
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
            g.iter().filter(|(_, r)| r.shortcut == *shortcut).map(|(n, _)| n.clone()).collect()
        };
        for t in targets { self.unregister_action(&t); }
        Ok(())
    }

    /// Löste den Namen aus einem activated()-Signal zur Ziel-Fenster-ID auf.
    /// Wird von der UI beim BackendEvent::ShortcutTriggered aufgerufen.
    fn resolve_shortcut_trigger(&self, name: &str) -> Option<WindowId> {
        let g = self.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
        // Exakter Treffer (Shortcut-Name aus dem Signal)...
        if let Some(e) = g.get(name) { return Some(e.window_id); }
        // ...oder Pfad-Treffer (Signal transportierte den Action-Pfad).
        g.iter().find(|(n, _)| name.ends_with(&format!("/{n}")) || name.ends_with(&**n))
            .map(|(_, e)| e.window_id)
    }

    fn start_events(&self, tx: Sender<BackendEvent>) -> Result<(), BackendError> {
        let mut guard = self.event_thread.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() { return Ok(()); }
        self.running.store(true, Ordering::Relaxed);
        let handle = Self::spawn_event_loop(
            self.collection.clone(), Arc::clone(&self.running), tx)?;
        *guard = Some(handle);
        Ok(())
    }

    fn stop_events(&self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.event_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}

impl Drop for KdeWaylandBackend {
    fn drop(&mut self) {
        self.stop_events();
        // Temporäre Collection entfernen → alle Hotkeys verschwinden aus KWin.
        let mgr = Proxy::new(KWIN_DEST, GSM_PATH, TIMEOUT, &*self.conn);
        let _: Result<(), _> = mgr.method_call(GSM_IFACE, "deleteCollection", (&self.collection,));
        debugln!("[KWinDBus] Hotkey-Collection entfernt: {}", self.collection);
    }
}

/// Keycode → Keysym: versucht eine X-Verbindung (XWayland läuft unter
/// Plasma-Wayland fast immer), sonst Fallback auf eine statische
/// Standard-XKB-QWERTY-US-Keymap.
fn keycode_to_keysym(keycode: u8) -> Option<u32> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    if std::env::var_os("DISPLAY").is_some() {
        if let Ok((conn, _)) = x11rb::connect(None) {
            if let Ok(cookie) = conn.get_keyboard_mapping(keycode, 1) {
                if let Ok(kbd) = cookie.reply() {
                    // x11rb 0.13: kbd.keysyms ist ein Vec<u32>, jeweils
                    // `keysyms_per_keycode` Einträge pro Keycode. Der erste
                    // Eintrag ist das Basis-Keysym.
                    if let Some(&sym) = kbd.keysyms.first() {
                        if sym != 0 {
                            return Some(sym);
                        }
                    }
                }
            }
        }
    }
    FALLBACK_KEYMAP.get(keycode.checked_sub(8)? as usize).copied().filter(|s| *s != 0)
}

/// Standard-XKB-QWERTY-US-Keymap ab KeyCode 8 (Ausschnitt bis KeyCode 134).
const FALLBACK_KEYMAP: &[u32] = &[
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
        return Some((sym as u8 as char).to_lowercase().to_string());
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
