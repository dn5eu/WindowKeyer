//! Eigene Tray-/App-Icons aus Dateien im Projekt-/Nutzerverzeichnis laden.
//!
//! ksni meldet das Tray-Icon entweder als themenbasierten `icon_name`
//! (freedesktop Icon Theme) oder als Pixelpuffer über `pixmap_icon()`.
//! Um ein eigenes PNG/SVG zu verwenden, lesen wir die Datei hier ein und
//! konvertieren sie in einen ARGB32-Puffer, den ksni via D-Bus an das
//! StatusNotifier-Host (z. B. KDE Plasma) übergibt.

use std::path::PathBuf;

/// Mögliche Verzeichnisse, in denen `windowkeyer.png` / `windowkeyer.svg`
/// gefunden werden — der Reihe nach durchsucht.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Explizit per Umgebungsvariablen gesetzt (empfohlener Weg):
    //        WINDOWKEYER_ICON=/pfad/zum/icon.png cargo run
    if let Ok(p) = std::env::var("WINDOWKEYER_ICON") {
        let path = PathBuf::from(p);
        if let Some(parent) = path.parent() {
            dirs.push(parent.to_path_buf());
        }
    }

    // 2. Aktuelles Arbeitsverzeichnis (praktisch bei `cargo run`)
    dirs.push(PathBuf::from("."));

    // 3. Projektverzeichnis ./assets und ./icons
    dirs.push(PathBuf::from("assets"));
    dirs.push(PathBuf::from("icons"));

    // 4. Nutzerdatenbank: ~/.local/share/icons/hicolor/scalable/apps/
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join("icons/hicolor/scalable/apps"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            PathBuf::from(home)
                .join(".local/share/icons/hicolor/scalable/apps"),
        );
    }

    dirs
}

/// Dateinamen, unter denen das eigene Icon gesucht wird.
const ICON_FILE_NAMES: &[&str] = &["windowkeyer.png", "windowkeyer.svg"];

/// Findet die erste existierende Icon-Datei in den Kandidatenverzeichnissen.
pub fn find_custom_icon_file() -> Option<PathBuf> {
    for dir in candidate_dirs() {
        for name in ICON_FILE_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Lädt das eigene Icon und gibt es als ARGB32-Pixelpuffer zurück
/// (Format wie von gdk_pixbuf::Pixbuf::get_pixels(), vorliegt: 8-bit,
/// non-premultiplied, byte-Reihenfolge R-G-B-A bzw. auf Big-Endian A-R-G-B).
///
/// Rückgabe: (width, height, argb_bytes)
pub fn load_custom_icon_pixmap() -> Option<(i32, i32, Vec<u8>)> {
    let path = find_custom_icon_file()?;
    crate::debugln!("[windowkeyer] Tray-Icon geladen aus: {}", path.display());

    // SVG rendert gdk-pixbuf nur mit dem librsvg-Loader; schlägt das fehl,
    // bekommen wir einen Fehler und fallen im Tray auf den Themennamen zurück.
    let pixbuf = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, 64, 64, true).ok()?;

    // Sicherstellen, dass der Puffer 8-bit pro Kanal ist (RGB oder RGBA —
    // 3-Kanal-Pixbufs werden unten beim Konvertieren mit Alpha 255 gefüllt).
    // Bei anderer Bittiefe (z. B. 16-bit PNG) über eine neue Pixbuf-Kopie
    // normalisieren.
    let pixbuf = if pixbuf.bits_per_sample() == 8 && matches!(pixbuf.n_channels(), 3 | 4) {
        pixbuf
    } else {
        // RGB/RGBA mit anderer Bittiefe: erst zu 8-bit skalieren/konvertieren.
        gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(&path, 64, 64, true).ok()?
    };

    let w = pixbuf.width();
    let h = pixbuf.height();
    let rowstride = pixbuf.rowstride() as usize;
    let n_channels = pixbuf.n_channels() as usize;
    // pixels() ist unsafe, weil die Referenz nur gueltig ist, solange das
    // Pixbuf lebt und niemand die Daten mutiert. Beides ist hier erfuetlt:
    // pixbuf bleibt bis zum Ende der Funktion in Scope, und wir lesen nur.
    let raw = unsafe { pixbuf.pixels() }; // &[u8], Laenge >= rowstride * h

    // StatusNotifier-Protokoll: "network byte order" = Big-Endian, d. h. die
    // Bytes pro Pixel stehen in der Reihenfolge A-R-G-B im Puffer.
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let src = y * rowstride + x * n_channels;
            let r = raw[src];
            let g = raw[src + 1];
            let b = raw[src + 2];
            let a = if n_channels == 4 { raw[src + 3] } else { 255 };
            let dst = (y * w as usize + x) * 4;
            out[dst] = a;
            out[dst + 1] = r;
            out[dst + 2] = g;
            out[dst + 3] = b;
        }
    }

    Some((w, h, out))
}
