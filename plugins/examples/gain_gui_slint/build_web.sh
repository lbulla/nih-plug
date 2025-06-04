#!/bin/sh

set -ex

RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals --cfg web_sys_unstable_apis' \
  cargo +nightly build --package gain_gui_slint --lib --target wasm32-unknown-unknown --release -Z build-std=std,panic_abort

wasm-bindgen ../../../target/wasm32-unknown-unknown/release/gain_gui_slint.wasm --out-dir ./web/pkg/ --target web
wasm-opt ./web/pkg/gain_gui_slint_bg.wasm -o ./web/pkg/gain_gui_slint_bg.wasm -O4 --all-features
