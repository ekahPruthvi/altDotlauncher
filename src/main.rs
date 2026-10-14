use gtk4::gdk::Display;
use gtk4::gio::{self, AppInfo};
use gtk4_layer_shell::Edge::{Bottom, Top};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Button, CssProvider, FlowBox, Image, ScrolledWindow,
    SelectionMode,
};
use std::collections::HashSet;

fn extend_data_dirs() {
    let home = std::env::var("HOME").unwrap_or_default();
    let current = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());

    let mut dirs: Vec<String> = current
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let extras = [
        "/usr/local/share".to_string(),
        "/usr/share".to_string(),
        "/var/lib/flatpak/exports/share".to_string(),
        format!("{home}/.local/share/flatpak/exports/share"),
        "/var/lib/snapd/desktop".to_string(),
    ];

    for extra in extras {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }

    std::env::set_var("XDG_DATA_DIRS", dirs.join(":"));
}

fn load_apps() -> Vec<AppInfo> {
    let mut seen = HashSet::new();
    let mut apps: Vec<AppInfo> = AppInfo::all()
        .into_iter()
        .filter(|a| a.should_show())
        .filter(|a| match a.id() {
            Some(id) => seen.insert(id.to_string()),
            None => true,
        })
        .collect();

    apps.sort_by_key(|a| a.display_name().to_lowercase());
    apps
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("alt.")
        .default_width(800)
        .default_height(560)
        .resizable(false)
        .decorated(false)
        .build();

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("altDot"));
    window.set_anchor(Top, true);
    window.set_anchor(Bottom, true);
    window.set_anchor(Edge::Right, true);
    window.set_anchor(Edge::Left, true);

    let grid = FlowBox::builder()
        .selection_mode(SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(4)
        .max_children_per_line(5)
        .row_spacing(25)
        .column_spacing(25)
        .margin_top(30)
        .margin_bottom(30)
        .margin_start(20)
        .margin_end(20)
        .valign(gtk4::Align::Start)
        .build();

    for info in load_apps() {
        let image = match info.icon() {
            Some(icon) => Image::from_gicon(&icon),
            None => Image::from_icon_name("application-x-executable"),
        };
        image.set_pixel_size(56);

        let button = Button::builder().child(&image).build();
        button.add_css_class("flat");
        button.add_css_class("app-btn");
        button.set_halign(gtk4::Align::Center);
        button.set_tooltip_text(Some(&info.display_name()));

        let app_handle = app.clone();
        button.connect_clicked(move |_| {
            if let Err(e) = info.launch(&[], None::<&gio::AppLaunchContext>) {
                eprintln!("Failed to launch {}: {}", info.display_name(), e);
            }
            app_handle.quit();
        });

        grid.insert(&button, -1);
    }

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&grid)
        .build();
    window.set_child(Some(&scroller));

    // Esc closes
    let keys = gtk4::EventControllerKey::new();
    let app_handle = app.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            app_handle.quit();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);

    let provider = CssProvider::new();
    provider.load_from_data(
        r#"
        window {
            background-color: rgba(18, 18, 18, 0.62);
            border-radius: 35px;
            border: 1px solid rgba(255, 255, 255, 0.1);
        }
        .app-btn {
            all: unset;
            padding: 12px;
            border-radius: 20px;
            border: none;
        }
        .app-btn:hover {
            background-color: rgba(124, 124, 124, 0.25);
        }
        "#,
    );
    gtk4::style_context_add_provider_for_display(
        &Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    window.present();
}

fn main() -> glib::ExitCode {
    extend_data_dirs();

    let app = Application::builder()
        .application_id("ekah.scu.alt")
        .build();
    app.connect_activate(build_ui);
    app.run()
}