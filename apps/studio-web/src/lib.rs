//! The studio's web entry: the page loads this module, which runs the same app as the native
//! window. Natively it is empty.

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    mmh3_studio::run();
}
