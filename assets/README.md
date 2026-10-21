# Tray-Icon

Lege hier dein eigenes Icon als `windowkeyer.png` (empfohlen: quadratisch,
mind. 64x64, transparenter Hintergrund) oder `windowkeyer.svg` ab.
Das Programm sucht der Reihe nach:

1. Verzeichnis der per `WINDOWKEYER_ICON=/pfad/zum/icon.png` gesetzten Datei
2. aktuelles Arbeitsverzeichnis (`./`)
3. `./assets/` und `./icons/`
4. `~/.local/share/icons/hicolor/scalable/apps/`

Wird keine Datei gefunden, nutzt das Tray-Icon den Theme-Namen
`preferences-system-windows` (bzw. einen frei konfigurierbaren Namen, s. u.).

Das enthaltene `windowkeyer.png` ist nur ein Platzhalter — einfach durch
dein eigenes Icon mit demselben Dateinamen ersetzen.
