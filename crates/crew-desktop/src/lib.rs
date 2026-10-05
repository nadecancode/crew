//! Native Linux application composition. Domain state and transport are supplied
//! by the shared client integration; this package's foundation owns only UI.

#[cfg(all(feature = "native-ui", target_os = "linux"))]
mod adaptive;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod appearance;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod application;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod navigation;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod shell;
#[cfg(all(feature = "native-ui", target_os = "linux"))]
pub mod views;
