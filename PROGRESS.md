# Windowkeyer – Projekt-Zusammenfassung für Coding Agent

> Stand: nach den Review-Fix-Commits `f27a497` und `151100a` (X11-Hotkeys stabil, Build fehlerfrei).

## Projekt-Überblick

**Windowkeyer** ist eine Desktop-Anwendung in Rust, die es ermöglicht, beliebigen Fenstern temporäre globale Hotkeys zuzuweisen. Die Hotkeys existieren nur im RAM und werden automatisch entfernt, sobald das zugehörige Fenster geschlossen wird.

- **Sprache:** Rust (Edition 2021)
- **GUI:** GTK 4 (via `gtk4` 0.9)
- **Aktuelle Version:** 0.1.0
- **Zielplattform:** Linux, primär KDE Plasma (X11 + Wayland via XWayland)

## Aktueller Stand

### ✅ Abgeschlossene Phasen

**Phase 1 – X11-Backend (EWMH)**
- Automatische Erkennung von X11 / Wayland / Desktop-Umgebung (`src/backend/detection.rs`)
- Funktionierendes X11-Backend: Fenster listen, aktivieren, minimieren
- Korrekte Kommunikation mit KWin über EWMH-Protokoll (`_NET_ACTIVE_WINDOW`, `_NET_WM_STATE`)
- Event-Loop im Hintergrund-Thread für `WindowClosed`-Erkennung

**Phase 2 – Temporäre Hotkeys**
- RAM-basierter `HotkeyManager` mit automatischem Cleanup bei `WindowClosed`
- Aufzeichnungsdialog für Tastenkombinationen (GTK4 `EventControllerKey`)
- Globales `XGrabKey`/`XUngrabKey` auf dem Root-Window
- Berücksichtigung von CapsLock/NumLock (Extra-Grabs für Modifier-Kombinationen)

**Phase 2b – Tray-Icon**
- Natives KDE-Tray-Icon via `ksni` (D-Bus, StatusNotifierItem)
- **Wichtig:** `tray-icon`-Crate funktioniert NICHT, weil es GTK 3 erzwingt und mit GTK 4 kollidiert!
- Close-to-Tray-Verhalten (Fenster wird nur versteckt, nicht zerstört)
- Linksklick öffnet das Fenster, Rechtsklick zeigt Menü

**Phase 2c – Review-Fixes (neu)**
- Modifier-Maske in `find_shortcut` korrigiert (Alt/Super werden jetzt korrekt gematcht)
- Idempotenz-Guard in `HotkeyManager::assign` (kein Grab-Leak bei Re-Assign desselben Shortcuts)
- Eigene X11-Verbindung für Grabs vs. Event-Loop (Race Conditions zwischen UI-Thread und Grab-Thread behoben)
- Hotkey-Aufzeichnung auf `connect_key_released` umgestellt (korrekte Keycodes/Modifier bei Shift-Kombinationen)
- Sauberes Beenden: kein `std::process::exit(0)` mehr, stattdessen `app.quit()`; Tray-Initialisierung per `tray_ready`-Guard vor doppelter Registrierung geschützt (App re-activation nach Close-to-Tray)

**Phase 3a – KDE Plasma Wayland Backend (neu)**
- Natives Backend via KWin-D-Bus (`src/backend/kde_wayland.rs`): Fensterliste/Aktivieren/Minimieren über `org.kde.KWin`, globale Hotkeys über KWin GlobalShortcuts (`org.kde.GlobalShortcutManager`, temporäre Collection, Drop räumt auf)
- Hotkey-Auslösung: `activated()`-Signal → `BackendEvent::ShortcutTriggered(name)` → `resolve_shortcut_trigger` → Fenster aktivieren
- WindowClosed-Erkennung per Polling (~2 s); `main.rs` prüft vorab `is_available()` und fällt sauber auf X11/XWayland zurück

### ⏳ Offene Phasen

**Phase 3b/3c – GNOME & wlroots**
- GNOME: Shell Extension + D-Bus
- wlroots (Sway/Hyprland): `wlr-foreign-toplevel-management`

## Architektur

```
src/
├── main.rs                    # Entry point, Backend-Auswahl, GTK-App
├── debug.rs                   # Debug-Logging via --debug Flag + debugln! Makro
├── models.rs                  # WindowInfo, Shortcut, WindowId
├── hotkey_manager.rs          # RAM-Manager für Shortcut→Window-Mappings
├── tray.rs                    # KDE Tray-Icon via ksni
├── ui/mod.rs                  # GTK4 UI (Fensterliste, Hotkey-Dialog)
└── backend/
    ├── mod.rs                 # WindowBackend-Trait, BackendError, BackendEvent
    ├── detection.rs           # Session/ Desktop-Erkennung
    ├── x11.rs                 # X11/EWMH-Backend (vollständig)
    └── kde_wayland.rs         # KDE-Wayland-Backend (KWin D-Bus, Phase 3a)
```

## Wichtige technische Details

### Verwendete Crates
```toml
gtk4 = "0.9"
glib = "0.20"
pango = "0.20"
libadwaita = { version = "0.7", features = ["v1_4"], optional = true }  # optionales Feature "adwaita", aktuell ungenutzt
x11rb = "0.13"
crossbeam-channel = "0.5"
thiserror = "2"
ksni = "0.2"
```

### Bekannte API-Pitfalls (aus der Entwicklung)
- **`ksni` 0.2:**
  - `tool_tip()` gibt `ToolTip` zurück (nicht `String`)
  - `ToolTip` hat Felder `title`, `description`, `icon_name` (NICHT `body`)
  - `activate(&mut self, x: i32, y: i32)` für Linksklick (X/Y-Koordinaten ignorierbar)
  - `MenuItem::Standard(StandardItem {...})` und `MenuItem::Separator` (kein `Separator::default()`)
  - Closure in `StandardItem::activate` hat Signatur `Box<dyn FnMut(&mut Self)>`
  - `service.spawn()` startet eigenen Thread, ist **kein** Future (kein `tokio::spawn`)
- **GTK 4:**
  - `HeaderBar` hat keine `set_title`/`set_subtitle` (Titel kommt vom `ApplicationWindow`)
  - `ModifierType::ALT_MASK` (nicht `MOD1_MASK`)
  - gdk4 ist nicht separat verfügbar, alles unter `gtk4::gdk`
  - `EventControllerKey::connect_key_pressed` liefert `keyval: Key`, `keycode: u32`, `state: ModifierType`
  - Für die Hotkey-Aufzeichnung `connect_key_released` verwenden — bei `key_pressed` liefert GTK unter manchen Layouts/Shift-Kombinationen den noch unveränderten Keycode (Shift selbst wird als Modifier erst mitgeliefert, wenn die Taste losgelassen wird)
- **X11/EWMH:**
  - `_NET_ACTIVE_WINDOW` und `_NET_WM_STATE` müssen an das **Root-Window** gesendet werden (nicht an das Zielfenster!)
  - Minimiertes Fenster需要先 `_NET_WM_STATE` REMOVE HIDDEN, dann `_NET_ACTIVE_WINDOW`
  - `DestroyNotify`: `e.window` ist das zerstörte Fenster, `e.event` ist das Root-Window
  - `e.state` in `KeyPress`/`KeyRelease` ist `KeyButMask`, via `u16::from()` konvertieren
  - `ungrab_key` Signatur (x11rb 0.13): `(keycode, window, modifiers)` – **nicht** `(window, modifiers, keycode)`!
  - Lock-/Blinker-Bits beim Grabbing bereinigen: `LOCK_MASK = 0x8002` (ShiftLock | Mod2/NumLock | Mod5/ScrollLock); zusätzlich Extra-Grabs für alle Lock-Kombinationen (`0x0000, 0x0002, 0x0010, 0x0012, 0x4000, 0x4002, 0x4010, 0x4012`), damit der Hotkey auch bei aktivem NumLock/CapsLock feuert
  - Grabs und Event-Loop sollten getrennte X11-Verbindungen nutzen (x11rb-Connection ist nicht thread-sicher genug für gleichzeitige Grab-Aufrufe + Event-Reads)

## Entwicklungsumgebung (NixOS)

Das Projekt wird in einer `nix-shell` entwickelt. Die `shell.nix` enthält alle Abhängigkeiten. Wichtige Punkte:
- `xdotool` statt `xdo` (liefert `libxdo.so`)
- `dbus` für `ksni` (D-Bus-Entwicklungsbibliotheken)
- Keine GTK 3 Abhängigkeiten mehr nötig (nach Wechsel zu `ksni`)
- `pkg-config` findet nicht alle Pakete automatisch → `LIBRARY_PATH`/`LD_LIBRARY_PATH` manuell setzen

## Bekannte Limitierungen

1. **Wayland:** Unter KDE Plasma nativ unterstützt (Phase 3a, KWin D-Bus). Auf anderen Wayland-Desktops (GNOME/wlroots) aktuell nur XWayland-Fenster sichtbar (Phase 3b/3c offen).
2. **KWin Focus Stealing Prevention:** Unter bestimmten Bedingungen blockiert KWin Fokus-Requests von fremden Apps. Das Fenster kommt dann zwar nach vorne, bekommt aber nicht immer den Tastaturfokus (Taskleisten-Icon blinkt).
3. **Debug-Ausgabe:** GTK gibt beim Start `Unbekannte Option --debug` aus (harmlos, kommt von GTKs CLI-Parser, nicht von unserem Code).
4. **Compiler-Warnungen:** Build ist wieder warnungsfrei (toter Code aus der Platzhalter-Ära entfernt: ungenutzte Trait-Methode `uses_x11_keycodes`, redundantes Feld `RegisteredShortcut::name`).

## Nächste Schritte (Priorität)

1. **Phase 3b – GNOME Wayland Backend** (optional)
   - Über GNOME Shell Extension + D-Bus

2. **Phase 3c – wlroots Backend** (optional, für Sway/Hyprland)

3. **Cleanup:**
   - Echte Fehler-Ausgaben vs. Debug-Logs final trennen

4. **Packaging** (optional):
   - Flatpak-Manifest
   - `.desktop`-Datei für Anwendungsmenü
   - AppImage-Build

## Starten des Projekts

```bash
nix-shell
cargo run              # Normaler Start
cargo run -- --debug   # Mit Debug-Logs
```

## Lizenz / Weiteres

Das Repository ist auf GitHub hochgeladen und mit dem Coding Agent verlinkt. Alle Dateien sind produktiv einsatzbereit für X11 und XWayland-Umgebungen.
