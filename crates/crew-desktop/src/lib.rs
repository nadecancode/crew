//! Native Linux application composition. Domain state and transport are supplied
//! by the shared client integration; this package's foundation owns only UI.

#[cfg(all(feature = "native-ui", target_os = "linux"))]
mod adaptive;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod appearance;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod application;
#[cfg(all(feature = "native-ui", feature = "local", target_os = "linux"))]
mod availability;
#[cfg(all(feature = "native-ui", feature = "local", target_os = "linux"))]
pub mod controller;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod dialogs;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod navigation;
#[cfg(all(feature = "native-ui", feature = "gateway", target_os = "linux"))]
pub mod remote;
#[cfg(all(feature = "native-ui", feature = "local", target_os = "linux"))]
pub mod service;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod shell;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod terminal;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod views;
