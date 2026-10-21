use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use gtk4::prelude::*;
use gtk4::gdk::{Key, ModifierType};
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, HeaderBar, Label,
    ListBox, ListBoxRow, Orientation, PolicyType, ScrolledWindow, SelectionMode,
};

use crate::backend::{BackendError, BackendEvent, WindowBackend};
use crate::hotkey_manager::HotkeyManager;
use crate::models::{Shortcut, WindowInfo};
use crate::backend::detection::SessionInfo;

#[derive(Clone)]
pub enum UiCommand { Refresh, Status(String), ShowWindow, Quit, }

struct RowViews {
    row: ListBoxRow,
    state_label: Label,
    subtitle: Label,
    hotkey_label: Label,
    remove_hotkey_button: Button,
}

struct UiState {
    backend: Arc<dyn WindowBackend>,
    list_box: ListBox,
    status_label: Label,
    session: SessionInfo,
    backend_events: Mutex<Receiver<BackendEvent>>,
    commands: Mutex<Receiver<UiCommand>>,
    commands_tx: Sender<UiCommand>,
    app: Application,
    hotkey_manager: Arc<Mutex<HotkeyManager>>,
    identity: RefCell<Vec<(u32, String, String, bool)>>,
    rows: RefCell<Vec<RowViews>>,
    ids: Rc<RefCell<Vec<u32>>>,
    ticks: Cell<u32>,
    poll_source_id: Cell<Option<glib::SourceId>>,
}

fn subtitle_text(info: &WindowInfo) -> String {
    format!("{} · 0x{:x}", if info.app_id.is_empty() { "unbekannte App" } else { &info.app_id }, info.id)
}

fn request_activate(backend: &Arc<dyn WindowBackend>, commands: &Sender<UiCommand>, id: u32) {
    debugln!("[UI] Aktivierungs-Anfrage für 0x{id:x}");
    let result = backend.activate_window(id);
    report(result, commands, id, "Fokus angefordert");
    let _ = commands.send(UiCommand::Refresh);
}

fn request_minimize(backend: &Arc<dyn WindowBackend>, commands: &Sender<UiCommand>, id: u32) {
    debugln!("[UI] Minimierungs-Anfrage für 0x{id:x}");
    let result = backend.minimize_window(id);
    report(result, commands, id, "Minimierung angefordert");
    let _ = commands.send(UiCommand::Refresh);
}

fn report(result: Result<(), BackendError>, commands: &Sender<UiCommand>, id: u32, success_text: &str) {
    match result {
        Ok(()) => { let _ = commands.send(UiCommand::Status(format!("{success_text} (0x{:x})", id))); }
        Err(e) => {
            eprintln!("[UI] Backend-Fehler: {e}");
            let _ = commands.send(UiCommand::Status(format!("Fehler: {e}")));
        }
    }
}

fn build_row(info: &WindowInfo, backend: Arc<dyn WindowBackend>, commands: Sender<UiCommand>, hotkey_manager: Arc<Mutex<HotkeyManager>>) -> RowViews {
    let title = Label::new(Some(&info.title));
    title.set_halign(Align::Start); title.set_ellipsize(pango::EllipsizeMode::End); title.set_max_width_chars(40); title.add_css_class("heading");

    let subtitle = Label::new(Some(&subtitle_text(info)));
    subtitle.set_halign(Align::Start); subtitle.add_css_class("dim-label");

    let texts = GtkBox::new(Orientation::Vertical, 2);
    texts.append(&title); texts.append(&subtitle);

    let state_label = Label::new(Some(info.state_label()));
    state_label.set_width_chars(10); state_label.add_css_class("dim-label");

    let hotkey_label = Label::new(Some("Kein Hotkey"));
    hotkey_label.set_width_chars(12); hotkey_label.add_css_class("dim-label");

    let focus_button = Button::with_label("Fokus");
    let minimize_button = Button::with_label("Minimieren");
    let hotkey_button = Button::with_label("Hotkey");
    let remove_hotkey_button = Button::with_label("X");
    remove_hotkey_button.set_tooltip_text(Some("Hotkey entfernen"));

    let buttons = GtkBox::new(Orientation::Horizontal, 6);
    buttons.append(&focus_button); buttons.append(&minimize_button); buttons.append(&hotkey_button); buttons.append(&remove_hotkey_button);

    let row_box = GtkBox::new(Orientation::Horizontal, 12);
    row_box.set_margin_start(12); row_box.set_margin_end(12); row_box.set_margin_top(8); row_box.set_margin_bottom(8);
    row_box.append(&texts); row_box.append(&state_label); row_box.append(&hotkey_label); row_box.append(&buttons);

    let row = ListBoxRow::new();
    row.set_child(Some(&row_box)); row.set_activatable(true);

    {
        let backend = Arc::clone(&backend);
        let commands = commands.clone();
        let id = info.id;
        focus_button.connect_clicked(move |_| { request_activate(&backend, &commands, id); });
    }
    {
        let backend = Arc::clone(&backend);
        let commands = commands.clone();
        let id = info.id;
        minimize_button.connect_clicked(move |_| { request_minimize(&backend, &commands, id); });
    }
    {
        let backend = Arc::clone(&backend);
        let commands = commands.clone();
        let hotkey_manager = Arc::clone(&hotkey_manager);
        let id = info.id;
        let title = info.title.clone();
        hotkey_button.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<ApplicationWindow>().unwrap();
            if let Some(shortcut) = capture_hotkey(&parent, &title, Arc::clone(&backend)) {
                let mut manager = hotkey_manager.lock().unwrap();
                if let Err(e) = manager.assign(shortcut, id, backend.as_ref()) {
                    eprintln!("[UI] Hotkey-Zuweisung fehlgeschlagen: {e}");
                }
                let _ = commands.send(UiCommand::Refresh);
            }
        });
    }
    {
        let backend = Arc::clone(&backend);
        let commands = commands.clone();
        let hotkey_manager = Arc::clone(&hotkey_manager);
        let id = info.id;
        remove_hotkey_button.connect_clicked(move |_| {
            let mut manager = hotkey_manager.lock().unwrap();
            manager.remove_all_for_window(id, backend.as_ref());
            let _ = commands.send(UiCommand::Refresh);
        });
    }

    RowViews { row, state_label, subtitle, hotkey_label, remove_hotkey_button }
}

fn capture_hotkey(parent: &impl gtk4::prelude::IsA<gtk4::Window>, window_title: &str, backend: Arc<dyn WindowBackend>) -> Option<Shortcut> {
    let window = gtk4::Window::builder()
    .title("Hotkey zuweisen")
    .transient_for(parent)
    .modal(true)
    .default_width(320)
    .default_height(160)
    .build();

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    content.set_margin_start(20);
    content.set_margin_end(20);
    content.set_margin_top(20);
    content.set_margin_bottom(20);
    content.append(&gtk4::Label::new(Some(&format!("Tastenkombination für:\n\"{}\"", window_title))));

    let status = gtk4::Label::new(Some("Beliebige Taste drücken (Esc zum Abbrechen)"));
    status.add_css_class("dim-label");
    content.append(&status);
    window.set_child(Some(&content));

    let result = Rc::new(RefCell::new(None));
    let result_clone = Rc::clone(&result);
    let backend_clone = Arc::clone(&backend);
    let window_clone = window.clone();

    let main_loop = glib::MainLoop::new(None, false);
    let main_loop_for_key = main_loop.clone();
    let main_loop_for_close = main_loop.clone();

    // KORREKTUR: Auf key_released lauschen statt key_pressed. GTK liefert beim
    // Drücken von Shift+taste einen anderen Keycode als beim X11-Grab (der an der
    //physischen Taste hängt). Beim Loslassen meldet GTK denselben Basis-Keycode wie
    // das KeyRelease-Event des Grabs — dadurch funktionieren auch Shift-Kombinationen.
    // Abbruch schon beim Drücken von Esc (nicht erst beim Loslassen)
    let esc_controller = gtk4::EventControllerKey::new();
    let window_for_esc = window.clone();
    let loop_for_esc = main_loop.clone();
    esc_controller.connect_key_pressed(move |_controller, keyval, _keycode, _state| {
        if keyval == Key::Escape {
            window_for_esc.hide();
            loop_for_esc.quit();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(esc_controller);

    let controller = gtk4::EventControllerKey::new();
    // KORREKTUR: In gtk4 0.9 liefert connect_key_released eine Closure, deren
    // Callback (u32, u32, ModifierType) -> () zurueckgeben muss — die
    // Propagation-Rueckgabe gibt es erst ab gtk4 0.10. Der Rueckgabewert ist hier
    // ohnehin wirkungslos: Die Hotkey-Erkennung laeuft in einem eigenen modalen
    // Fenster mit lokalem MainLoop und stoppt die Aufnahme ueber main_loop.quit().
    controller.connect_key_released(move |_controller, keyval, keycode, state| {
        if keyval == Key::Escape || is_modifier_only(keyval) {
            return;
        }

        let display_text = format_keyval_state(keyval, state);
        status.set_text(&format!("Erkannt: {}", display_text));

        if keycode > 0 && keycode <= 255 {
            if let Ok(shortcut) = backend_clone.create_shortcut(display_text, keycode as u8, state.bits() as u16) {
                *result_clone.borrow_mut() = Some(shortcut);
                window_clone.hide();
                main_loop_for_key.quit();
            } else {
                status.set_text("Ungültige Taste oder Kombination.");
            }
        }
    });
    window.add_controller(controller);

    window.connect_close_request(move |_| {
        main_loop_for_close.quit();
        glib::Propagation::Stop
    });

    window.present();
    main_loop.run();

    result.take()
}

fn is_modifier_only(keyval: Key) -> bool {
    matches!(keyval, Key::Control_L | Key::Control_R | Key::Alt_L | Key::Alt_R | Key::Shift_L | Key::Shift_R | Key::Super_L | Key::Super_R | Key::Caps_Lock | Key::Num_Lock | Key::Scroll_Lock)
}

fn format_keyval_state(keyval: Key, state: ModifierType) -> String {
    let mut parts = Vec::new();
    if state.contains(ModifierType::CONTROL_MASK) { parts.push("Strg"); }
    if state.contains(ModifierType::ALT_MASK) { parts.push("Alt"); }
    if state.contains(ModifierType::SHIFT_MASK) { parts.push("Shift"); }
    if state.contains(ModifierType::SUPER_MASK) { parts.push("Super"); }

    let key_name = keyval.name().map(|s| s.to_string()).unwrap_or_else(|| format!("Key{}", keyval));
    let mut chars = key_name.chars();
    let key_name_formatted = match chars.next() { None => String::new(), Some(f) => f.to_uppercase().collect::<String>() + chars.as_str() };
    parts.push(&key_name_formatted);
    parts.join("+")
}

impl UiState {
    fn set_status(&self, text: &str) { self.status_label.set_text(text); }

    fn refresh(&self) {
        let windows = match self.backend.list_windows() {
            Ok(w) => w,
            Err(e) => { self.set_status(&format!("Backend-Fehler: {e}")); return; }
        };

        let identity: Vec<(u32, String, String, bool)> = windows.iter().map(|w| (w.id, w.title.clone(), w.app_id.clone(), w.minimized)).collect();
        let needs_rebuild = self.identity.borrow().as_slice() != identity.as_slice();

        if needs_rebuild {
            debugln!("[UI] Liste neu aufbauen: {} Fenster", windows.len());
            while let Some(child) = self.list_box.first_child() { self.list_box.remove(&child); }
            let mut new_rows = Vec::new(); let mut new_ids = Vec::new();
            for info in &windows {
                let views = build_row(info, Arc::clone(&self.backend), self.commands_tx.clone(), Arc::clone(&self.hotkey_manager));
                new_ids.push(info.id); new_rows.push(views);
            }
            for views in &new_rows { self.list_box.append(&views.row); }
            *self.rows.borrow_mut() = new_rows; *self.ids.borrow_mut() = new_ids; *self.identity.borrow_mut() = identity;
        }

        {
            let rows = self.rows.borrow();
            let manager = self.hotkey_manager.lock().unwrap();
            for (index, info) in windows.iter().enumerate() {
                if let Some(views) = rows.get(index) {
                    views.state_label.set_text(info.state_label());
                    views.subtitle.set_text(&subtitle_text(info));
                    let shortcut_text = manager.get_shortcut_for_window(info.id).unwrap_or_else(|| "Kein Hotkey".to_string());
                    views.hotkey_label.set_text(&shortcut_text);
                    views.remove_hotkey_button.set_sensitive(manager.get_shortcut_for_window(info.id).is_some());
                }
            }
        }

        let mut status = format!("{} Fenster · Backend: {} · WM: {}", windows.len(), self.backend.name(), self.backend.window_manager_name());
        if let Some(warning) = self.session.warning() { status.push_str(&format!(" — {warning}")); }
        if windows.is_empty() { status.push_str(" — Keine passenden Fenster gefunden."); }
        self.set_status(&status);
    }

    fn poll(&self) {
        let mut refresh = false; let mut status_message: Option<String> = None;

        { let rx = self.backend_events.lock().unwrap_or_else(|e| e.into_inner());
            while let Ok(event) = rx.try_recv() {
                match event {
                    BackendEvent::WindowsChanged => refresh = true,
                    BackendEvent::WindowClosed(id) => {
                        debugln!("[UI] Fenster geschlossen: 0x{id:x} -> Hotkeys werden entfernt");
                        self.hotkey_manager.lock().unwrap().on_window_closed(id, self.backend.as_ref());
                        refresh = true;
                    }
                    BackendEvent::KeyPress(keycode, modifiers) => {
                        if let Some(window_id) = self.hotkey_manager.lock().unwrap().find_shortcut(keycode, modifiers) {
                            request_activate(&self.backend, &self.commands_tx, window_id);
                        }
                    }
                    // Phase 3a (KDE Wayland): KWin meldet den Shortcut-/Action-
                    // Namen aus dem activated()-Signal; das Backend löst daraus
                    // die Ziel-Fenster-ID auf.
                    BackendEvent::ShortcutTriggered(name) => {
                        if let Some(window_id) = self.backend.resolve_shortcut_trigger(&name) {
                            debugln!("[UI] Shortcut '{name}' -> Fenster 0x{window_id:x}");
                            request_activate(&self.backend, &self.commands_tx, window_id);
                        } else {
                            debugln!("[UI] Shortcut-Trigger '{name}' ohne bekanntes Fenster ignoriert");
                        }
                    }
                    BackendEvent::BackendLost(msg) => { status_message = Some(format!("Verbindung verloren: {msg}")); refresh = true; }
                }
            }
        }

        { let rx = self.commands.lock().unwrap_or_else(|e| e.into_inner());
            while let Ok(command) = rx.try_recv() {
                match command {
                    UiCommand::Refresh => refresh = true,
                    UiCommand::Status(msg) => status_message = Some(msg),
                    UiCommand::ShowWindow => {
                        // Wird im Main-Loop-Handler von main.rs behandelt
                    }
                    UiCommand::Quit => {
                        // KORREKTUR: Kein process::exit(0) mehr — das übersprang Drop
                        // (X11-Grabs blieben aktiv!) und konnte mitten im Lock passieren.
                        // Stattdessen sauberes Herunterfahren über die Application.
                        self.app.quit();
                    }
                }
            }
        }

        if let Some(msg) = status_message { self.set_status(&msg); }
        self.ticks.set(self.ticks.get() + 1);
        if refresh || self.ticks.get() % 7 == 0 { self.refresh(); }
    }
}

pub fn build_ui(app: &Application, backend: Arc<dyn WindowBackend>, session: SessionInfo, hotkey_manager: Arc<Mutex<HotkeyManager>>, _commands_tx: Sender<UiCommand>) -> gtk4::ApplicationWindow {
    let window = ApplicationWindow::builder().application(app).title("Windowkeyer").default_width(950).default_height(560).build();
    let header = HeaderBar::new(); header.set_show_title_buttons(true);
    let refresh_button = Button::with_label("Aktualisieren"); header.pack_end(&refresh_button);

    let list_box = ListBox::new(); list_box.set_selection_mode(SelectionMode::Single); list_box.set_activate_on_single_click(true);
    let scrolled = ScrolledWindow::builder().child(&list_box).hexpand(true).vexpand(true).build();
    scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);

    let status_label = Label::new(Some("Fenster werden geladen …"));
    status_label.set_halign(Align::Start); status_label.set_xalign(0.0); status_label.set_wrap(true); status_label.set_max_width_chars(100);
    status_label.set_margin_start(12); status_label.set_margin_end(12); status_label.set_margin_top(8); status_label.set_margin_bottom(8);
    status_label.add_css_class("dim-label");

    let content = GtkBox::new(Orientation::Vertical, 0); content.append(&scrolled); content.append(&status_label);
    window.set_titlebar(Some(&header)); window.set_child(Some(&content));

    let (backend_tx, backend_rx) = unbounded::<BackendEvent>();
    let (commands_tx, commands_rx) = unbounded::<UiCommand>();
    let ids: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));

    let state = Rc::new(UiState {
        backend: Arc::clone(&backend), list_box: list_box.clone(), status_label, session, app: app.clone(),
                        backend_events: Mutex::new(backend_rx), commands: Mutex::new(commands_rx), commands_tx: commands_tx.clone(),
                        hotkey_manager, identity: RefCell::new(Vec::new()), rows: RefCell::new(Vec::new()), ids: Rc::clone(&ids), ticks: Cell::new(0),
                        poll_source_id: Cell::new(None),
    });

    {
        let backend = Arc::clone(&backend); let commands = commands_tx.clone(); let ids = Rc::clone(&ids);
        list_box.connect_row_activated(move |_box, row| {
            let index = row.index(); debugln!("[UI] row-activated: index={index}");
            if index < 0 { return; }
            let id = ids.borrow().get(index as usize).copied();
            match id { Some(id) => request_activate(&backend, &commands, id), None => debugln!("[UI] row-activated: keine ID für Index {index}") }
        });
    }

    state.refresh();
    if let Err(e) = backend.start_events(backend_tx) { state.set_status(&format!("Event-Loop nicht gestartet: {e}.")); }

    // KORREKTUR: Der Polling-Timer wird an das Fenster gehängt und beim Destroy
    // automatisch entfernt. Vorher lief bei jeder Re-Aktivierung (z.B. Tray-Klick
    // nach Fenster-Schließen) ein alter Timer weiter, der ins Leere pollte.
    // In glib 0.20 wird der Source über SourceId::remove(self) entfernt
    // (id.destroy() existiert nicht mehr).
    let state_for_destroy = Rc::clone(&state);
    window.connect_destroy(move |_| {
        if let Some(id) = state_for_destroy.poll_source_id.take() {
            id.remove();
        }
    });
    { let state_for_timer = Rc::clone(&state);
      let state = Rc::clone(&state);
      let id = glib::timeout_add_local(Duration::from_millis(300), move || { state.poll(); glib::ControlFlow::Continue });
      state_for_timer.poll_source_id.set(Some(id)); }
    { let commands_tx = commands_tx.clone(); refresh_button.connect_clicked(move |_| { let _ = commands_tx.send(UiCommand::Refresh); }); }

    let window_for_close = window.clone();
    window.connect_close_request(move |_| {
        window_for_close.hide();
        glib::Propagation::Stop // Verhindert, dass GTK das Fenster schließt
    });

    window.present();
    window // Rückgabe hinzufügen
}

pub fn show_error(app: &Application, message: &str) {
    let window = ApplicationWindow::builder().application(app).title("Windowkeyer — Startfehler").default_width(560).default_height(240).build();
    let header = HeaderBar::new(); header.set_show_title_buttons(true);
    let label = Label::new(Some(message)); label.set_wrap(true); label.set_max_width_chars(70);
    label.set_margin_top(24); label.set_margin_bottom(24); label.set_margin_start(24); label.set_margin_end(24);
    window.set_titlebar(Some(&header)); window.set_child(Some(&label)); window.present();
}

