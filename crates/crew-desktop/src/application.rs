use std::{cell::RefCell, rc::Rc, sync::Once};

use gtk::{gio, glib, prelude::*};

use crate::shell::Shell;

pub const APPLICATION_ID: &str = "dev.crew.Crew";

pub fn register_resources() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let bytes =
            glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/crew.gresource")));
        let resource = gio::Resource::from_data(&bytes).expect("compiled Crew resource bundle");
        gio::resources_register(&resource);
    });
}

/// Build the real GTK application. Repeated activation presents its existing
/// window. Closing or quitting only ends this client process.
pub fn build() -> gtk::Application {
    glib::set_prgname(Some("crew-desktop"));
    glib::set_application_name("Crew");
    let app = gtk::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    let shell = Rc::new(RefCell::new(None::<Shell>));
    app.connect_startup(|_| register_resources());
    app.connect_activate(move |app| {
        if let Some(window) = app.windows().first() {
            window.present();
        } else {
            let window = Shell::new(app);
            window.window().present();
            *shell.borrow_mut() = Some(window);
        }
    });
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);
    app
}

pub fn run() -> glib::ExitCode {
    build().run()
}
