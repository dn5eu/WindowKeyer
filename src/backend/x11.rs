use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageData, ClientMessageEvent,
    ConnectionExt as _, EventMask, Window, GrabMode, ModMask,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::backend::{BackendError, BackendEvent, WindowBackend};
use crate::models::{Shortcut, WindowId, WindowInfo};

#[derive(Clone)]
struct Atoms {
    supporting_wm_check: Atom,
    client_list_stacking: Atom,
    client_list: Atom,
    active_window: Atom,
    net_wm_name: Atom,
    net_wm_state: Atom,
    state_hidden: Atom,
    state_skip_taskbar: Atom,
    state_skip_pager: Atom,
    wm_window_type: Atom,
    window_type_dock: Atom,
    window_type_desktop: Atom,
    window_type_notification: Atom,
    wm_change_state: Atom,
    utf8_string: Atom,
    wm_state: Atom,
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom, BackendError> {
    let cookie = conn.intern_atom(false, name.as_bytes()).map_err(|e| BackendError::X11(e.to_string()))?;
    Ok(cookie.reply().map_err(|e| BackendError::X11(e.to_string()))?.atom)
}

impl Atoms {
    fn new(conn: &RustConnection) -> Result<Self, BackendError> {
        Ok(Self {
            supporting_wm_check: intern(conn, "_NET_SUPPORTING_WM_CHECK")?,
           client_list_stacking: intern(conn, "_NET_CLIENT_LIST_STACKING")?,
           client_list: intern(conn, "_NET_CLIENT_LIST")?,
           active_window: intern(conn, "_NET_ACTIVE_WINDOW")?,
           net_wm_name: intern(conn, "_NET_WM_NAME")?,
           net_wm_state: intern(conn, "_NET_WM_STATE")?,
           state_hidden: intern(conn, "_NET_WM_STATE_HIDDEN")?,
           state_skip_taskbar: intern(conn, "_NET_WM_STATE_SKIP_TASKBAR")?,
           state_skip_pager: intern(conn, "_NET_WM_STATE_SKIP_PAGER")?,
           wm_window_type: intern(conn, "_NET_WM_WINDOW_TYPE")?,
           window_type_dock: intern(conn, "_NET_WM_WINDOW_TYPE_DOCK")?,
           window_type_desktop: intern(conn, "_NET_WM_WINDOW_TYPE_DESKTOP")?,
           window_type_notification: intern(conn, "_NET_WM_WINDOW_TYPE_NOTIFICATION")?,
           wm_change_state: intern(conn, "WM_CHANGE_STATE")?,
           utf8_string: intern(conn, "UTF8_STRING")?,
           wm_state: intern(conn, "WM_STATE")?,
        })
    }
}

fn prop_values(conn: &RustConnection, win: Window, prop: Atom) -> Result<Vec<u32>, BackendError> {
    let cookie = conn.get_property(false, win, prop, AtomEnum::NONE, 0, u32::MAX).map_err(|e| BackendError::X11(e.to_string()))?;
    let reply = cookie.reply().map_err(|e| BackendError::X11(e.to_string()))?;
    Ok(reply.value32().map(|it| it.collect::<Vec<u32>>()).unwrap_or_default())
}

fn prop_string(conn: &RustConnection, win: Window, prop: Atom, kind: Atom) -> Option<String> {
    let cookie = conn.get_property(false, win, prop, kind, 0, 4096).ok()?;
    let reply = cookie.reply().ok()?;
    if reply.format != 8 { return None; }
    let bytes = reply.value8()?.collect::<Vec<u8>>();
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if text.is_empty() { None } else { Some(text) }
}

fn wm_class(conn: &RustConnection, win: Window) -> String {
    let Ok(cookie) = conn.get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024) else { return String::new(); };
    let Ok(reply) = cookie.reply() else { return String::new(); };
    let Some(values) = reply.value8() else { return String::new(); };
    let bytes = values.collect::<Vec<u8>>();
    let parts: Vec<&[u8]> = bytes.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
    let pick = |i: usize| parts.get(i).copied().map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default();
    let class = pick(1);
    if class.is_empty() { pick(0) } else { class }
}

fn is_hidden(conn: &RustConnection, win: Window, atoms: &Atoms) -> bool {
    if let Ok(state) = prop_values(conn, win, atoms.net_wm_state) {
        if state.contains(&atoms.state_hidden) { return true; }
    }
    if let Ok(state) = prop_values(conn, win, atoms.wm_state) {
        if state.first() == Some(&3) { return true; }
    }
    false
}

fn server_time(_conn: &RustConnection) -> u32 { x11rb::CURRENT_TIME }

pub struct X11Backend {
    conn: Arc<RustConnection>,
    grab_conn: Arc<Mutex<Option<Arc<RustConnection>>>>,
    atoms: Atoms,
    root: Window,
    wm_name: String,
    running: Arc<AtomicBool>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
}

impl X11Backend {
    pub fn new() -> Result<Self, BackendError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| BackendError::Connection(e.to_string()))?;
        let conn = Arc::new(conn);
        let atoms = Atoms::new(&conn)?;
        let root = conn.setup().roots[screen_num].root;

        let wm_window = prop_values(&conn, root, atoms.supporting_wm_check).ok().and_then(|v| v.first().copied()).ok_or(BackendError::NoEwmh)?;
        let check = prop_values(&conn, wm_window, atoms.supporting_wm_check).unwrap_or_default();
        if check.first() != Some(&wm_window) { return Err(BackendError::NoEwmh); }

        let wm_name = prop_string(&conn, wm_window, atoms.net_wm_name, atoms.utf8_string).unwrap_or_else(|| "Unbekannter Window Manager".to_string());

        let _ = conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE));
        let _ = conn.flush();

        debugln!("[X11] verbunden: root={root:#010x}, wm={wm_name}");
        Ok(Self { conn, grab_conn: Arc::new(Mutex::new(None)), atoms, root, wm_name, running: Arc::new(AtomicBool::new(false)), event_thread: Mutex::new(None) })
    }

    fn grab_connection(&self) -> Arc<RustConnection> {
        self.grab_conn.lock().unwrap_or_else(|e| e.into_inner())
            .clone().unwrap_or_else(|| Arc::clone(&self.conn))
    }

    fn should_skip(&self, win: Window) -> bool {
        let types = prop_values(&self.conn, win, self.atoms.wm_window_type).unwrap_or_default();
        let skip_types = [self.atoms.window_type_dock, self.atoms.window_type_desktop, self.atoms.window_type_notification];
        if types.iter().any(|t| skip_types.contains(t)) { return true; }
        let state = prop_values(&self.conn, win, self.atoms.net_wm_state).unwrap_or_default();
        state.contains(&self.atoms.state_skip_taskbar) || state.contains(&self.atoms.state_skip_pager)
    }

    fn send_client_message(&self, target: Window, kind: Atom, data: [u32; 5]) -> Result<(), BackendError> {
        let event = ClientMessageEvent::new(32, target, kind, ClientMessageData::from(data));
        self.conn.send_event(false, self.root, EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT, event)
        .map_err(|e| BackendError::X11(e.to_string()))?.check().map_err(|e| BackendError::X11(e.to_string()))
    }
}

impl WindowBackend for X11Backend {
    fn name(&self) -> &str { "X11/EWMH" }
    fn window_manager_name(&self) -> &str { &self.wm_name }

    fn list_windows(&self) -> Result<Vec<WindowInfo>, BackendError> {
        let mut ids = prop_values(&self.conn, self.root, self.atoms.client_list_stacking).unwrap_or_default();
        if ids.is_empty() { ids = prop_values(&self.conn, self.root, self.atoms.client_list).unwrap_or_default(); }
        ids.reverse();
        let active = prop_values(&self.conn, self.root, self.atoms.active_window).unwrap_or_default().first().copied().unwrap_or(0);
        let mut result = Vec::new();

        for id in ids {
            let Ok(cookie) = self.conn.get_window_attributes(id) else { continue; };
            if cookie.reply().is_err() { continue; }
            if self.should_skip(id) { continue; }

            let title = prop_string(&self.conn, id, self.atoms.net_wm_name, self.atoms.utf8_string)
            .or_else(|| prop_string(&self.conn, id, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
            .unwrap_or_else(|| format!("Fenster {id:#x}"));
            let app_id = wm_class(&self.conn, id);

            result.push(WindowInfo { id, title, app_id, minimized: is_hidden(&self.conn, id, &self.atoms), active: id == active });
        }
        Ok(result)
    }

    fn activate_window(&self, id: WindowId) -> Result<(), BackendError> {
        let cookie = self.conn.get_window_attributes(id).map_err(|e| BackendError::X11(e.to_string()))?;
        cookie.reply().map_err(|_| BackendError::NoSuchWindow(id))?;

        let hidden = is_hidden(&self.conn, id, &self.atoms);
        let time = server_time(&self.conn);
        debugln!("[X11] AKTIVIEREN: ziel={id:#010x}, minimiert={hidden}, timestamp={time}");

        if hidden {
            self.send_client_message(id, self.atoms.net_wm_state, [0, self.atoms.state_hidden, 0, 2, 0])?;
            debugln!("[X11]   -> _NET_WM_STATE REMOVE HIDDEN gesendet");
        }
        self.send_client_message(id, self.atoms.active_window, [2, time, 0, 0, 0])?;
        debugln!("[X11]   -> _NET_ACTIVE_WINDOW gesendet");
        self.conn.flush().map_err(|e| BackendError::X11(e.to_string()))?;
        debugln!("[X11]   -> flush OK");
        Ok(())
    }

    fn minimize_window(&self, id: WindowId) -> Result<(), BackendError> {
        debugln!("[X11] MINIMIEREN: ziel={id:#010x}");
        self.send_client_message(id, self.atoms.net_wm_state, [1, self.atoms.state_hidden, 0, 2, 0])?;
        self.send_client_message(id, self.atoms.wm_change_state, [3, 0, 0, 0, 0])?;
        self.conn.flush().map_err(|e| BackendError::X11(e.to_string()))?;
        debugln!("[X11]   -> flush OK");
        Ok(())
    }

    fn create_shortcut(&self, display_text: String, keycode: u8, modifiers: u16) -> Result<Shortcut, BackendError> {
        if keycode == 0 { return Err(BackendError::Unsupported("Ungültiger Keycode".into())); }
        // KORREKTUR: Masken definieren — CapsLock/NumLock/ScrollLock (Locks) und die
        // X11-"Blinker"-Masken (X11_MASK), die GTK beim Tastendruck mitliefert, aber
        // nicht Teil der eigentlichen Kombination sind.
        const LOCK_MASK: u16 = 0x8002; // ShiftLock (CapsLock) | Lock (NumLock) | Mod5 (ScrollLock)
        const X11_MASK: u16 = 0x80 | 0x4000 | 0x8000; // X11KeyDial | AltGr | Super
        let clean_modifiers = modifiers & !(LOCK_MASK | X11_MASK);
        Ok(Shortcut { display_text, keycode, modifiers: clean_modifiers })
    }

    fn grab_hotkey(&self, shortcut: &Shortcut) -> Result<(), BackendError> {
        // KORREKTUR: "locked" (2. Parameter) auf false — sonst wird der Hotkey bei
        // aktivem Lock (z.B. NumLock) nicht ausgelöst. Zusätzlich werden alle
        // denkbaren Lock-/Blinker-Kombinationen gegrabt, damit der Grab auch bei
        // gedrückter Feststelltaste greift.
        let conn = self.grab_connection();
        let lock_masks = [0x0000u16, 0x0002, 0x0010, 0x0012, 0x4000, 0x4002, 0x4010, 0x4012];
        for extra in lock_masks {
            let mask = ModMask::from(shortcut.modifiers | extra);
            let _ = conn.grab_key(false, self.root, mask, shortcut.keycode, GrabMode::ASYNC, GrabMode::ASYNC);
        }
        conn.flush().map_err(|e| BackendError::X11(e.to_string()))?;
        Ok(())
    }

    fn ungrab_hotkey(&self, shortcut: &Shortcut) -> Result<(), BackendError> {
        let conn = self.grab_connection();
        let lock_masks = [0x0000u16, 0x0002, 0x0010, 0x0012, 0x4000, 0x4002, 0x4010, 0x4012];
        for extra in lock_masks {
            let mask = ModMask::from(shortcut.modifiers | extra);
            let _ = conn.ungrab_key(shortcut.keycode, self.root, mask);
        }
        conn.flush().map_err(|e| BackendError::X11(e.to_string()))?;
        Ok(())
    }

    fn start_events(&self, tx: Sender<BackendEvent>) -> Result<(), BackendError> {
        let mut guard = self.event_thread.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() { return Ok(()); }

        // KORREKTUR: Eigene X11-Verbindung nur für Events/Grabs. Grabs und
        // KeyEvents dürfen nicht über die geteilte UI-Connection laufen, sonst
        // konkurrieren Event-Loop und UI-Thread um dieselben Antworten (Race).
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| BackendError::Connection(e.to_string()))?;
        let root = conn.setup().roots[screen_num].root;
        let conn = Arc::new(conn);

        // Root für KeyEvents UND Struktur-/Property-Changes abonnieren.
        // KORREKTUR: Die Event-Maske des Root-Fensters wird vom Server ersetzt, nicht
        // addiert — daher müssen alle Masken in EINER Änderung gesetzt werden.
        let _ = conn.change_window_attributes(root, &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE
                | EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE));
        let _ = conn.flush();

        // Grab-Operationen über dieselbe Connection wie den Event-Loop ausführen,
        // damit Grabs nicht mit UI-Replies auf der Haupt-Connection vermischt werden.
        *self.grab_conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&conn));

        self.running.store(true, Ordering::Relaxed);
        let handle = spawn_event_loop(conn, self.atoms.clone(), root, Arc::clone(&self.running), tx)?;
        *guard = Some(handle);
        Ok(())
    }

    fn stop_events(&self) {
        self.running.store(false, Ordering::Relaxed);
        // KORREKTUR: RustConnection (x11rb 0.13) hat kein disconnect() — der
        // Event-Thread beendet sich selbst über das running-Flag (poll_for_event
        // ist hier eh ein nicht-blockierendes Polling mit 20ms-Sleep), danach
        // wird die Connection durch Drop des Arcs geschlossen.
        if let Some(conn) = self.grab_conn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = conn.flush();
        }
        if let Some(handle) = self.event_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

impl Drop for X11Backend {
    fn drop(&mut self) { self.stop_events(); }
}

fn spawn_event_loop(conn: Arc<RustConnection>, atoms: Atoms, root: Window, running: Arc<AtomicBool>, tx: Sender<BackendEvent>) -> Result<JoinHandle<()>, BackendError> {
    std::thread::Builder::new().name("x11-events".into()).spawn(move || {
        let mut changed = false;
        let mut last_emit = Instant::now();

        while running.load(Ordering::Relaxed) {
            match conn.poll_for_event() {
                Ok(Some(event)) => match event {
                    Event::DestroyNotify(e) => {
                        // KORREKTUR: e.window ist das zerstörte Fenster, e.event ist nur das Root-Window, auf dem wir lauschen
                        let _ = tx.send(BackendEvent::WindowClosed(e.window));
                        changed = true;
                    }
                    Event::KeyRelease(e) => {
                        // KORREKTUR: KeyRelease statt KeyPress verwenden. Bei Grab-Events
                        // wird der Keycode des eigentlichen Hotkeys garantiert korrekt
                        // geliefert, und die Fensteraktivierung erfolgt beim Loslassen
                        // (Standardverhalten von Hotkey-Daemons). Lock-Bits bereinigen.
                        const LOCK_MASK: u16 = 0x8002; // ShiftLock | Lock (NumLock) | Mod5
                        let mods = u16::from(e.state) & !LOCK_MASK;
                        let _ = tx.send(BackendEvent::KeyPress(e.detail, mods));
                    }
                    Event::CreateNotify(_) | Event::MapNotify(_) | Event::UnmapNotify(_) | Event::ConfigureNotify(_) | Event::ReparentNotify(_) => {
                        changed = true;
                    }
                    Event::PropertyNotify(e) => {
                        if e.window == root && (e.atom == atoms.active_window || e.atom == atoms.client_list || e.atom == atoms.client_list_stacking) {
                            changed = true;
                        }
                    }
                    _ => {}
                },
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(BackendEvent::BackendLost(e.to_string()));
                    break;
                }
            }
            let _ = conn.flush();
            if changed && last_emit.elapsed() >= Duration::from_millis(100) {
                let _ = tx.send(BackendEvent::WindowsChanged);
                changed = false;
                last_emit = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }).map_err(|e| BackendError::Connection(format!("Event-Thread: {e}")))
}
