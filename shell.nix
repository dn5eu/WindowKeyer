{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "windowkeyer-dev";

  nativeBuildInputs = with pkgs; [
    pkg-config
    gcc
    gobject-introspection
  ];

  buildInputs = with pkgs; [
    # --- Rust Toolchain ---
    rustc
    cargo
    rustfmt
    clippy

    # --- GUI & Desktop Bibliotheken ---
    gtk4
    libadwaita
    glib
    pango
    cairo
    gdk-pixbuf
    graphene

    # --- Wayland ---
    wayland
    libxkbcommon

    # --- X11 ---
    libxcb
    libX11
    libXrandr
    libXi

    # --- D-Bus (WICHTIG für ksni Tray Icon) ---
    dbus

    # --- Testwerkzeuge ---
    xorg.xeyes
    xorg.xprop
  ];

  shellHook = ''
    export GSETTINGS_SCHEMA_DIR="${pkgs.glib}/share/gsettings-schemas/${pkgs.glib.name}/glib-2.0/schemas:${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}/glib-2.0/schemas:${pkgs.libadwaita}/share/gsettings-schemas/${pkgs.libadwaita.name}/glib-2.0/schemas"
    export GDK_PIXBUF_MODULE_FILE="$(echo ${pkgs.gdk-pixbuf}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache)"
    export GDK_PIXBUF_MODULEDIR="${pkgs.gdk-pixbuf}/lib/gdk-pixbuf-2.0/2.10.0/loaders"
  '';
}
