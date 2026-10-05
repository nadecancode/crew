//! Native settings and app-owned surfaces. High-contrast settings retain the
//! toolkit palette; application CSS never removes the native focus indicator.

use std::{cell::Cell, rc::Rc};

use gtk::{glib, prelude::*};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

pub struct AppearanceController {
    display: gtk::gdk::Display,
    settings: gtk::Settings,
    base: gtk::CssProvider,
    palette: gtk::CssProvider,
    choice: Cell<Appearance>,
    system_prefer_dark: Cell<bool>,
    applying: Cell<bool>,
}

impl AppearanceController {
    pub fn new(display: &gtk::gdk::Display) -> Rc<Self> {
        let settings = gtk::Settings::for_display(display);
        let base = gtk::CssProvider::new();
        base.load_from_resource("/dev/crew/Crew/style.css");
        gtk::style_context_add_provider_for_display(
            display,
            &base,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let palette = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            display,
            &palette,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let controller = Rc::new(Self {
            display: display.clone(),
            base,
            system_prefer_dark: Cell::new(settings.is_gtk_application_prefer_dark_theme()),
            settings,
            palette,
            choice: Cell::new(Appearance::System),
            applying: Cell::new(false),
        });
        for property in [
            "gtk-theme-name",
            "gtk-application-prefer-dark-theme",
            "gtk-interface-color-scheme",
            "gtk-interface-contrast",
        ] {
            if controller.settings.find_property(property).is_none() {
                continue;
            }
            let weak = Rc::downgrade(&controller);
            controller
                .settings
                .connect_notify_local(Some(property), move |settings, pspec| {
                    if let Some(controller) = weak.upgrade() {
                        if controller.applying.get() {
                            return;
                        }
                        if pspec.name() == "gtk-application-prefer-dark-theme" {
                            controller
                                .system_prefer_dark
                                .set(settings.is_gtk_application_prefer_dark_theme());
                        }
                        controller.refresh();
                    }
                });
        }
        controller.refresh();
        controller
    }

    pub fn choice(&self) -> Appearance {
        self.choice.get()
    }

    pub fn set(&self, choice: Appearance) {
        self.choice.set(choice);
        self.refresh();
    }

    fn enum_setting(&self, property: &str) -> Option<String> {
        self.settings.find_property(property)?;
        let value = self.settings.property_value(property);
        glib::EnumValue::from_value(&value).map(|(_, value)| value.nick().to_owned())
    }

    fn refresh(&self) {
        let theme = self
            .settings
            .gtk_theme_name()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let high_contrast = theme.contains("highcontrast")
            || self.enum_setting("gtk-interface-contrast").as_deref() == Some("more");
        let system_dark = match self.enum_setting("gtk-interface-color-scheme").as_deref() {
            Some("dark") => true,
            Some("light") => false,
            _ => self.system_prefer_dark.get() || theme.ends_with("-dark"),
        };
        let dark = match self.choice.get() {
            Appearance::System => system_dark,
            Appearance::Dark => true,
            Appearance::Light => false,
        };
        self.applying.set(true);
        self.settings.set_gtk_application_prefer_dark_theme(dark);
        self.applying.set(false);
        if high_contrast {
            // Empty provider yields all colors to the platform's contrast theme.
            self.palette.load_from_data("");
        } else {
            self.palette.load_from_resource(if dark {
                "/dev/crew/Crew/dark.css"
            } else {
                "/dev/crew/Crew/light.css"
            });
        }
    }
}

impl Drop for AppearanceController {
    fn drop(&mut self) {
        gtk::style_context_remove_provider_for_display(&self.display, &self.base);
        gtk::style_context_remove_provider_for_display(&self.display, &self.palette);
    }
}
