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
    #[cfg(feature = "local")]
    let controller = Rc::new(RefCell::new(None::<Rc<crate::controller::Controller>>));
    app.connect_startup(|_| register_resources());
    app.connect_activate(move |app| {
        if let Some(window) = app.windows().first() {
            window.present();
        } else {
            let window = Shell::new(app);
            #[cfg(feature = "local")]
            {
                *controller.borrow_mut() =
                    Some(crate::controller::Controller::start(window.clone()));
            }
            let shell_owner = Rc::downgrade(&shell);
            #[cfg(feature = "local")]
            let client_owner = Rc::downgrade(&controller);
            window.window().connect_close_request(move |_| {
                if let Some(owner) = shell_owner.upgrade() {
                    owner.borrow_mut().take();
                }
                #[cfg(feature = "local")]
                if let Some(owner) = client_owner.upgrade() {
                    owner.borrow_mut().take();
                }
                glib::Propagation::Proceed
            });
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
