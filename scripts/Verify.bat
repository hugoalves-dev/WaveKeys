@echo off
set MODE=%1
cargo fmt --all -- --check || exit /b 1
cargo clippy --workspace --all-targets -- -D warnings || exit /b 1
cargo test --workspace || exit /b 1
if /I "%MODE%"=="Full" cargo build --workspace --release --target-dir build\cargo
