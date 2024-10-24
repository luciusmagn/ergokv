{
  description = "ergokv development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustVersion = pkgs.rust-bin.nightly.latest.default;
      in
      {
        devShell = pkgs.mkShell {
          buildInputs = with pkgs; [
            (rustVersion.override { extensions = [ "rust-src" ]; })
            rust-analyzer
            protobuf
            grpc
            pkg-config
            openssl
            tidb # For TiKV
            gcc
            libcxx
          ];

          PROTOC = "${pkgs.protobuf}/bin/protoc";
          PROTOC_INCLUDE = "${pkgs.protobuf}/include";

          shellHook = ''
            echo "ergokv development environment"
            echo "Rust, Protobuf, and gRPC are available"
          '';
        };
      }
    );
}
