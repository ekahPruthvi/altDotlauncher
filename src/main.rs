use async_channel::Sender;
use gtk4::gdk::Display;
use gtk4::gio::{self, AppInfo};
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Button, CssProvider, FlowBox, Image, ScrolledWindow,
    SelectionMode,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL_SECS: u64 = 5;
const SAVE_EVERY_SECS: u64 = 60;
const MAX_SESSIONS_PER_APP: usize = 1000;

#[derive(Serialize, Deserialize, Clone, Default)]
struct Session {
    start: u64, 
    end: u64, 
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Usage {
    total_secs: u64,
    sessions: Vec<Session>,
    #[serde(skip)]
    open_since: Option<u64>,
}

type UsageMap = Arc<Mutex<HashMap<String, Usage>>>;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn usage_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("alt").join("appMon.json")
}

fn load_usage() -> HashMap<String, Usage> {
    std::fs::read_to_string(usage_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_usage(map: &HashMap<String, Usage>) {
    let path = usage_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(map) {
        let _ = std::fs::write(path, json);
    }
}

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

fn build_resolver(apps: &[AppInfo]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for a in apps {
        let Some(id) = a.id().map(|s| s.to_string()) else {
            continue;
        };
        let base = id.trim_end_matches(".desktop").to_lowercase();
        map.entry(base).or_insert_with(|| id.clone());

        let kf = glib::KeyFile::new();
        if kf
            .load_from_data_dirs(format!("applications/{id}"), glib::KeyFileFlags::NONE)
            .is_ok()
        {
            if let Ok(wm) = kf.string("Desktop Entry", "StartupWMClass") {
                map.entry(wm.to_lowercase()).or_insert_with(|| id.clone());
            }
        }

        if let Some(exe) = a
            .commandline()
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase())
        {
            map.entry(exe).or_insert_with(|| id.clone());
        }
    }
    map
}


fn niri_open_app_ids() -> Option<HashSet<String>> {
    let out = Command::new("niri")
        .args(["msg", "--json", "windows"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let set = v
        .as_array()?
        .iter()
        .filter_map(|w| w.get("app_id")?.as_str().map(|s| s.to_lowercase()))
        .collect();
    Some(set)
}

fn spawn_tracker(resolver: HashMap<String, String>, usage: UsageMap) {
    std::thread::spawn(move || {
        let mut last_poll = now();
        let mut last_save = now();

        loop {
            std::thread::sleep(Duration::from_secs(POLL_SECS));
            let t = now();
            let dt = t.saturating_sub(last_poll);
            last_poll = t;

            let Some(open_ids) = niri_open_app_ids() else {
                continue;
            };

            let running: HashSet<&String> = open_ids
                .iter()
                .filter_map(|app_id| resolver.get(app_id))
                .collect();

            let mut map = usage.lock().unwrap();

            for (id, u) in map.iter_mut() {
                if running.contains(id) {
                    if u.open_since.is_none() {
                        u.open_since = Some(t);
                    }
                    u.total_secs += dt;
                } else if let Some(start) = u.open_since.take() {
                    u.sessions.push(Session { start, end: t });
                    if u.sessions.len() > MAX_SESSIONS_PER_APP {
                        let excess = u.sessions.len() - MAX_SESSIONS_PER_APP;
                        u.sessions.drain(0..excess);
                    }
                }
            }

            if t.saturating_sub(last_save) >= SAVE_EVERY_SECS {
                save_usage(&map);
                last_save = t;
            }
        }
    });
}

fn spawn_super_watchers(tx: Sender<()>) {
    use evdev::{EventType, KeyCode};

    let left = KeyCode::KEY_LEFTMETA.code();
    let right = KeyCode::KEY_RIGHTMETA.code();

    for (_path, mut dev) in evdev::enumerate() {
        let has_super = dev
            .supported_keys()
            .map_or(false, |k| k.contains(KeyCode::KEY_LEFTMETA));
        if !has_super {
            continue;
        }

        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut meta_down = false;
            let mut clean = false;

            loop {
                let events = match dev.fetch_events() {
                    Ok(e) => e,
                    Err(_) => break,
                };
                for ev in events {
                    if ev.event_type() != EventType::KEY {
                        continue;
                    }
                    let (code, value) = (ev.code(), ev.value());

                    if code == left || code == right {
                        match value {
                            1 => {
                                meta_down = true;
                                clean = true;
                            }
                            0 => {
                                if meta_down && clean {
                                    let _ = tx.send_blocking(());
                                }
                                meta_down = false;
                            }
                            _ => {}
                        }
                    } else if value == 1 && meta_down {
                        clean = false;
                    }
                }
            }
        });
    }
}

fn populate(grid: &FlowBox, window: &ApplicationWindow) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }

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

        let win = window.clone();
        button.connect_clicked(move |_| {
            if let Err(e) = info.launch(&[], None::<&gio::AppLaunchContext>) {
                eprintln!("Failed to launch {}: {}", info.display_name(), e);
            }
            win.set_visible(false);
        });

        grid.insert(&button, -1);
    }
}

fn build_window(app: &Application) -> (ApplicationWindow, FlowBox) {
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
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, 130);

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

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&grid)
        .build();
    window.set_child(Some(&scroller));

    let keys = gtk4::EventControllerKey::new();
    let win = window.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            win.set_visible(false);
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

    (window, grid)
}


fn main() -> glib::ExitCode {
    extend_data_dirs();

    let app = Application::builder()
        .application_id("ekah.scu.alt")
        .build();

    app.connect_startup(|app| {
        std::mem::forget(app.hold());

        let apps = load_apps();
        let resolver = build_resolver(&apps);

        let mut stats = load_usage();
        for a in &apps {
            if let Some(id) = a.id() {
                stats.entry(id.to_string()).or_default();
            }
        }
        let usage: UsageMap = Arc::new(Mutex::new(stats));
        spawn_tracker(resolver, usage);

        let (window, grid) = build_window(app);

        let toggle = {
            let window = window.clone();
            move || {
                if window.is_visible() {
                    window.set_visible(false);
                } else {
                    populate(&grid, &window);
                    window.present();
                }
            }
        };

        let (tx, rx) = async_channel::unbounded::<()>();
        spawn_super_watchers(tx);
        let toggle_for_keys = toggle.clone();
        glib::spawn_future_local(async move {
            while rx.recv().await.is_ok() {
                toggle_for_keys();
            }
        });

        let action = gio::SimpleAction::new("toggle", None);
        action.connect_activate(move |_, _| toggle());
        app.add_action(&action);
    });

    let first = Rc::new(Cell::new(true));
    app.connect_activate(move |app| {
        if first.replace(false) {
            return;
        }
        app.activate_action("toggle", None);
    });

    app.run()
}