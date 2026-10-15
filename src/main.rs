use async_channel::Sender;
use gtk4::gdk::Display;
use gtk4::gio::{self, AppInfo};
use gtk4::prelude::*;
use gtk4::{
    glib, Application, ApplicationWindow, Button, CssProvider, Entry as GtkEntry, FlowBox, IconTheme, Image, Label,
    Overlay, Picture, ScrolledWindow, SelectionMode, Box as GtkBox,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use infoprober::{Document, Entry};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::process::exit;
use self_update::cargo_crate_version;

const POLL_SECS: u64 = 5;
const SAVE_EVERY_SECS: u64 = 60;
const SUGGEST_COUNT: usize = 5;
const SEARCH_RESULTS: usize = 3;
const LOGO_TEXT: &str = "alt●";
const SEARCH_GLYPH: &str = "";
const LAUNCH_ANIM_US: i64 = 300_000;

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

fn reset_launch(window: &ApplicationWindow) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        return;
    };
    let mut ghosts = Vec::new();
    let mut c = overlay.first_child();
    while let Some(w) = c {
        c = w.next_sibling();
        if w.has_css_class("launch-ghost") {
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

fn launch_animated(info: &AppInfo, source: &Button, window: &ApplicationWindow) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        let _ = info.launch(&[], None::<&gio::AppLaunchContext>);
        window.set_visible(false);
        return;
    };
    if overlay.has_css_class("launching") {
        return;
    }

    if let Err(e) = info.launch(&[], None::<&gio::AppLaunchContext>) {
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

#[derive(Clone)]
struct SearchUi {
    entry: GtkEntry,
    logo: Label,
    sugg_title: Label,
    apps_title: Label,
    scroller: ScrolledWindow,
    mode_panel: GtkBox,
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
    }

    fn exit(&self) {
        self.entry.set_visible(false);
        self.entry.set_text("");
        self.logo.set_text(LOGO_TEXT);
        self.sugg_title.set_text("Suggestions");
        self.apps_title.set_visible(true);
        self.scroller.set_visible(true);
        self.mode_panel.set_visible(false);
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
        .default_height(560)
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

    let search = SearchUi {
        entry: search_entry,
        logo: altdotlabel,
        sugg_title: suggboxtitle,
        apps_title: allappstitle,
        scroller: scroller.clone(),
        mode_panel,
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
            if key == gtk4::gdk::Key::Escape {
                if ui.active() {
                    ui.exit();
                    refresh_suggestions(&sugg, &win, &u);
                } else {
                    win.set_visible(false);
                }
                return glib::Propagation::Stop;
            }
            if ui.active() {
                let step = match key {
                    gtk4::gdk::Key::Left | gtk4::gdk::Key::Up => Some(-1),
                    gtk4::gdk::Key::Right | gtk4::gdk::Key::Down => Some(1),
                    _ => None,
                };
                if let Some(d) = step {
                    move_selection(&sugg, d);
                    return glib::Propagation::Stop;
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
                        if !ui.active() {
                            if let Some(n) = c.to_digit(10) {
                                if (1..=SUGGEST_COUNT as u32).contains(&n)
                                    && launch_suggestion(&sugg, (n - 1) as usize)
                                {
                                    return glib::Propagation::Stop;
                                }
                            }
                        }
                        return glib::Propagation::Stop;
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
            }
        });

        let sg = suggestionbox.clone();
        search.entry.connect_activate(move |_| {
            launch_suggestion(&sg, selected_result(&sg));
        });
    }

    let display = Display::default().expect("no display");
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
            background-color: rgba(47, 47, 47, 0.49);
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
            border-radius: 25px;
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