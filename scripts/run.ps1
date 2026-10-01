$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
& {
    rustup target add wasm32-unknown-unknown
    cargo build --release -p world-viewer --target wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) { throw "Compilation WASM échouée" }
    $Lock = Get-Content Cargo.lock -Raw
    $Version = [regex]::Match($Lock, 'name = "wasm-bindgen"\r?\nversion = "([^"]+)"').Groups[1].Value
    cargo install wasm-bindgen-cli --version $Version --locked
    if ($LASTEXITCODE -ne 0) { throw "Installation wasm-bindgen échouée" }
    wasm-bindgen --target web --out-dir web/pkg --out-name world_viewer target/wasm32-unknown-unknown/release/world_viewer.wasm
    if ($LASTEXITCODE -ne 0) { throw "Assemblage WASM échoué" }
}
if (!(Test-Path "data/latest")) {
    cargo run --release -p world-app -- preview --seed 42 --resolution 128
    if ($LASTEXITCODE -ne 0) { throw "Génération échouée" }
}
$Port = if ($env:WORLD_PORT) { $env:WORLD_PORT } else { "8001" }
cargo run --release -p world-app -- serve --port $Port
