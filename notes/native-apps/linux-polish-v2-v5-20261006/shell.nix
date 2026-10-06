# Private capture session tools: GTK runtime, Xvfb, Openbox, AT-SPI, xdotool, ImageMagick.
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [
    gtk4 vte-gtk4 pkg-config gcc xorg-server xwininfo imagemagick dbus at-spi2-core
    openbox xdotool (python3.withPackages (p: [ p.pyatspi p.pygobject3 ]))
  ];
  shellHook = ''
    export CAPTURE_GI_TYPELIB_PATH="${pkgs.at-spi2-core}/lib/girepository-1.0:${pkgs.glib.out}/lib/girepository-1.0:${pkgs.gobject-introspection}/lib/girepository-1.0"
  '';
}
