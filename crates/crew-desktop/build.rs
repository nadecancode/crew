use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=resources");
    if env::var_os("CARGO_FEATURE_NATIVE_UI").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
    {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides OUT_DIR"))
        .join("crew.gresource");
    let status = Command::new("glib-compile-resources")
        .args([
            "resources/crew.gresource.xml",
            "--sourcedir=resources",
            "--target",
        ])
        .arg(output)
        .status()
        .expect("native-ui requires glib-compile-resources in the GTK development environment");
    assert!(
        status.success(),
        "Crew's native resources failed to compile"
    );
}
