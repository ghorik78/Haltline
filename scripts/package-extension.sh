#!/usr/bin/env bash
# Builds the prebuilt Zed extension: dist/haltline-zed-extension.{tar.gz,zip}.
# The archive holds a `haltline/` folder with extension.toml, extension.wasm
# and the configuration schema, the same layout Zed uses for installed
# extensions.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --locked --target wasm32-wasip2 -p haltline-zed

api_version=$(sed -n 's/^zed_extension_api = "\(.*\)"/\1/p' Cargo.toml)
stage=dist/stage/haltline
rm -rf dist/stage
mkdir -p "$stage/debug_adapter_schemas"
cp target/wasm32-wasip2/release/haltline_zed.wasm "$stage/extension.wasm"
cp debug_adapter_schemas/Haltline.json "$stage/debug_adapter_schemas/"
cp LICENSE README.md README.ru.md "$stage/"
# Zed records the library kind and API version in installed manifests.
{
    cat extension.toml
    printf '\n[lib]\nkind = "Rust"\nversion = "%s"\n' "$api_version"
} > "$stage/extension.toml"

mkdir -p dist
rm -f dist/haltline-zed-extension.tar.gz dist/haltline-zed-extension.zip
tar -C dist/stage -czf dist/haltline-zed-extension.tar.gz haltline
if command -v zip >/dev/null; then
    (cd dist/stage && zip -qr ../haltline-zed-extension.zip haltline)
fi
rm -rf dist/stage
ls -l dist/haltline-zed-extension.*
