#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    crew_desktop::application::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("crew-desktop is the native Linux app; Apple apps use the Crew Xcode project");
    std::process::exit(2);
}
