//! Dependency and binding-generation probe; not a Crew application.
//! The GTK/VTE functions are type-checked, not launched.

#[cfg(not(target_arch = "wasm32"))]
uniffi::setup_scaffolding!("crew_platform_probe");

#[cfg_attr(not(target_arch = "wasm32"), uniffi::export)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen)]
pub fn protocol_fixture() -> u32 {
    6
}

#[cfg(all(target_os = "linux", feature = "gtk-smoke"))]
pub fn native_label() -> gtk::Label {
    gtk::Label::new(Some("Crew platform feasibility"))
}

#[cfg(all(target_os = "linux", feature = "terminal-smoke"))]
pub fn native_terminal() -> vte4::Terminal {
    vte4::Terminal::new()
}
