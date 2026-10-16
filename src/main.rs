use async_channel::Sender;
use gtk4::gdk::Display;
use gtk4::gio::{self, AppInfo};
use gtk4::prelude::*;
use gtk4::{
    glib, pango, Application, ApplicationWindow, Button, CssProvider, Entry as GtkEntry, FlowBox, IconTheme, Image,
    Label, Overlay, Picture, ScrolledWindow, SelectionMode, Stack, StackTransitionType, Box as GtkBox,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use infoprober::{Document, Entry};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::process::exit;
use self_update::cargo_crate_version;

const POLL_SECS: u64 = 5;
const SAVE_EVERY_SECS: u64 = 60;
const SUGGEST_COUNT: usize = 5;
const SEARCH_RESULTS: usize = 4;
const LOGO_TEXT: &str = "alt●";
const SEARCH_GLYPH: &str = "";
const LAUNCH_ANIM_US: i64 = 300_000;

const PANEL_HEIGHT: i32 = 320;
const PAGE_MAX: usize = 8;
const FILE_DEPTH: usize = 5;
const FILE_SCAN_LIMIT: usize = 40_000;
const FILE_MIN_QUERY: usize = 2;
const CLIP_KEEP: usize = 50;
const CLIP_MAX_BYTES: usize = 10_000;
const FILE_SKIP_DIRS: [&str; 5] = ["node_modules", "target", "venv", "__pycache__", "site-packages"];

/// terminals offered for Terminal=true apps: (binary, args placed before the app's command)
const TERMINALS: [(&str, &[&str]); 2] = [("kitty", &[]), ("xterm", &["-e"])];

/// (label, icon, shell command) -- edit this list to add your own pills
const PILLS: [(&str, &str, &str); 5] = [
    ("Lock", "system-lock-screen", "loginctl lock-session"),
    ("Suspend", "system-suspend", "systemctl suspend"),
    ("Log out", "system-log-out", "niri msg action quit --skip-confirmation"),
    ("Restart", "system-reboot", "systemctl reboot"),
    ("Power off", "system-shutdown", "systemctl poweroff"),
];

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

struct Usage {
    slots: Vec<HashMap<String, u32>>,
    alive: HashMap<String, usize>,
    dirty: bool,
}

impl Usage {
    fn new() -> Self {
        Self {
            slots: vec![HashMap::new(); 7 * 24],
            alive: HashMap::new(),
            dirty: false,
        }
    }
}

type UsageMap = Arc<Mutex<Usage>>;

fn current_slot() -> Option<usize> {
    let dt = glib::DateTime::now_local().ok()?;
    let day = (dt.day_of_week() - 1).clamp(0, 6) as usize;
    let hour = dt.hour().clamp(0, 23) as usize;
    Some(day * 24 + hour)
}

///   echo "firefox" > "$XDG_RUNTIME_DIR/altDot.search" && pkill -USR2 altDot
fn search_request_path() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("altDot.search")
}

static SIG_TOGGLE: AtomicBool = AtomicBool::new(false);
static SIG_SEARCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_: libc::c_int) {
    SIG_TOGGLE.store(true, Ordering::SeqCst);
}

extern "C" fn on_sigusr2(_: libc::c_int) {
    SIG_SEARCH.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGUSR1, on_sigusr1 as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, on_sigusr2 as *const () as libc::sighandler_t);
    }
}

fn take_search_request() -> Option<String> {
    let path = search_request_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let q = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.is_empty() {
        None
    } else {
        Some(q)
    }
}

fn usage_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        });
    base.join("alt").join("appMon.probe")
}

fn load_usage() -> Usage {
    let mut usage = Usage::new();

    let Ok(src) = std::fs::read_to_string(usage_path()) else {
        return usage;
    };
    let doc = match infoprober::parse(&src) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("appMon.probe: {e}");
            return usage;
        }
    };

    for (d, name) in DAYS.iter().enumerate() {
        let Some(section) = doc.section(name) else {
            continue;
        };
        for hour_entry in &section.entries {
            let Some(hour) = hour_entry.key.parse::<usize>().ok() else {
                continue;
            };
            if !(1..=24).contains(&hour) {
                continue;
            }
            let Some(apps) = hour_entry.value.as_map() else {
                continue;
            };
            let slot = &mut usage.slots[d * 24 + (hour - 1)];
            for app in apps {
                if let Some(n) = app.value.parse::<u32>() {
                    slot.insert(app.key.to_string(), n);
                }
            }
        }
    }
    usage
}

fn save_usage(usage: &Usage) {
    let path = usage_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = match infoprober::parse(&existing) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("appMon.probe is malformed ({e}); backing it up to .bak");
            let _ = std::fs::copy(&path, path.with_extension("probe.bak"));
            Document::new()
        }
    };

    for (d, name) in DAYS.iter().enumerate() {
        let section = doc.section_or_insert(*name);
        section.entries.clear();

        for h in 0..24 {
            let mut apps: Vec<(&String, &u32)> = usage.slots[d * 24 + h].iter().collect();
            apps.sort();
            let entries: Vec<Entry> = apps
                .into_iter()
                .map(|(k, v)| Entry::new(k.clone(), v.to_string()))
                .collect();
            section.push((h + 1).to_string(), entries);
        }
    }
    doc.section_or_insert("appfuzz");

    if let Err(e) = doc.validate() {
        eprintln!("appMon.probe not saved: {e}");
        return;
    }

    let tmp = path.with_extension("probe.tmp");
    if std::fs::write(&tmp, doc.render()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
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
        let mut last_save = std::time::Instant::now();

        loop {
            std::thread::sleep(Duration::from_secs(POLL_SECS));

            let Some(open_ids) = niri_open_app_ids() else {
                continue;
            };
            let Some(slot) = current_slot() else {
                continue;
            };

            let running: HashSet<&String> = open_ids
                .iter()
                .filter_map(|app_id| resolver.get(app_id))
                .collect();

            let mut guard = usage.lock().unwrap();
            let u = &mut *guard;

            u.alive.retain(|id, _| running.contains(id));

            for id in running {
                if u.alive.get(id).copied() != Some(slot) {
                    *u.slots[slot].entry(id.clone()).or_insert(0) += 1;
                    u.alive.insert(id.clone(), slot);
                    u.dirty = true;
                }
            }

            if u.dirty && last_save.elapsed().as_secs() >= SAVE_EVERY_SECS {
                save_usage(u);
                u.dirty = false;
                last_save = std::time::Instant::now();
            }
        }
    });
}

struct SuperState {
    down: AtomicBool,
    clean: AtomicBool,
}

fn spawn_super_watchers(tx: Sender<()>) {
    use evdev::{EventType, KeyCode, RelativeAxisCode};

    let left = KeyCode::KEY_LEFTMETA.code();
    let right = KeyCode::KEY_RIGHTMETA.code();

    let touch = KeyCode::BTN_TOUCH.0;
    let finger = KeyCode::BTN_TOOL_FINGER.0;
    let wheel = [
        RelativeAxisCode::REL_WHEEL.0,
        RelativeAxisCode::REL_HWHEEL.0,
        RelativeAxisCode::REL_WHEEL_HI_RES.0,
        RelativeAxisCode::REL_HWHEEL_HI_RES.0,
    ];

    let state = Arc::new(SuperState {
        down: AtomicBool::new(false),
        clean: AtomicBool::new(false),
    });

    for (_path, mut dev) in evdev::enumerate() {
        let is_keyboard = dev
            .supported_keys()
            .map_or(false, |k| k.contains(KeyCode::KEY_LEFTMETA));
        let is_pointer = !is_keyboard
            && (dev
                .supported_keys()
                .map_or(false, |k| k.contains(KeyCode::BTN_LEFT))
                || dev
                    .supported_relative_axes()
                    .map_or(false, |r| r.contains(RelativeAxisCode::REL_WHEEL)));

        if is_keyboard {
            let tx = tx.clone();
            let st = state.clone();
            std::thread::spawn(move || loop {
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
                                st.down.store(true, Ordering::SeqCst);
                                st.clean.store(true, Ordering::SeqCst);
                            }
                            0 => {
                                if st.down.load(Ordering::SeqCst) && st.clean.load(Ordering::SeqCst)
                                {
                                    let _ = tx.send_blocking(());
                                }
                                st.down.store(false, Ordering::SeqCst);
                            }
                            _ => {}
                        }
                    } else if value == 1 && st.down.load(Ordering::SeqCst) {
                        st.clean.store(false, Ordering::SeqCst);
                    }
                }
            });
        } else if is_pointer {
            let st = state.clone();
            std::thread::spawn(move || loop {
                let events = match dev.fetch_events() {
                    Ok(e) => e,
                    Err(_) => break,
                };
                for ev in events {
                    if !st.down.load(Ordering::SeqCst) {
                        continue;
                    }
                    let (code, value) = (ev.code(), ev.value());
                    let taints = match ev.event_type() {
                        EventType::KEY => value == 1 && code != touch && code != finger,
                        EventType::RELATIVE => wheel.contains(&code),
                        _ => false,
                    };
                    if taints {
                        st.clean.store(false, Ordering::SeqCst);
                    }
                }
            });
        }
    }
}

// ───────────────────────── UI ─────────────────────────

fn make_app_button(info: AppInfo, window: &ApplicationWindow) -> Button {
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
    button.connect_clicked(move |b| launch_animated(&info, b, &win));
    button
}

fn spawn_detached(cmd: &mut Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn in_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|d| {
        std::fs::metadata(d.join(bin))
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

fn installed_terminals() -> Vec<&'static str> {
    TERMINALS.iter().map(|(n, _)| *n).filter(|n| in_path(n)).collect()
}

/// the app's .desktop file, loaded the same way build_resolver does (no DesktopAppInfo needed)
fn desktop_keyfile(info: &AppInfo) -> Option<glib::KeyFile> {
    let id = info.id()?;
    let kf = glib::KeyFile::new();
    kf.load_from_data_dirs(format!("applications/{id}"), glib::KeyFileFlags::NONE)
        .ok()?;
    Some(kf)
}

fn is_terminal_app(info: &AppInfo) -> bool {
    desktop_keyfile(info)
        .and_then(|kf| kf.boolean("Desktop Entry", "Terminal").ok())
        .unwrap_or(false)
}

/// minimal shell-style splitter for Exec= lines (quotes + backslashes)
fn split_exec(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut quote: Option<char> = None;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = it.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    in_tok = true;
                }
                '\\' => {
                    if let Some(n) = it.next() {
                        cur.push(n);
                        in_tok = true;
                    }
                }
                c if c.is_whitespace() => {
                    if in_tok {
                        out.push(std::mem::take(&mut cur));
                        in_tok = false;
                    }
                }
                _ => {
                    cur.push(c);
                    in_tok = true;
                }
            },
        }
    }
    if in_tok {
        out.push(cur);
    }
    out
}

fn terminal_app_argv(info: &AppInfo) -> Option<Vec<String>> {
    let kf = desktop_keyfile(info)?;
    let exec = kf.string("Desktop Entry", "Exec").ok()?.to_string();
    let argv: Vec<String> = split_exec(&exec)
        .into_iter()
        .filter(|a| !(a.len() == 2 && a.starts_with('%') && !a.starts_with("%%")))
        .map(|a| a.replace("%%", "%"))
        .collect();
    Some(argv)
}

fn run_app(info: &AppInfo, term: Option<&'static str>) -> Result<(), String> {
    match term {
        Some(t) => {
            let argv = terminal_app_argv(info).ok_or_else(|| "no Exec line".to_string())?;
            if argv.is_empty() {
                return Err("empty Exec line".to_string());
            }
            let flags: &[&str] = TERMINALS
                .iter()
                .find(|(n, _)| *n == t)
                .map(|(_, f)| *f)
                .unwrap_or(&[]);
            let mut cmd = Command::new(t);
            cmd.args(flags).args(&argv);
            spawn_detached(&mut cmd).map_err(|e| e.to_string())
        }
        None => info
            .launch(&[], None::<&gio::AppLaunchContext>)
            .map_err(|e| e.to_string()),
    }
}

fn picker_widget(window: &ApplicationWindow) -> Option<gtk4::Widget> {
    let overlay = window.child().and_downcast::<Overlay>()?;
    let mut c = overlay.first_child();
    while let Some(w) = c {
        if w.has_css_class("term-pick") {
            return Some(w);
        }
        c = w.next_sibling();
    }
    None
}

fn close_picker(window: &ApplicationWindow) -> bool {
    let Some(p) = picker_widget(window) else {
        return false;
    };
    if let Some(o) = window.child().and_downcast::<Overlay>() {
        o.remove_overlay(&p);
    }
    true
}

fn collect_buttons(w: &gtk4::Widget, out: &mut Vec<Button>) {
    if let Ok(b) = w.clone().downcast::<Button>() {
        out.push(b);
        return;
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        collect_buttons(&ch, out);
        c = ch.next_sibling();
    }
}

fn ask_terminal(info: &AppInfo, source: &Button, window: &ApplicationWindow, terms: Vec<&'static str>) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        start_launch(info, source, window, terms.first().copied());
        return;
    };
    if picker_widget(window).is_some() {
        return;
    }

    let picker = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .css_classes(["term-pick"])
        .build();

    let title = Label::new(Some(&format!("Open {} with", info.display_name())));
    title.add_css_class("term-pick-title");
    picker.append(&title);

    let row = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .halign(gtk4::Align::Center)
        .build();

    let mut first: Option<Button> = None;
    for (i, t) in terms.iter().enumerate() {
        let b = Button::with_label(&format!("{}  {}", i + 1, t));
        b.add_css_class("term-btn");
        let (info, source, win, overlay, weak, t) = (
            info.clone(),
            source.clone(),
            window.clone(),
            overlay.clone(),
            picker.downgrade(),
            *t,
        );
        b.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                overlay.remove_overlay(&p);
            }
            start_launch(&info, &source, &win, Some(t));
        });
        row.append(&b);
        if first.is_none() {
            first = Some(b);
        }
    }
    picker.append(&row);

    let hint = Label::new(Some("Esc to cancel"));
    hint.add_css_class("sugg-empty-sub");
    picker.append(&hint);

    overlay.add_overlay(&picker);
    if let Some(b) = first {
        b.grab_focus();
    }
}

fn launch_animated(info: &AppInfo, source: &Button, window: &ApplicationWindow) {
    if is_terminal_app(info) {
        let terms = installed_terminals();
        match terms.len() {
            0 => {}
            1 => {
                start_launch(info, source, window, Some(terms[0]));
                return;
            }
            _ => {
                ask_terminal(info, source, window, terms);
                return;
            }
        }
    }
    start_launch(info, source, window, None);
}

fn reset_launch(window: &ApplicationWindow) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        return;
    };
    let mut ghosts = Vec::new();
    let mut c = overlay.first_child();
    while let Some(w) = c {
        c = w.next_sibling();
        if w.has_css_class("launch-ghost") || w.has_css_class("term-pick") {
            ghosts.push(w);
        }
    }
    for g in ghosts {
        overlay.remove_overlay(&g);
    }
    overlay.remove_css_class("launching");
    if let Some(card) = overlay.child() {
        card.set_opacity(1.0);
    }
}

fn start_launch(info: &AppInfo, source: &Button, window: &ApplicationWindow, term: Option<&'static str>) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        let _ = run_app(info, term);
        window.set_visible(false);
        return;
    };
    if overlay.has_css_class("launching") {
        return;
    }

    if let Err(e) = run_app(info, term) {
        eprintln!("Failed to launch {}: {}", info.display_name(), e);
        window.set_visible(false);
        return;
    }

    let (ow, oh) = (overlay.width(), overlay.height());
    let (Some(card), Some(b)) = (overlay.child(), source.compute_bounds(&overlay)) else {
        window.set_visible(false);
        return;
    };
    if ow <= 0 || oh <= 0 {
        window.set_visible(false);
        return;
    }

    let icon: gio::Icon = info
        .icon()
        .unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast());
    let paintable = IconTheme::for_display(&source.display()).lookup_by_gicon(
        &icon,
        256,
        overlay.scale_factor(),
        overlay.direction(),
        gtk4::IconLookupFlags::empty(),
    );

    let side = 56.0_f32;
    let x0 = b.x() + (b.width() - side) / 2.0;
    let y0 = b.y() + (b.height() - side) / 2.0;
    let (ms, mt) = (x0, y0);
    let (me, mb) = (ow as f32 - x0 - side, oh as f32 - y0 - side);

    let ghost = Picture::for_paintable(&paintable);
    ghost.add_css_class("launch-ghost");
    ghost.set_can_target(false);
    ghost.set_halign(gtk4::Align::Fill);
    ghost.set_valign(gtk4::Align::Fill);
    ghost.set_margin_start(ms.round() as i32);
    ghost.set_margin_top(mt.round() as i32);
    ghost.set_margin_end(me.round() as i32);
    ghost.set_margin_bottom(mb.round() as i32);
    overlay.add_overlay(&ghost);
    overlay.add_css_class("launching");

    let start = Cell::new(0_i64);
    let win = window.clone();
    ghost.add_tick_callback(move |g, clock| {
        let now = clock.frame_time();
        if start.get() == 0 {
            start.set(now);
        }
        let t = ((now - start.get()) as f64 / LAUNCH_ANIM_US as f64).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(3);
        let shrink = |v: f32| (v as f64 * (1.0 - e)).round().max(0.0) as i32;

        g.set_margin_start(shrink(ms));
        g.set_margin_top(shrink(mt));
        g.set_margin_end(shrink(me));
        g.set_margin_bottom(shrink(mb));

        g.set_opacity(if t < 0.6 { 1.0 } else { 1.0 - (t - 0.6) / 0.4 });
        card.set_opacity(1.0 - (t * 1.6).min(1.0));

        if t >= 1.0 {
            win.set_visible(false);
            reset_launch(&win);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

fn top_apps(usage: &UsageMap, slot: usize, n: usize) -> Vec<String> {
    let guard = usage.lock().unwrap();
    let total = guard.slots.len();
    if total == 0 {
        return Vec::new();
    }

    let prev = (slot + total - 1) % total;
    let next = (slot + 1) % total;
    let mut sums: HashMap<&String, (u64, u32)> = HashMap::new();
    for s in [prev, slot, next] {
        for (id, &count) in &guard.slots[s] {
            let e = sums.entry(id).or_insert((0, 0));
            e.0 += count as u64;
            if s == slot {
                e.1 = count;
            }
        }
    }

    let mut v: Vec<(&String, (u64, u32))> = sums.into_iter().collect();
    v.sort_by(|a, b| {
        b.1 .0
            .cmp(&a.1 .0)
            .then(b.1 .1.cmp(&a.1 .1))
            .then(a.0.cmp(b.0))
    });
    v.into_iter().take(n).map(|(k, _)| k.clone()).collect()
}

fn refresh_suggestions(suggbox: &GtkBox, window: &ApplicationWindow, usage: &UsageMap) {
    while let Some(child) = suggbox.first_child() {
        suggbox.remove(&child);
    }
    let ids = current_slot()
        .map(|slot| top_apps(usage, slot, SUGGEST_COUNT))
        .unwrap_or_default();
    let apps = load_apps();

    let row = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk4::Align::Center)
        .build();

    let mut shown = 0;
    for id in ids {
        if let Some(info) = apps
            .iter()
            .find(|a| a.id().map_or(false, |x| x.as_str() == id))
        {
            shown += 1;

            let cell = GtkBox::builder()
                .orientation(gtk4::Orientation::Vertical)
                .spacing(5)
                .halign(gtk4::Align::Center)
                .hexpand(false)
                .build();
            cell.append(&make_app_button(info.clone(), window));

            let idx = Label::new(Some(&shown.to_string()));
            idx.add_css_class("sugg-idx");
            idx.set_halign(gtk4::Align::Center);
            cell.append(&idx);

            row.append(&cell);
        }
    }

    suggbox.set_visible(true);
    if shown > 0 {
        suggbox.append(&row);
    } else {
        let placeholder = GtkBox::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(12)
            .margin_top(10)
            .halign(gtk4::Align::Center)
            .css_classes(["sugg-empty"])
            .build();

        let icon = Image::from_icon_name("missing");
        icon.set_pixel_size(56);
        placeholder.append(&icon);

        let text = GtkBox::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(2)
            .valign(gtk4::Align::Center)
            .build();

        let title = Label::new(Some("Indexing Suggestions..."));
        title.set_halign(gtk4::Align::Start);
        title.add_css_class("sugg-empty-title");
        text.append(&title);

        let sub = Label::new(Some("Keep using to get predicted suggestions"));
        sub.set_halign(gtk4::Align::Start);
        sub.add_css_class("sugg-empty-sub");
        text.append(&sub);

        placeholder.append(&text);
        suggbox.append(&placeholder);
    }
}

fn launch_suggestion(suggbox: &GtkBox, index: usize) -> bool {
    let Some(row) = suggbox.first_child() else {
        return false;
    };
    let mut cell = row.first_child();
    for _ in 0..index {
        cell = cell.and_then(|c| c.next_sibling());
    }
    let Some(button) = cell
        .and_then(|c| c.first_child())
        .and_then(|b| b.downcast::<Button>().ok())
    else {
        return false;
    };
    button.emit_clicked();
    true
}

fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q_lower = query.trim().to_lowercase();
    let q: Vec<char> = q_lower.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();

    let mut score = 0_i32;
    let mut ti = 0_usize;
    let mut prev: Option<usize> = None;

    for &qc in &q {
        let mut found = None;
        while ti < t.len() {
            if t[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let pos = found?;

        score += 10;
        match prev {
            Some(p) if pos == p + 1 => score += 15,
            Some(p) => score -= (pos - p - 1).min(10) as i32,
            None => score -= pos.min(10) as i32,
        }
        if pos == 0 || !t[pos - 1].is_alphanumeric() {
            score += 12;
        }
        prev = Some(pos);
    }

    let haystack: String = t.iter().collect();
    if haystack.contains(&q_lower) {
        score += 40;
        if haystack.starts_with(&q_lower) {
            score += 30;
        }
    }
    score -= t.len() as i32 / 8;
    Some(score)
}

fn result_cells(suggbox: &GtkBox) -> Vec<gtk4::Widget> {
    let mut cells = Vec::new();
    if let Some(row) = suggbox.first_child() {
        let mut c = row.first_child();
        while let Some(w) = c {
            c = w.next_sibling();
            cells.push(w);
        }
    }
    cells
}

fn selected_result(suggbox: &GtkBox) -> usize {
    result_cells(suggbox)
        .iter()
        .position(|c| c.has_css_class("sugg-top"))
        .unwrap_or(0)
}

fn move_selection(suggbox: &GtkBox, delta: i32) {
    let cells = result_cells(suggbox);
    if cells.is_empty() {
        return;
    }
    let cur = cells
        .iter()
        .position(|c| c.has_css_class("sugg-top"))
        .unwrap_or(0);
    let next = (cur as i32 + delta).rem_euclid(cells.len() as i32) as usize;
    cells[cur].remove_css_class("sugg-top");
    cells[next].add_css_class("sugg-top");
}

fn show_results(suggbox: &GtkBox, window: &ApplicationWindow, query: &str) {
    while let Some(child) = suggbox.first_child() {
        suggbox.remove(&child);
    }

    let mut scored: Vec<(i32, AppInfo)> = load_apps()
        .into_iter()
        .filter_map(|a| {
            let by_name = fuzzy_score(query, &a.display_name());
            let by_id = a
                .id()
                .and_then(|id| fuzzy_score(query, id.as_str().trim_end_matches(".desktop")));
            let s = match (by_name, by_id) {
                (Some(x), Some(y)) => x.max(y - 20),
                (Some(x), None) => x,
                (None, Some(y)) => y - 20,
                (None, None) => return None,
            };
            Some((s, a))
        })
        .collect();

    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1.display_name()
                .to_lowercase()
                .cmp(&b.1.display_name().to_lowercase())
        })
    });
    scored.truncate(SEARCH_RESULTS);

    if scored.is_empty() {
        let none = Label::new(Some("No applications found"));
        none.add_css_class("sugg-empty-sub");
        none.set_halign(gtk4::Align::Start);
        none.set_margin_start(10);
        suggbox.append(&none);
        return;
    }

    let row = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk4::Align::Center)
        .build();

    for (i, (_, info)) in scored.into_iter().enumerate() {
        let cell = GtkBox::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(0)
            .halign(gtk4::Align::Center)
            .hexpand(false)
            .build();

        let button = make_app_button(info.clone(), window);
        if i == 0 {
            cell.add_css_class("sugg-top");
        }
        cell.append(&button);

        let name = Label::new(Some(&info.display_name()));
        name.add_css_class("sugg-name");
        name.set_margin_end(25);
        name.set_halign(gtk4::Align::Center);
        cell.append(&name);

        row.append(&cell);
    }
    suggbox.append(&row);
}


// ───────────────────── mode panel: Files / Web / Calc / Clipboard / Pills ─────────────────────

/// "relevant" = fuzzy match that is not just a loose scatter of letters
fn relevant(query: &str, text: &str) -> Option<i32> {
    let qn = query.chars().filter(|c| !c.is_whitespace()).count() as i32;
    let s = fuzzy_score(query, text)?;
    if s >= 14 * qn { Some(s) } else { None }
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

fn copy_text(text: &str) {
    if let Some(d) = Display::default() {
        d.clipboard().set_text(text);
    }
}

// ---- calculator (no extra crates): + - * / % ^ ( ) , pi e, sqrt sin cos tan ln log abs exp floor ceil round ----

struct CalcParser {
    s: Vec<char>,
    i: usize,
    ops: usize,
}

impl CalcParser {
    fn peek(&mut self) -> Option<char> {
        while self.i < self.s.len() && self.s[self.i].is_whitespace() {
            self.i += 1;
        }
        self.s.get(self.i).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(c) = self.peek() {
            match c {
                '+' => {
                    self.i += 1;
                    self.ops += 1;
                    v += self.term()?;
                }
                '-' => {
                    self.i += 1;
                    self.ops += 1;
                    v -= self.term()?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(c) = self.peek() {
            match c {
                '*' | '×' => {
                    self.i += 1;
                    self.ops += 1;
                    v *= self.unary()?;
                }
                '/' | '÷' => {
                    self.i += 1;
                    self.ops += 1;
                    v /= self.unary()?;
                }
                '%' => {
                    self.i += 1;
                    self.ops += 1;
                    v %= self.unary()?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                self.ops += 1;
                Some(-self.unary()?)
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.peek() == Some('^') {
            self.i += 1;
            self.ops += 1;
            let e = self.unary()?;
            return Some(base.powf(e));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        let c = self.peek()?;
        if c == '(' {
            self.i += 1;
            let v = self.expr()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
            return Some(v);
        }
        if c.is_ascii_digit() || c == '.' {
            let st = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == '.') {
                self.i += 1;
            }
            let t: String = self.s[st..self.i].iter().collect();
            return t.parse().ok();
        }
        if c.is_alphabetic() {
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i].is_alphabetic() {
                self.i += 1;
            }
            let name: String = self.s[st..self.i].iter().collect::<String>().to_lowercase();
            match name.as_str() {
                "pi" => return Some(std::f64::consts::PI),
                "e" => return Some(std::f64::consts::E),
                _ => {}
            }
            if self.peek() != Some('(') {
                return None;
            }
            self.i += 1;
            let a = self.expr()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
            self.ops += 1;
            return match name.as_str() {
                "sqrt" => Some(a.sqrt()),
                "sin" => Some(a.sin()),
                "cos" => Some(a.cos()),
                "tan" => Some(a.tan()),
                "ln" => Some(a.ln()),
                "log" => Some(a.log10()),
                "abs" => Some(a.abs()),
                "exp" => Some(a.exp()),
                "floor" => Some(a.floor()),
                "ceil" => Some(a.ceil()),
                "round" => Some(a.round()),
                _ => None,
            };
        }
        None
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{:.10}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// None unless the query is a real expression (a bare "5" or "pi" is not "relevant")
fn calc(q: &str) -> Option<String> {
    let q = q.trim().trim_start_matches('=');
    let mut p = CalcParser { s: q.chars().collect(), i: 0, ops: 0 };
    let v = p.expr()?;
    if p.peek().is_some() || p.ops == 0 || !v.is_finite() {
        return None;
    }
    Some(fmt_num(v))
}

// ---- files: breadth-first walk of $HOME, aborts as soon as a newer query starts ----

fn search_files(q: &str, generation: &AtomicU64, mine: u64) -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((home, 0));
    let mut best: Vec<(i32, PathBuf)> = Vec::new();
    let mut visited = 0_usize;

    'walk: while let Some((dir, depth)) = queue.pop_front() {
        if generation.load(Ordering::Relaxed) != mine {
            return Vec::new();
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            visited += 1;
            if visited > FILE_SCAN_LIMIT {
                break 'walk;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_dir() && depth < FILE_DEPTH && !FILE_SKIP_DIRS.contains(&name.as_str()) {
                queue.push_back((e.path(), depth + 1));
            }
            if let Some(s) = relevant(q, &name) {
                best.push((s, e.path()));
            }
        }
        if best.len() > 400 {
            best.sort_by(|a, b| b.0.cmp(&a.0));
            best.truncate(PAGE_MAX * 4);
        }
    }

    best.sort_by(|a, b| b.0.cmp(&a.0));
    best.truncate(PAGE_MAX);
    best.into_iter().map(|(_, p)| p).collect()
}

// ---- clipboard history (memory only, never written to disk) ----

fn watch_clipboard(display: &Display, hist: Rc<RefCell<VecDeque<String>>>) {
    let cb = display.clipboard();

    let grab = {
        let cb = cb.clone();
        move || {
            let cb = cb.clone();
            let hist = hist.clone();
            glib::spawn_future_local(async move {
                if let Ok(Some(t)) = cb.read_text_future().await {
                    let t = t.to_string();
                    if t.trim().is_empty() || t.len() > CLIP_MAX_BYTES {
                        return;
                    }
                    let mut h = hist.borrow_mut();
                    h.retain(|x| x != &t);
                    h.push_front(t);
                    h.truncate(CLIP_KEEP);
                }
            });
        }
    };

    grab();
    cb.connect_changed(move |_| grab());
}

// ---- hits ----

#[derive(Clone)]
struct Hit {
    title: String,
    subtitle: String,
    icon: String,
    action: Rc<dyn Fn()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    Files,
    Web,
    Calc,
    Clip,
    Pills,
}

impl Cat {
    const ALL: [Cat; 5] = [Cat::Files, Cat::Web, Cat::Calc, Cat::Clip, Cat::Pills];

    fn idx(self) -> usize {
        self as usize
    }

    fn id(self) -> &'static str {
        match self {
            Cat::Files => "files",
            Cat::Web => "web",
            Cat::Calc => "calc",
            Cat::Clip => "clip",
            Cat::Pills => "pills",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Cat::Files => "Files",
            Cat::Web => "Web results",
            Cat::Calc => "Calculator",
            Cat::Clip => "Clipboard",
            Cat::Pills => "Pills",
        }
    }
}

fn pretty_dir(p: &std::path::Path) -> String {
    let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && dir.starts_with(&home) {
        format!("~{}", &dir[home.len()..])
    } else {
        dir
    }
}

fn file_hits(paths: Vec<PathBuf>) -> Vec<Hit> {
    paths
        .into_iter()
        .map(|p| {
            let uri = gio::File::for_path(&p).uri().to_string();
            Hit {
                title: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                subtitle: pretty_dir(&p),
                icon: if p.is_dir() { "folder" } else { "text-x-generic" }.to_string(),
                action: Rc::new(move || {
                    let _ = AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
                }),
            }
        })
        .collect()
}

// TODO: real web results need an HTTP client + a search API; for now this is one "search the web" hit
fn web_hits(q: &str) -> Vec<Hit> {
    let q = q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let uri = format!("https://duckduckgo.com/?q={}", url_encode(q));
    vec![Hit {
        title: format!("Search the web for “{q}”"),
        subtitle: "duckduckgo.com".to_string(),
        icon: "applications-internet".to_string(),
        action: Rc::new(move || {
            let _ = AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
        }),
    }]
}

fn calc_hits(q: &str) -> Vec<Hit> {
    match calc(q) {
        Some(res) => vec![Hit {
            title: format!("= {res}"),
            subtitle: "Enter to copy the result".to_string(),
            icon: "accessories-calculator".to_string(),
            action: Rc::new(move || copy_text(&res)),
        }],
        None => Vec::new(),
    }
}

fn clip_hits(q: &str, hist: &VecDeque<String>) -> Vec<Hit> {
    let ql = q.trim().to_lowercase();
    if ql.is_empty() {
        return Vec::new();
    }
    hist.iter()
        .filter(|t| t.to_lowercase().contains(&ql))
        .take(PAGE_MAX)
        .map(|t| {
            let first = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            let text = t.clone();
            Hit {
                title: first,
                subtitle: "Clipboard · Enter to copy again".to_string(),
                icon: "edit-paste".to_string(),
                action: Rc::new(move || copy_text(&text)),
            }
        })
        .collect()
}

fn pill_hits(q: &str) -> Vec<Hit> {
    let mut v: Vec<(i32, Hit)> = PILLS
        .iter()
        .filter_map(|(name, icon, cmd)| {
            let s = relevant(q, name)?;
            let cmd = cmd.to_string();
            Some((
                s,
                Hit {
                    title: name.to_string(),
                    subtitle: cmd.clone(),
                    icon: icon.to_string(),
                    action: Rc::new(move || {
                        let mut c = Command::new("sh");
                        c.arg("-c").arg(&cmd);
                        let _ = spawn_detached(&mut c);
                    }),
                },
            ))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, h)| h).collect()
}

fn hit_button(hit: &Hit, window: &ApplicationWindow) -> Button {
    let icon = Image::from_icon_name(&hit.icon);
    icon.set_pixel_size(28);

    let title = Label::new(Some(&hit.title));
    title.add_css_class("hit-title");
    title.set_xalign(0.0);
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_max_width_chars(70);

    let sub = Label::new(Some(&hit.subtitle));
    sub.add_css_class("hit-sub");
    sub.set_xalign(0.0);
    sub.set_ellipsize(pango::EllipsizeMode::Middle);
    sub.set_max_width_chars(80);

    let text = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(1)
        .valign(gtk4::Align::Center)
        .build();
    text.append(&title);
    text.append(&sub);

    let row = GtkBox::builder().orientation(gtk4::Orientation::Horizontal).spacing(12).build();
    row.append(&icon);
    row.append(&text);

    let button = Button::builder().child(&row).hexpand(true).build();
    button.add_css_class("flat");
    button.add_css_class("hit-row");

    let action = hit.action.clone();
    let win = window.clone();
    button.connect_clicked(move |_| {
        action();
        win.set_visible(false);
    });
    button
}

fn clear_box(b: &GtkBox) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

fn select_page(stack: &Stack, tabs: &[(&'static str, Button)], id: &str) {
    stack.set_visible_child_name(id);
    for (tid, b) in tabs {
        if *tid == id {
            b.add_css_class("active");
        } else {
            b.remove_css_class("active");
        }
    }
}

#[derive(Clone)]
struct ModePanel {
    window: ApplicationWindow,
    stack: Stack,
    tabs: Vec<(&'static str, Button)>,
    home: GtkBox,
    pages: Vec<GtkBox>,
    results: Rc<RefCell<Vec<Vec<Hit>>>>,
    clip: Rc<RefCell<VecDeque<String>>>,
    generation: Arc<AtomicU64>,
    apps_box: GtkBox,
    hits_scroller: ScrolledWindow,
    sel: Rc<Cell<Option<usize>>>,
    focused: Rc<Cell<bool>>,
}

impl ModePanel {
    fn new(window: &ApplicationWindow, host: &GtkBox, display: &Display, apps_box: &GtkBox) -> Self {
        let clip = Rc::new(RefCell::new(VecDeque::new()));
        watch_clipboard(display, clip.clone());

        let stack = Stack::builder()
            .transition_type(StackTransitionType::Crossfade)
            .transition_duration(120)
            .hexpand(true)
            .vexpand(true)
            .build();

        let make_page = || {
            GtkBox::builder()
                .orientation(gtk4::Orientation::Vertical)
                .spacing(4)
                .margin_start(20)
                .margin_end(20)
                .margin_top(10)
                .margin_bottom(14)
                .valign(gtk4::Align::Start)
                .build()
        };

        let home = make_page();
        stack.add_named(&home, Some("home"));
        let mut pages = Vec::new();
        for c in Cat::ALL {
            let p = make_page();
            stack.add_named(&p, Some(c.id()));
            pages.push(p);
        }

        let tabbar = GtkBox::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .margin_start(20)
            .margin_end(20)
            .margin_top(12)
            .css_classes(["mode-tabs"])
            .build();

        let mut tabs: Vec<(&'static str, Button)> = Vec::new();
        let labels = std::iter::once(("home", "Home")).chain(Cat::ALL.iter().map(|c| (c.id(), c.title())));
        for (id, label) in labels {
            let b = Button::with_label(label);
            b.add_css_class("flat");
            b.add_css_class("mode-tab");
            tabbar.append(&b);
            tabs.push((id, b));
        }
        select_page(&stack, &tabs, "home");

        let scroller = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .min_content_height(PANEL_HEIGHT)
            .max_content_height(PANEL_HEIGHT)
            .vexpand(true)
            .child(&stack)
            .build();

        host.append(&tabbar);
        host.append(&scroller);

        let panel = Self {
            window: window.clone(),
            stack,
            tabs,
            home,
            pages,
            results: Rc::new(RefCell::new(vec![Vec::new(); Cat::ALL.len()])),
            clip,
            generation: Arc::new(AtomicU64::new(0)),
            apps_box: apps_box.clone(),
            hits_scroller: scroller,
            sel: Rc::new(Cell::new(None)),
            focused: Rc::new(Cell::new(false)),
        };

        for (id, b) in panel.tabs.clone() {
            let p = panel.clone();
            b.connect_clicked(move |_| p.select(id));
        }
        panel
    }

    fn select(&self, id: &str) {
        select_page(&self.stack, &self.tabs, id);
        self.hits_scroller.vadjustment().set_value(0.0);
        self.focused.set(false);
        self.sel.set(None);
        self.apply_selection();
    }

    fn mode_focused(&self) -> bool {
        self.focused.get()
    }

    /// hits shown on the visible page, in the same order as their rows
    fn visible_hits(&self) -> Vec<Hit> {
        let name = self.stack.visible_child_name().map(|n| n.to_string()).unwrap_or_default();
        let res = self.results.borrow();
        if name.is_empty() || name == "home" {
            Cat::ALL.iter().filter_map(|c| res[c.idx()].first().cloned()).collect()
        } else {
            Cat::ALL
                .iter()
                .find(|c| c.id() == name)
                .map(|c| res[c.idx()].iter().take(PAGE_MAX).cloned().collect())
                .unwrap_or_default()
        }
    }

    fn visible_rows(&self) -> Vec<gtk4::Widget> {
        let name = self.stack.visible_child_name().map(|n| n.to_string()).unwrap_or_default();
        let page = match Cat::ALL.iter().position(|c| c.id() == name) {
            Some(i) => self.pages[i].clone(),
            None => self.home.clone(),
        };
        let mut rows = Vec::new();
        let mut c = page.first_child();
        while let Some(w) = c {
            c = w.next_sibling();
            if w.has_css_class("hit-row") {
                rows.push(w);
            }
        }
        rows
    }

    fn scroll_to(&self, row: &gtk4::Widget) {
        let adj = self.hits_scroller.vadjustment();
        if let Some(b) = row.compute_bounds(&self.stack) {
            let (y, h) = (b.y() as f64, b.height() as f64);
            let (top, page) = (adj.value(), adj.page_size());
            if y < top {
                adj.set_value((y - 8.0).max(0.0));
            } else if y + h > top + page {
                adj.set_value(y + h - page + 8.0);
            }
        }
    }

    fn apply_selection(&self) {
        let rows = self.visible_rows();
        if self.focused.get() && rows.is_empty() {
            self.focused.set(false);
        }
        let cur = if self.focused.get() {
            Some(self.sel.get().unwrap_or(0).min(rows.len().saturating_sub(1)))
        } else {
            None
        };
        self.sel.set(cur);

        for (i, r) in rows.iter().enumerate() {
            if Some(i) == cur {
                r.add_css_class("hit-sel");
            } else {
                r.remove_css_class("hit-sel");
            }
        }
        if self.focused.get() {
            self.apps_box.add_css_class("mode-focus");
        } else {
            self.apps_box.remove_css_class("mode-focus");
        }
        if let Some(i) = cur {
            self.scroll_to(&rows[i]);
        }
    }

    /// back to the app results (left / right, or up from the first mode result)
    fn focus_apps(&self) {
        self.focused.set(false);
        self.sel.set(None);
        self.apply_selection();
    }

    /// down: apps -> first mode result -> next ...; up: previous, and from the first one back to apps
    fn move_hit(&self, delta: i32) {
        let n = self.visible_rows().len();
        if n == 0 {
            return;
        }
        if !self.focused.get() {
            if delta > 0 {
                self.focused.set(true);
                self.sel.set(Some(0));
                self.apply_selection();
            }
            return;
        }
        let cur = self.sel.get().unwrap_or(0);
        if delta < 0 {
            if cur == 0 {
                self.focus_apps();
                return;
            }
            self.sel.set(Some(cur - 1));
        } else {
            self.sel.set(Some((cur + 1).min(n - 1)));
        }
        self.apply_selection();
    }

    fn is_home(&self) -> bool {
        self.stack.visible_child_name().map_or(true, |n| n == "home")
    }

    fn next_page(&self, delta: i32) {
        let cur = self.stack.visible_child_name().map(|n| n.to_string()).unwrap_or_default();
        let i = self.tabs.iter().position(|(id, _)| *id == cur).unwrap_or(0) as i32;
        let n = (i + delta).rem_euclid(self.tabs.len() as i32) as usize;
        self.select(self.tabs[n].0);
    }

    fn reset(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        for r in self.results.borrow_mut().iter_mut() {
            r.clear();
        }
        self.select("home");
        self.render();
    }

    fn search(&self, q: &str) {
        let q = q.trim().to_string();
        let mine = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.focused.set(false);
        self.sel.set(None);

        {
            let mut r = self.results.borrow_mut();
            r[Cat::Files.idx()].clear();
            r[Cat::Web.idx()] = web_hits(&q);
            r[Cat::Calc.idx()] = calc_hits(&q);
            r[Cat::Clip.idx()] = clip_hits(&q, &self.clip.borrow());
            r[Cat::Pills.idx()] = pill_hits(&q);
        }
        self.render();

        if q.chars().count() >= FILE_MIN_QUERY {
            let (tx, rx) = async_channel::bounded::<Vec<PathBuf>>(1);
            let generation = self.generation.clone();
            let qq = q.clone();
            std::thread::spawn(move || {
                let found = search_files(&qq, &generation, mine);
                let _ = tx.send_blocking(found);
            });

            let panel = self.clone();
            glib::spawn_future_local(async move {
                if let Ok(paths) = rx.recv().await {
                    if panel.generation.load(Ordering::SeqCst) == mine {
                        panel.results.borrow_mut()[Cat::Files.idx()] = file_hits(paths);
                        panel.render();
                    }
                }
            });
        }
    }

    fn render(&self) {
        let res = self.results.borrow();
        clear_box(&self.home);

        for c in Cat::ALL {
            let hits = &res[c.idx()];
            let page = &self.pages[c.idx()];
            clear_box(page);

            // home: top hit of each category, nothing at all for empty ones
            if let Some(top) = hits.first() {
                let head = Label::new(Some(c.title()));
                head.add_css_class("hit-cat");
                head.set_halign(gtk4::Align::Start);
                self.home.append(&head);
                self.home.append(&hit_button(top, &self.window));
            }

            if hits.is_empty() {
                let none = Label::new(Some("Nothing found"));
                none.add_css_class("sugg-empty-sub");
                none.set_halign(gtk4::Align::Start);
                page.append(&none);
            } else {
                for h in hits.iter().take(PAGE_MAX) {
                    page.append(&hit_button(h, &self.window));
                }
            }
        }
        self.apply_selection();
    }

    /// runs the selected hit of the visible page (first one if none is selected)
    fn activate_current(&self) -> bool {
        let hits = self.visible_hits();
        let idx = self.sel.get().unwrap_or(0);
        match hits.get(idx).or(hits.first()) {
            Some(h) => {
                let a = h.action.clone();
                a();
                self.window.set_visible(false);
                true
            }
            None => false,
        }
    }
}

#[derive(Clone)]
struct SearchUi {
    entry: GtkEntry,
    logo: Label,
    sugg_title: Label,
    apps_title: Label,
    scroller: ScrolledWindow,
    mode_panel: GtkBox,
    panel: ModePanel,
    anchor: GtkBox,
}

impl SearchUi {
    fn active(&self) -> bool {
        WidgetExt::is_visible(&self.entry)
    }

    fn enter(&self) {
        self.logo.set_text(SEARCH_GLYPH);
        self.entry.set_visible(true);
        self.sugg_title.set_text("Applications");
        self.apps_title.set_visible(false);
        self.scroller.set_visible(false);
        self.mode_panel.set_visible(true);
        self.panel.select("home");
    }

    fn exit(&self) {
        self.entry.set_visible(false);
        self.entry.set_text("");
        self.logo.set_text(LOGO_TEXT);
        self.sugg_title.set_text("Suggestions");
        self.apps_title.set_visible(true);
        self.scroller.set_visible(true);
        self.mode_panel.set_visible(false);
        self.panel.reset();
        self.anchor.grab_focus();
    }
}

fn populate(grid: &FlowBox, window: &ApplicationWindow) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }

    for info in load_apps() {
        grid.insert(&make_app_button(info, window), -1);
    }
}

fn max_monitor_height(display: &Display) -> i32 {
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i)?.downcast::<gtk4::gdk::Monitor>().ok())
        .map(|m| m.geometry().height())
        .max()
        .unwrap_or(1080)
}

fn animate_on_map(win: &ApplicationWindow, target: &gtk4::Box) {
    let t = target.clone();
    win.connect_map(move |_| t.add_css_class("enter"));
    let t = target.clone();
    win.connect_unmap(move |_| t.remove_css_class("enter"));
}

fn build_window(
    app: &Application,
    usage: &UsageMap,
) -> (ApplicationWindow, ApplicationWindow, FlowBox, GtkBox, SearchUi) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("alt.")
        .default_width(800)
        // .default_height(560)
        .resizable(false)
        .decorated(false)
        .css_classes(["win"])
        .build();

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("altDot"));
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, 130);

    let bd = ApplicationWindow::builder()
        .application(app)
        .title("altbd")
        .fullscreened(true)
        .decorated(false)
        .css_classes(["backDrop"])
        .build();

    bd.init_layer_shell();
    bd.set_layer(Layer::Overlay);
    bd.set_namespace(Some("altBackdrop"));
    bd.set_keyboard_mode(KeyboardMode::None);
    bd.set_anchor(Edge::Top, true);
    bd.set_anchor(Edge::Bottom, true);
    bd.set_anchor(Edge::Right, true);
    bd.set_anchor(Edge::Left, true);

    let bd_fill = gtk4::Box::builder().css_classes(["bd-fill"]).build();
    bd.set_child(Some(&bd_fill));
    animate_on_map(&bd, &bd_fill);

    let bd_for_hide = bd.clone();
    window.connect_hide(move |_| bd_for_hide.set_visible(false));

    let grid = FlowBox::builder()
        .selection_mode(SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(4)
        .max_children_per_line(5)
        .row_spacing(25)
        .column_spacing(25)
        .margin_top(10)
        .margin_bottom(30)
        .margin_start(20)
        .margin_end(20)
        .valign(gtk4::Align::Start)
        .build();

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&grid)
        .hexpand(true)
        .vexpand(true)
        .height_request(360)
        .build();

    let topbox = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(5)
        .hexpand(true)
        .css_classes(["topboxy"])
        .build();

    let header = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(2)
        .css_classes(["winheader"])
        .build();

    let altdotlabel = Label::new(Some(LOGO_TEXT));
    altdotlabel.set_margin_start(10);
    altdotlabel.set_css_classes(&["altlogo"]);
    header.append(&altdotlabel);

    let search_entry = GtkEntry::builder()
        .placeholder_text("Search apps")
        .has_frame(false)
        .hexpand(true)
        .visible(false)
        .margin_start(10)
        .css_classes(["search-entry"])
        .build();
    header.append(&search_entry);

    let suggestionbox = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(0)
        .css_classes(["suggapps"])
        .build();

    let suggboxtitle = Label::new(Some("Suggestions"));
    suggboxtitle.set_margin_start(10);
    suggboxtitle.set_halign(gtk4::Align::Start);
    suggboxtitle.set_hexpand(true);
    suggboxtitle.set_css_classes(&["pagetitle"]);

    topbox.append(&header);
    topbox.append(&suggboxtitle);
    topbox.append(&suggestionbox);

    let allappstitle = Label::new(Some("App Library"));
    allappstitle.set_margin_start(30);
    allappstitle.set_margin_top(5);
    allappstitle.set_halign(gtk4::Align::Start);
    allappstitle.set_hexpand(true);
    allappstitle.set_css_classes(&["pagetitle"]);


    let card = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["win-card"])
        .focusable(true)
        .build();
    card.append(&topbox);
    card.append(&allappstitle);
    card.append(&scroller);

    let mode_panel = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .visible(false)
        .css_classes(["mode-panel"])
        .build();
    card.append(&mode_panel);

    let display = Display::default().expect("no display");
    let panel = ModePanel::new(&window, &mode_panel, &display, &suggestionbox);

    let search = SearchUi {
        entry: search_entry,
        logo: altdotlabel,
        sugg_title: suggboxtitle,
        apps_title: allappstitle,
        scroller: scroller.clone(),
        mode_panel,
        panel,
        anchor: card.clone(),
    };

    let overlay = Overlay::new();
    overlay.set_child(Some(&card));
    window.set_child(Some(&overlay));
    animate_on_map(&window, &card);

    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    {
        let win = window.clone();
        let sugg = suggestionbox.clone();
        let ui = search.clone();
        let u = usage.clone();
        keys.connect_key_pressed(move |_, key, _, state| {
            if let Some(p) = picker_widget(&win) {
                if key == gtk4::gdk::Key::Escape {
                    close_picker(&win);
                    if ui.active() {
                        ui.entry.grab_focus();
                    } else {
                        ui.anchor.grab_focus();
                    }
                    return glib::Propagation::Stop;
                }
                if let Some(n) = key.to_unicode().and_then(|c| c.to_digit(10)) {
                    let mut btns = Vec::new();
                    collect_buttons(&p, &mut btns);
                    if n >= 1 {
                        if let Some(b) = btns.get((n - 1) as usize) {
                            b.emit_clicked();
                        }
                    }
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
            if key == gtk4::gdk::Key::Escape {
                if ui.active() {
                    ui.exit();
                    refresh_suggestions(&sugg, &win, &u);
                } else {
                    win.set_visible(false);
                }
                return glib::Propagation::Stop;
            }
            if ui.active() && (key == gtk4::gdk::Key::Tab || key == gtk4::gdk::Key::ISO_Left_Tab) {
                ui.panel.next_page(if key == gtk4::gdk::Key::ISO_Left_Tab { -1 } else { 1 });
                return glib::Propagation::Stop;
            }
            if ui.active() {
                match key {
                    // up / down walk the mode-panel results
                    gtk4::gdk::Key::Down => {
                        ui.panel.move_hit(1);
                        return glib::Propagation::Stop;
                    }
                    gtk4::gdk::Key::Up => {
                        ui.panel.move_hit(-1);
                        return glib::Propagation::Stop;
                    }
                    // left / right: back to the app results, then move through them
                    gtk4::gdk::Key::Left | gtk4::gdk::Key::Right => {
                        if ui.panel.mode_focused() {
                            ui.panel.focus_apps();
                        } else {
                            move_selection(&sugg, if key == gtk4::gdk::Key::Left { -1 } else { 1 });
                        }
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }

            let blocked = gtk4::gdk::ModifierType::CONTROL_MASK
                | gtk4::gdk::ModifierType::ALT_MASK
                | gtk4::gdk::ModifierType::SUPER_MASK
                | gtk4::gdk::ModifierType::META_MASK;
            if !state.intersects(blocked) {
                if let Some(c) = key
                    .to_unicode()
                    .filter(|c| !c.is_control() && !c.is_whitespace())
                {
                    if c.is_numeric() {
                        if ui.active() {
                            return glib::Propagation::Proceed;
                        }
                        if let Some(n) = c.to_digit(10) {
                            if (1..=SUGGEST_COUNT as u32).contains(&n)
                                && launch_suggestion(&sugg, (n - 1) as usize)
                            {
                                return glib::Propagation::Stop;
                            }
                        }
                        // not a shortcut: fall through and start a search with the digit
                    }

                    if !ui.active() {
                        ui.enter();
                        ui.entry.grab_focus();
                        ui.entry.set_text(&c.to_string());
                        ui.entry.set_position(-1);
                        return glib::Propagation::Stop;
                    }
                }
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);

    {
        let ui = search.clone();
        let sg = suggestionbox.clone();
        let w = window.clone();
        let u = usage.clone();
        search.entry.connect_changed(move |e| {
            if !ui.active() {
                return;
            }
            let q = e.text().to_string();
            if q.trim().is_empty() {
                ui.exit();
                refresh_suggestions(&sg, &w, &u);
            } else {
                show_results(&sg, &w, &q);
                ui.panel.search(&q);
            }
        });

        let sg = suggestionbox.clone();
        let panel = search.panel.clone();
        search.entry.connect_activate(move |_| {
            if panel.mode_focused() {
                panel.activate_current();
            } else if panel.is_home() {
                if !launch_suggestion(&sg, selected_result(&sg)) {
                    panel.activate_current();
                }
            } else {
                panel.activate_current();
            }
        });
    }

    let slide_px = max_monitor_height(&display);

    let provider = CssProvider::new();
    provider.load_from_data(
        &r#"

        :root {
            --pri-border: rgba(255, 255, 255, 0.1);
        }

        .win, .backDrop {
            background: transparent;
        }

        .win-card:focus,
        .win-card:focus-visible {
            outline: none;
        }

        .win-card {
            background-color: rgba(10, 10, 10, 0.49);
            border-radius: 35px;
            border: 1px solid var(--pri-border);
            opacity: 0;
        }

        .bd-fill {
            background: linear-gradient(to bottom, #04040498, transparent);
            opacity: 0;
        }

        @keyframes win-zoom-out {
            from { opacity: 0; transform: scale(1.08); }
            to   { opacity: 1; transform: scale(1); }
        }
        .win-card.enter {
            animation-name: win-zoom-out;
            animation-duration: 140ms;
            animation-timing-function: ease-out;
            animation-fill-mode: both;
        }

        @keyframes bd-slide-down {
            from { opacity: 0; transform: translateY(-__SLIDE__px); }
            to   { opacity: 1; transform: translateY(0px); }
        }
        .bd-fill.enter {
            animation-name: bd-slide-down;
            animation-duration: 350ms;
            animation-timing-function: ease-out;
            animation-fill-mode: both;
        }

        .app-btn {
            all: unset;
            padding: 12px;
            border-radius: 20px;
            border: none;
        }
        .app-btn:hover {
            background-color: rgba(255, 255, 255, 0.06);
        }

        scrollbar {
            all: unset;
            background-color: transparent;    
        }

        scrollbar trough {
            background-color: transparent; 
        }

        scrollbar trough:hover {
            background-color: rgba(0, 0, 0, 0);
        }

        scrollbar range {
            background-color: transparent;
        }

        scrollbar slider {
            background-color: transparent;
            border: 2px solid transparent;
        }

        scrollbar slider:hover {
            background-color: transparent;
        }

        .winheader {
            padding: 0 0 5px 0;
            border-bottom: 1px solid var(--pri-border);
        }

        .sugg-empty-title {
            font-size: 14px;
            font-weight: 700;
            color: rgba(253, 253, 253, 0.7);
        }

        .sugg-empty-sub {
            font-size: 11px;
            color: rgba(253, 253, 253, 0.4);
        }

        .sugg-empty image {
            opacity: 0.5;
        }

        .sugg-idx {
            padding: 5px;
            min-width: 15px;
            background-color: rgba(0, 0, 0, 0.53);
            border-radius: 5px;
            font-size: 11px;
            font-weight: 600;
            color: rgba(253, 253, 253, 0.4);
        }

        entry.search-entry {
            background: none;
            border: none;
            box-shadow: none;
            outline: none;
            font-size: 16px;
            color: rgba(253, 253, 253, 0.29);
            padding: 0 6px;
            min-height: 0;
        }

        .sugg-name {
            font-size: 11px;
            color: rgba(253, 253, 253, 0.29);
            font-weight: 700;
        }

        .sugg-top {
            border-radius: 20px;
            background-color: rgba(255, 255, 255, 0.06);
        }

        .topboxy {
            padding: 20px;
            border-bottom: 1px solid var(--pri-border);
        }

        .altlogo {
            font-size: 20px;
            font-weight: 800;
            color: rgba(253, 253, 253, 0.29);
        }

        .mode-tab {
            all: unset;
            padding: 4px 12px;
            border-radius: 20px;
            font-size: 12px;
            font-weight: 600;
            color: rgba(253, 253, 253, 0.4);
        }
        .mode-tab:hover {
            background-color: rgba(255, 255, 255, 0.06);
        }
        .mode-tab.active {
            background-color: rgba(255, 255, 255, 0.12);
            color: rgba(253, 253, 253, 0.8);
        }

        .hit-row {
            all: unset;
            padding: 8px 12px;
            border-radius: 16px;
        }
        .hit-row:hover {
            background-color: rgba(255, 255, 255, 0.06);
        }
        .hit-title {
            font-size: 13px;
            font-weight: 700;
            color: rgba(253, 253, 253, 0.7);
        }
        .hit-sub {
            font-size: 11px;
            color: rgba(253, 253, 253, 0.4);
        }
        .hit-cat {
            margin: 6px 0 0 12px;
            font-size: 11px;
            font-weight: 300;
            color: rgba(253, 253, 253, 0.29);
        }

        .hit-row.hit-sel {
            background-color: rgba(255, 255, 255, 0.12);
        }
        .mode-focus .sugg-top {
            background-color: transparent;
        }

        .term-pick {
            padding: 22px 26px;
            border-radius: 24px;
            background-color: rgba(18, 18, 18, 0.95);
            border: 1px solid var(--pri-border);
        }
        .term-pick-title {
            font-size: 14px;
            font-weight: 700;
            color: rgba(253, 253, 253, 0.7);
        }
        .term-btn {
            all: unset;
            padding: 8px 20px;
            border-radius: 14px;
            font-size: 13px;
            font-weight: 600;
            color: rgba(253, 253, 253, 0.7);
            background-color: rgba(255, 255, 255, 0.08);
        }
        .term-btn:hover, .term-btn:focus {
            background-color: rgba(255, 255, 255, 0.2);
        }

        .pagetitle {
            font-size: 14px;
            font-weight: 300;
            color: rgba(253, 253, 253, 0.29);
        }

        "#
        .replace("__SLIDE__", &slide_px.to_string()),
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    (window, bd, grid, suggestionbox, search)
}

struct UpdateOutcome {
    updated: bool,
    version: String,
}

const ALT_BIN_PATH: &str = "/usr/bin/altDot";

fn ensure_writable(path: &str) -> Result<(), String> {
    use std::ffi::CString;

    let c_path = CString::new(path).map_err(|e| e.to_string())?;

    let path_obj = std::path::Path::new(path);
    if path_obj.exists() {
        let writable = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 };
        if !writable {
            return Err(format!(
                "no write permission for '{}'. Try: sudo calibrate --update",
                path
            ));
        }
    }

    if let Some(parent) = path_obj.parent() {
        let c_parent = CString::new(parent.to_string_lossy().to_string())
            .map_err(|e| e.to_string())?;
        let dir_writable = unsafe { libc::access(c_parent.as_ptr(), libc::W_OK) == 0 };
        if !dir_writable {
            return Err(format!(
                "no write permission for directory '{}'. Try: sudo calibrate --update",
                parent.display()
            ));
        }
    }

    Ok(())
}

fn check_for_updates() -> Result<UpdateOutcome, Box<dyn std::error::Error>> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner("ekahPruthvi")
        .repo_name("altDotlauncher")
        .bin_name("altDot")
        .target("x86_64")
        .bin_install_path(ALT_BIN_PATH)
        .show_download_progress(true)
        .no_confirm(true)
        .current_version(cargo_crate_version!())
        .build()?
        .update()?;

    Ok(UpdateOutcome {
        updated: status.is_updated(),
        version: status.version().to_string(),
    })
}

fn run_update() -> i32 {
    println!("Checking for updates...");

    if let Err(msg) = ensure_writable(ALT_BIN_PATH) {
        eprintln!("Cannot update calibrate: {}", msg);
        return 1;
    }

    match check_for_updates() {
        Ok(outcome) => {
            if outcome.updated {
                println!("calibrate updated to version {}", outcome.version);
            } else {
                println!("calibrate is already up to date ({})", outcome.version);
            }
            0
        }
        Err(e) => {
            eprintln!("Update failed: {}", e);
            1
        }
    }
}

fn main() -> glib::ExitCode {
    extend_data_dirs();

    let args: Vec<String> = std::env::args().collect();
    
    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        exit(1);
    } else if args.len() > 1 && (args[1] == "--update" || args[1] == "-U") {
        exit(run_update());
    }


    let app = Application::builder()
        .application_id("ekah.scu.alt")
        .build();

    app.connect_startup(|app| {
        std::mem::forget(app.hold());
        let apps = load_apps();
        let resolver = build_resolver(&apps);
        let usage: UsageMap = Arc::new(Mutex::new(load_usage()));
        spawn_tracker(resolver, usage.clone());

        let (window, bd, grid, suggestionbox, search) = build_window(app, &usage);

        let last_slot = Rc::new(Cell::new(current_slot()));
        {
            let sb = suggestionbox.clone();
            let w = window.clone();
            let u = usage.clone();
            let last = last_slot.clone();
            let ui = search.clone();
            refresh_suggestions(&sb, &w, &u);
            glib::timeout_add_seconds_local(30, move || {
                let now = current_slot();
                if now != last.get() {
                    last.set(now);
                    if !ui.active() {
                        refresh_suggestions(&sb, &w, &u);
                    }
                }
                glib::ControlFlow::Continue
            });
        }

        let toggle = {
            let window = window.clone();
            let suggestionbox = suggestionbox.clone();
            let usage = usage.clone();
            let search = search.clone();
            move || {
                if window.is_visible() {
                    window.set_visible(false);
                } else {
                    reset_launch(&window);
                    search.exit();
                    populate(&grid, &window);
                    refresh_suggestions(&suggestionbox, &window, &usage);
                    bd.present();
                    window.present();
                    search.anchor.grab_focus();
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
        install_signal_handlers();
        {
            let t = toggle.clone();
            let window = window.clone();
            let search = search.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                if SIG_TOGGLE.swap(false, Ordering::SeqCst) {
                    t();
                }
                if SIG_SEARCH.swap(false, Ordering::SeqCst) {
                    if !window.is_visible() {
                        t();
                    }
                    if let Some(q) = take_search_request() {
                        search.enter();
                        search.entry.grab_focus();
                        search.entry.set_text(&q);
                        search.entry.set_position(-1);
                    }
                }
                glib::ControlFlow::Continue
            });
        }

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