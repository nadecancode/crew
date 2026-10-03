{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  packages = with pkgs; [ rustup gcc pkg-config just git bubblewrap socat ];
}
