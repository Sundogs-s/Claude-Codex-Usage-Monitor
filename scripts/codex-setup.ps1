Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Write-Host "[codex-setup] Checking Rust toolchain..."

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup was not found in PATH. Install Rust via https://rustup.rs/ first."
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found in PATH. Ensure Rust toolchain is correctly installed."
}

rustup --version
cargo --version

Write-Host "[codex-setup] Fetching dependencies..."
cargo fetch

Write-Host "[codex-setup] Prebuilding workspace (release)..."
cargo build --release

Write-Host "[codex-setup] Environment is ready."
