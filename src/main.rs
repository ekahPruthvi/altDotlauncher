use async_channel::Sender;
use gtk4::gdk::Display;
use gtk4::gio::{self, AppInfo};
use gtk4::prelude::*;
use gtk4::{
    glib, pango, Application, ApplicationWindow, Button, CssProvider, Entry as GtkEntry, FlowBox, IconTheme, Image,
    Label, Overlay, Picture, ScrolledWindow, SelectionMode, Stack, StackTransitionType, Box as GtkBox,
    MediaFile, MediaStream, TextView, WrapMode,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use infoprober::{Document, Entry};
use rodio::{Decoder, OutputStream, Sink};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use std::process::exit;
use self_update::cargo_crate_version;

const POLL_SECS: u64 = 5;
const SAVE_EVERY_SECS: u64 = 60;
const SUGGEST_COUNT: usize = 5;
const SEARCH_RESULTS: usize = 3;
const LOGO_TEXT: &str = "alt●";
const SEARCH_GLYPH: &str = "";
const LAUNCH_ANIM_US: i64 = 300_000;

const PANEL_HEIGHT: i32 = 320;
const PAGE_MAX: usize = 8;
const FILE_DEPTH: usize = 5;
const FILE_SCAN_LIMIT: usize = 40_000;
const FILE_MIN_QUERY: usize = 2;
const CLIP_PREVIEW_WIDTH: &str = "400";
const CLIP_THUMB_CACHE: usize = 256;

const CLIP_ENABLED: bool = true;
const FILES_ENABLED: bool = true;
const CLIP_ALL_MAX: usize = 200;
const CLIP_IMG_MAX_BYTES: usize = 25_000_000;
const CUSTOM_ICON_DIR: &str = "/var/lib/cynager/icons";
const HIT_ICON_SIZE: i32 = 46;
const FAST_DEBOUNCE_MS: u64 = 70;
const MODE_DEBOUNCE_MS: u64 = 500;
const FILE_INDEX_TTL: Duration = Duration::from_secs(120);
const CLIP_LIST_TTL: Duration = Duration::from_millis(1500);
const FILE_SKIP_DIRS: [&str; 5] = ["node_modules", "target", "venv", "__pycache__", "site-packages"];

const TERMINALS: [(&str, &[&str]); 2] = [("kitty", &[]), ("xterm", &["-e"])];

const PILLS: [(&str, &str, &str); 0] = [
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

static SIG_PIPE_W: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIG_PIPE_W.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        unsafe {
            libc::write(fd, &b as *const u8 as *const libc::c_void, 1);
        }
    }
}

fn spawn_signal_listener() -> async_channel::Receiver<i32> {
    let (tx, rx) = async_channel::unbounded::<i32>();
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return rx;
    }
    let (rd, wr) = (fds[0], fds[1]);
    SIG_PIPE_W.store(wr, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGUSR1, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, on_signal as *const () as libc::sighandler_t);
    }
    std::thread::spawn(move || loop {
        let mut b = 0_u8;
        let n = unsafe { libc::read(rd, &mut b as *mut u8 as *mut libc::c_void, 1) };
        if n == 1 {
            if tx.send_blocking(b as i32).is_err() {
                break;
            }
        } else if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        } else {
            break;
        }
    });
    rx
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

    apps.sort_by_cached_key(|a| a.display_name().to_lowercase());
    apps
}

struct AppEntry {
    info: AppInfo,
    name: String,
    name_lc: Vec<char>,
    id_lc: Vec<char>,
    sort_key: String,
}

thread_local! {
    static APP_CACHE: RefCell<Option<Rc<Vec<AppInfo>>>> = RefCell::new(None);
    static APP_INDEX: RefCell<Option<Rc<Vec<AppEntry>>>> = RefCell::new(None);
    static GRID_STALE: Cell<bool> = Cell::new(true);
    static APP_MONITOR: RefCell<Option<gio::AppInfoMonitor>> = RefCell::new(None);
    static ICON_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn cached_apps() -> Rc<Vec<AppInfo>> {
    APP_CACHE.with(|c| c.borrow_mut().get_or_insert_with(|| Rc::new(load_apps())).clone())
}

fn seed_app_cache(apps: Rc<Vec<AppInfo>>) {
    APP_CACHE.with(|c| *c.borrow_mut() = Some(apps));
}

fn invalidate_apps() {
    APP_CACHE.with(|c| *c.borrow_mut() = None);
    APP_INDEX.with(|c| *c.borrow_mut() = None);
    GRID_STALE.with(|g| g.set(true));
}

fn take_grid_stale() -> bool {
    GRID_STALE.with(|g| g.replace(false))
}

fn watch_app_changes() {
    let monitor = gio::AppInfoMonitor::get();
    monitor.connect_changed(|_| invalidate_apps());
    APP_MONITOR.with(|m| *m.borrow_mut() = Some(monitor));
}

fn lower_into(buf: &mut Vec<char>, s: &str) {
    buf.clear();
    buf.extend(s.chars().flat_map(|c| c.to_lowercase()));
}

fn lower_chars(s: &str) -> Vec<char> {
    let mut v = Vec::with_capacity(s.len());
    lower_into(&mut v, s);
    v
}

fn app_index() -> Rc<Vec<AppEntry>> {
    if let Some(i) = APP_INDEX.with(|c| c.borrow().clone()) {
        return i;
    }
    let idx: Vec<AppEntry> = cached_apps()
        .iter()
        .map(|a| {
            let name = a.display_name().to_string();
            let id = a.id().map(|i| i.to_string()).unwrap_or_default();
            AppEntry {
                info: a.clone(),
                name_lc: lower_chars(&name),
                id_lc: lower_chars(id.trim_end_matches(".desktop")),
                sort_key: name.to_lowercase(),
                name,
            }
        })
        .collect();
    let rc = Rc::new(idx);
    APP_INDEX.with(|c| *c.borrow_mut() = Some(rc.clone()));
    rc
}

struct Matcher {
    q: Vec<char>,
    ql: Vec<char>,
    qn: i32,
}

impl Matcher {
    fn new(query: &str) -> Self {
        let ql = lower_chars(query.trim());
        let q: Vec<char> = ql.iter().copied().filter(|c| !c.is_whitespace()).collect();
        let qn = q.len() as i32;
        Self { q, ql, qn }
    }

    fn score(&self, t: &[char]) -> Option<i32> {
        if self.q.is_empty() {
            return Some(0);
        }
        if t.len() < self.q.len() {
            return None;
        }

        let mut score = 0_i32;
        let mut ti = 0_usize;
        let mut prev: Option<usize> = None;

        for &qc in &self.q {
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

        if !self.ql.is_empty() && t.windows(self.ql.len()).any(|w| w == self.ql.as_slice()) {
            score += 40;
            if t.starts_with(&self.ql) {
                score += 30;
            }
        }
        score -= t.len() as i32 / 8;
        Some(score)
    }

    fn relevant(&self, t: &[char]) -> Option<i32> {
        let s = self.score(t)?;
        if s >= 14 * self.qn { Some(s) } else { None }
    }
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

            let snapshot = {
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
                    u.dirty = false;
                    Some(u.slots.clone())
                } else {
                    None
                }
            };

            if let Some(slots) = snapshot {
                save_usage(&Usage { slots, alive: HashMap::new(), dirty: false });
                last_save = Instant::now();
            }
        }
    });
}

struct SuperState {
    down: AtomicBool,
    clean: AtomicBool,
}

fn watch_input_device(
    path: PathBuf,
    mut dev: evdev::Device,
    is_keyboard: bool,
    tx: Sender<()>,
    st: Arc<SuperState>,
    watched: Arc<Mutex<HashSet<PathBuf>>>,
) {
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

    std::thread::spawn(move || {
        loop {
            let events = match dev.fetch_events() {
                Ok(e) => e,
                Err(_) => break,
            };
            for ev in events {
                let (code, value) = (ev.code(), ev.value());
                if is_keyboard {
                    if ev.event_type() != EventType::KEY {
                        continue;
                    }
                    if code == left || code == right {
                        match value {
                            1 => {
                                st.down.store(true, Ordering::SeqCst);
                                st.clean.store(true, Ordering::SeqCst);
                            }
                            0 => {
                                if st.down.load(Ordering::SeqCst) && st.clean.load(Ordering::SeqCst) {
                                    let _ = tx.send_blocking(());
                                }
                                st.down.store(false, Ordering::SeqCst);
                            }
                            _ => {}
                        }
                    } else if value == 1 && st.down.load(Ordering::SeqCst) {
                        st.clean.store(false, Ordering::SeqCst);
                    }
                } else {
                    if !st.down.load(Ordering::SeqCst) {
                        continue;
                    }
                    let taints = match ev.event_type() {
                        EventType::KEY => value == 1 && code != touch && code != finger,
                        EventType::RELATIVE => wheel.contains(&code),
                        _ => false,
                    };
                    if taints {
                        st.clean.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
        if is_keyboard {
            st.down.store(false, Ordering::SeqCst);
            st.clean.store(false, Ordering::SeqCst);
        }
        if let Ok(mut w) = watched.lock() {
            w.remove(&path);
        }
    });
}

fn spawn_super_watchers(tx: Sender<()>) {
    use evdev::{KeyCode, RelativeAxisCode};

    let state = Arc::new(SuperState {
        down: AtomicBool::new(false),
        clean: AtomicBool::new(false),
    });
    let watched: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

    std::thread::spawn(move || loop {
        let mut present: HashSet<PathBuf> = HashSet::new();
        if let Ok(rd) = std::fs::read_dir("/dev/input") {
            for e in rd.flatten() {
                let path = e.path();
                let is_event = path
                    .file_name()
                    .map_or(false, |n| n.to_string_lossy().starts_with("event"));
                if !is_event {
                    continue;
                }
                present.insert(path.clone());

                if watched.lock().map_or(true, |w| w.contains(&path)) {
                    continue;
                }
                let Ok(dev) = evdev::Device::open(&path) else {
                    continue;
                };

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

                if let Ok(mut w) = watched.lock() {
                    w.insert(path.clone());
                }
                if is_keyboard || is_pointer {
                    watch_input_device(path, dev, is_keyboard, tx.clone(), state.clone(), watched.clone());
                }
            }
        }
        if let Ok(mut w) = watched.lock() {
            w.retain(|p| present.contains(p));
        }
        std::thread::sleep(Duration::from_secs(2));
    });
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
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn user_systemd_available() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        in_path("systemd-run")
            && std::env::var_os("XDG_RUNTIME_DIR")
                .map(|d| Path::new(&d).join("systemd/private").exists())
                .unwrap_or(false)
    })
}

fn spawn_survivor(argv: &[String], cwd: Option<PathBuf>) -> std::io::Result<()> {
    let mut cmd = if user_systemd_available() {
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--scope", "--collect", "--quiet", "--"]).args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    spawn_detached(&mut cmd)
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

fn terminal_app_argv(info: &AppInfo, file: Option<&Path>) -> Option<Vec<String>> {
    let kf = desktop_keyfile(info)?;
    let exec = kf.string("Desktop Entry", "Exec").ok()?.to_string();
    let path = file.map(|p| p.to_string_lossy().to_string());
    let uri = file.map(|p| gio::File::for_path(p).uri().to_string());

    let mut used = false;
    let mut argv: Vec<String> = Vec::new();
    for tok in split_exec(&exec) {
        match tok.as_str() {
            "%f" | "%F" => {
                if let Some(p) = &path {
                    argv.push(p.clone());
                    used = true;
                }
            }
            "%u" | "%U" => {
                if let Some(u) = &uri {
                    argv.push(u.clone());
                    used = true;
                }
            }
            t if t.len() == 2 && t.starts_with('%') && !t.starts_with("%%") => {}
            t => argv.push(t.replace("%%", "%")),
        }
    }
    if !used {
        if let Some(p) = path {
            argv.push(p);
        }
    }
    Some(argv)
}

fn run_app(info: &AppInfo, term: Option<&'static str>, file: Option<&Path>) -> Result<(), String> {
    match term {
        Some(t) => {
            let argv = terminal_app_argv(info, file).ok_or_else(|| "no Exec line".to_string())?;
            if argv.is_empty() {
                return Err("empty Exec line".to_string());
            }
            let flags: &[&str] = TERMINALS
                .iter()
                .find(|(n, _)| *n == t)
                .map(|(_, f)| *f)
                .unwrap_or(&[]);
            let mut full: Vec<String> = vec![t.to_string()];
            full.extend(flags.iter().map(|s| s.to_string()));
            full.extend(argv);
            spawn_survivor(&full, None).map_err(|e| e.to_string())
        }
        None => {
            if let Some(argv) = terminal_app_argv(info, file).filter(|a| !a.is_empty()) {
                let cwd = desktop_keyfile(info)
                    .and_then(|kf| kf.string("Desktop Entry", "Path").ok())
                    .map(|p| PathBuf::from(p.to_string()))
                    .filter(|p| p.is_dir());
                return spawn_survivor(&argv, cwd).map_err(|e| e.to_string());
            }
            let files: Vec<gio::File> = file.map(gio::File::for_path).into_iter().collect();
            info.launch(&files, None::<&gio::AppLaunchContext>)
                .map_err(|e| e.to_string())
        }
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

struct PickOpt {
    label: String,
    icon: Option<gio::Icon>,
    run: Rc<dyn Fn()>,
}

fn show_picker(window: &ApplicationWindow, title: &str, options: Vec<PickOpt>) {
    let Some(overlay) = window.child().and_downcast::<Overlay>() else {
        return;
    };
    if picker_widget(window).is_some() || options.is_empty() {
        return;
    }

    let picker = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .css_classes(["term-pick"])
        .build();

    let head = Label::new(Some(title));
    head.add_css_class("term-pick-title");
    head.set_margin_bottom(4);
    picker.append(&head);

    let mut first: Option<Button> = None;
    for (i, opt) in options.into_iter().enumerate() {
        let row = GtkBox::builder().orientation(gtk4::Orientation::Horizontal).spacing(10).build();
        if let Some(icon) = &opt.icon {
            let img = Image::from_gicon(icon);
            img.set_pixel_size(22);
            row.append(&img);
        }
        let text = Label::new(Some(&format!("{}  {}", i + 1, opt.label)));
        text.set_xalign(0.0);
        row.append(&text);

        let b = Button::builder().child(&row).hexpand(true).build();
        b.add_css_class("term-btn");

        let (overlay, weak, run) = (overlay.clone(), picker.downgrade(), opt.run.clone());
        b.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                overlay.remove_overlay(&p);
            }
            run();
        });
        picker.append(&b);
        if first.is_none() {
            first = Some(b);
        }
    }

    let hint = Label::new(Some("Esc to cancel"));
    hint.add_css_class("sugg-empty-sub");
    hint.set_margin_top(4);
    picker.append(&hint);

    overlay.add_overlay(&picker);
    if let Some(b) = first {
        b.grab_focus();
    }
}

fn ask_terminal(info: &AppInfo, source: &Button, window: &ApplicationWindow, terms: Vec<&'static str>) {
    if window.child().and_downcast::<Overlay>().is_none() {
        start_launch(info, source, window, terms.first().copied());
        return;
    }
    let options = terms
        .into_iter()
        .map(|t| {
            let (info, source, win) = (info.clone(), source.clone(), window.clone());
            PickOpt {
                label: t.to_string(),
                icon: None,
                run: Rc::new(move || start_launch(&info, &source, &win, Some(t))),
            }
        })
        .collect();
    show_picker(window, &format!("Open {} with", info.display_name()), options);
}

fn launch_file(info: &AppInfo, term: Option<&'static str>, path: &Path, window: &ApplicationWindow) {
    if let Err(e) = run_app(info, term, Some(path)) {
        eprintln!("Failed to open {} with {}: {}", path.display(), info.display_name(), e);
    }
    window.set_visible(false);
}

fn open_file_with(info: &AppInfo, path: &Path, window: &ApplicationWindow) {
    if is_terminal_app(info) {
        let terms = installed_terminals();
        if terms.len() > 1 {
            let options = terms
                .into_iter()
                .map(|t| {
                    let (info, path, win) = (info.clone(), path.to_path_buf(), window.clone());
                    PickOpt {
                        label: t.to_string(),
                        icon: None,
                        run: Rc::new(move || launch_file(&info, Some(t), &path, &win)),
                    }
                })
                .collect();
            show_picker(window, &format!("Open {} in", info.display_name()), options);
            return;
        }
        launch_file(info, terms.first().copied(), path, window);
        return;
    }
    launch_file(info, None, path, window);
}

const EDITOR_IDS: [&str; 14] = [
    "code",
    "cursor",
    "co.anysphere.cursor",
    "windsurf",
    "zed",
    "dev.zed.zed",
    "zeditor",
    "code-oss",
    "com.visualstudio.code",
    "visual-studio-code",
    "vscodium",
    "codium",
    "com.vscodium.codium",
    "sublime_text",
];

fn is_ide(a: &AppInfo) -> bool {
    desktop_keyfile(a)
        .and_then(|kf| kf.string("Desktop Entry", "Categories").ok())
        .map_or(false, |c| c.split(';').any(|t| t == "IDE"))
}

fn find_code_editors() -> Vec<AppInfo> {
    let all: Vec<AppInfo> = cached_apps().iter().cloned().collect();
    let key = |a: &AppInfo| {
        a.id()
            .map(|i| i.trim_end_matches(".desktop").to_lowercase())
            .unwrap_or_default()
    };

    let mut out: Vec<AppInfo> = Vec::new();
    for want in EDITOR_IDS {
        if let Some(a) = all.iter().find(|a| key(a) == want) {
            if !out.iter().any(|x| x.id() == a.id()) {
                out.push(a.clone());
            }
        }
    }
    for a in &all {
        if !out.iter().any(|x| x.id() == a.id()) && is_ide(a) {
            out.push(a.clone());
        }
    }
    out
}

fn open_file_dialog(window: &ApplicationWindow, path: &Path) {
    let file = gio::File::for_path(path);
    let ctype = file
        .query_info("standard::content-type", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .ok()
        .and_then(|i| i.content_type())
        .map(|c| c.to_string());

    let mut apps: Vec<AppInfo> = Vec::new();
    let mut default_id = None;
    if let Some(ct) = &ctype {
        if let Some(d) = AppInfo::default_for_type(ct, false) {
            default_id = d.id();
            apps.push(d);
        }
        for a in AppInfo::all_for_type(ct) {
            if !apps.iter().any(|x| x.id() == a.id()) {
                apps.push(a);
            }
        }
    }

    if ctype.as_deref() == Some("inode/directory") {
        let mut editors = find_code_editors();
        if !editors.is_empty() {
            apps.retain(|x| !editors.iter().any(|e| e.id() == x.id()));
            default_id = editors[0].id();
            editors.append(&mut apps);
            apps = editors;
        }
    }
    apps.truncate(9);

    match apps.len() {
        0 => {
            let _ = AppInfo::launch_default_for_uri(&file.uri(), None::<&gio::AppLaunchContext>);
            window.set_visible(false);
        }
        1 => open_file_with(&apps[0], path, window),
        _ => {
            let options = apps
                .into_iter()
                .map(|a| {
                    let mut label = a.display_name().to_string();
                    if a.id().is_some() && a.id() == default_id {
                        label.push_str("  (default)");
                    }
                    let (icon, path, win) = (a.icon(), path.to_path_buf(), window.clone());
                    PickOpt {
                        label,
                        icon,
                        run: Rc::new(move || open_file_with(&a, &path, &win)),
                    }
                })
                .collect();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            show_picker(window, &format!("Open {name} with"), options);
        }
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
        let _ = run_app(info, term, None);
        window.set_visible(false);
        return;
    };
    if overlay.has_css_class("launching") {
        return;
    }

    if let Err(e) = run_app(info, term, None) {
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

    suggbox.set_halign(gtk4::Align::Center);
    let ids = current_slot()
        .map(|slot| top_apps(usage, slot, SUGGEST_COUNT))
        .unwrap_or_default();
    let apps = cached_apps();

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
    clear_box(suggbox);
    suggbox.set_halign(gtk4::Align::Start);

    let index = app_index();
    let m = Matcher::new(query);

    let mut scored: Vec<(i32, usize)> = Vec::new();
    for (i, e) in index.iter().enumerate() {
        let by_name = m.score(&e.name_lc);
        let by_id = m.score(&e.id_lc);
        let s = match (by_name, by_id) {
            (Some(x), Some(y)) => x.max(y - 20),
            (Some(x), None) => x,
            (None, Some(y)) => y - 20,
            (None, None) => continue,
        };
        scored.push((s, i));
    }

    scored.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| index[a.1].sort_key.cmp(&index[b.1].sort_key))
            .then_with(|| a.1.cmp(&b.1))
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

    for (i, (_, ix)) in scored.into_iter().enumerate() {
        let entry = &index[ix];
        let info = entry.info.clone();
        let cell = GtkBox::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(0)
            .halign(gtk4::Align::Start)
            .hexpand(false)
            .build();

        let button = make_app_button(info, window);
        if i == 0 {
            cell.add_css_class("sugg-top");
        }
        cell.append(&button);

        let name = Label::new(Some(&entry.name));
        name.add_css_class("sugg-name");
        name.set_margin_end(25);
        name.set_halign(gtk4::Align::Center);
        cell.append(&name);

        row.append(&cell);
    }
    suggbox.append(&row);
}


// ───────────────────── mode panel: Files / Calc / Clipboard / Pills ─────────────────────

fn copy_text(text: &str) {
    if let Some(d) = Display::default() {
        d.clipboard().set_text(text);
    }
}

#[derive(Clone, Copy)]
struct Xf {
    m: f64,
    e: i64,
}

impl Xf {
    const ZERO: Xf = Xf { m: 0.0, e: 0 };

    fn norm(self) -> Option<Xf> {
        if !self.m.is_finite() {
            return None;
        }
        if self.m == 0.0 {
            return Some(Xf::ZERO);
        }
        let sh = self.m.abs().log10().floor() as i64;
        let mut m = self.m / 10f64.powi(sh as i32);
        let mut e = self.e + sh;
        if m.abs() >= 10.0 {
            m /= 10.0;
            e += 1;
        } else if m.abs() < 1.0 {
            m *= 10.0;
            e -= 1;
        }
        if !m.is_finite() {
            return None;
        }
        Some(Xf { m, e })
    }

    fn from_f64(v: f64) -> Option<Xf> {
        Xf { m: v, e: 0 }.norm()
    }

    fn to_f64(self) -> Option<f64> {
        if self.m == 0.0 {
            return Some(0.0);
        }
        if self.e > 308 || self.e < -308 {
            return None;
        }
        let v = self.m * 10f64.powi(self.e as i32);
        if v.is_finite() {
            Some(v)
        } else {
            None
        }
    }

    fn neg(self) -> Xf {
        Xf { m: -self.m, e: self.e }
    }

    fn add(self, b: Xf) -> Option<Xf> {
        if self.m == 0.0 {
            return Some(b);
        }
        if b.m == 0.0 {
            return Some(self);
        }
        let d = self.e - b.e;
        if d > 17 {
            return Some(self);
        }
        if d < -17 {
            return Some(b);
        }
        let (hi, lo) = if d >= 0 { (self, b) } else { (b, self) };
        let k = (hi.e - lo.e) as i32;
        Xf { m: hi.m + lo.m * 10f64.powi(-k), e: hi.e }.norm()
    }

    fn sub(self, b: Xf) -> Option<Xf> {
        self.add(b.neg())
    }

    fn mul(self, b: Xf) -> Option<Xf> {
        Xf { m: self.m * b.m, e: self.e + b.e }.norm()
    }

    fn div(self, b: Xf) -> Option<Xf> {
        if b.m == 0.0 {
            return None;
        }
        Xf { m: self.m / b.m, e: self.e - b.e }.norm()
    }

    fn log10abs(self) -> Option<f64> {
        if self.m == 0.0 {
            return None;
        }
        Some(self.m.abs().log10() + self.e as f64)
    }

    fn pow10(l: f64) -> Option<Xf> {
        if !l.is_finite() || l.abs() > 1e15 {
            return None;
        }
        let e = l.floor();
        Xf { m: 10f64.powf(l - e), e: e as i64 }.norm()
    }

    fn pow(self, b: Xf) -> Option<Xf> {
        let bf = b.to_f64()?;
        if self.m == 0.0 {
            return if bf > 0.0 { Some(Xf::ZERO) } else { None };
        }
        if let Some(af) = self.to_f64() {
            let r = af.powf(bf);
            if r.is_finite() && r != 0.0 {
                return Xf::from_f64(r);
            }
        }
        let neg = self.m < 0.0;
        if neg && bf.fract() != 0.0 {
            return None;
        }
        let mut r = Xf::pow10(self.log10abs()? * bf)?;
        if neg && (bf.abs() as u64) % 2 == 1 {
            r = r.neg();
        }
        Some(r)
    }

    fn sqrt(self) -> Option<Xf> {
        if self.m < 0.0 {
            return None;
        }
        if self.m == 0.0 {
            return Some(Xf::ZERO);
        }
        if self.e % 2 == 0 {
            Xf { m: self.m.sqrt(), e: self.e / 2 }.norm()
        } else {
            Xf { m: (self.m * 10.0).sqrt(), e: (self.e - 1) / 2 }.norm()
        }
    }
}

fn apply_fn(name: &str, a: Xf) -> Option<Xf> {
    match name {
        "sqrt" => a.sqrt(),
        "abs" => Some(Xf { m: a.m.abs(), e: a.e }),
        "ln" => {
            if a.m <= 0.0 {
                return None;
            }
            Xf::from_f64(a.log10abs()? * std::f64::consts::LN_10)
        }
        "log" => {
            if a.m <= 0.0 {
                return None;
            }
            Xf::from_f64(a.log10abs()?)
        }
        "exp" => Xf::pow10(a.to_f64()? * std::f64::consts::LOG10_E),
        "floor" | "ceil" | "round" => {
            if a.e >= 16 {
                return Some(a);
            }
            let v = a.to_f64()?;
            Xf::from_f64(match name {
                "floor" => v.floor(),
                "ceil" => v.ceil(),
                _ => v.round(),
            })
        }
        "sin" | "cos" | "tan" => {
            let v = a.to_f64()?;
            Xf::from_f64(match name {
                "sin" => v.sin(),
                "cos" => v.cos(),
                _ => v.tan(),
            })
        }
        _ => None,
    }
}

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

    fn expr(&mut self) -> Option<Xf> {
        let mut v = self.term()?;
        while let Some(c) = self.peek() {
            match c {
                '+' => {
                    self.i += 1;
                    self.ops += 1;
                    let r = self.term()?;
                    v = v.add(r)?;
                }
                '-' => {
                    self.i += 1;
                    self.ops += 1;
                    let r = self.term()?;
                    v = v.sub(r)?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn term(&mut self) -> Option<Xf> {
        let mut v = self.unary()?;
        while let Some(c) = self.peek() {
            match c {
                '*' | '×' => {
                    self.i += 1;
                    self.ops += 1;
                    let r = self.unary()?;
                    v = v.mul(r)?;
                }
                '/' | '÷' => {
                    self.i += 1;
                    self.ops += 1;
                    let r = self.unary()?;
                    v = v.div(r)?;
                }
                '%' => {
                    self.i += 1;
                    self.ops += 1;
                    let r = self.unary()?;
                    v = Xf::from_f64(v.to_f64()? % r.to_f64()?)?;
                }
                _ => break,
            }
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<Xf> {
        match self.peek()? {
            '-' => {
                self.i += 1;
                self.ops += 1;
                Some(self.unary()?.neg())
            }
            '+' => {
                self.i += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Option<Xf> {
        let base = self.atom()?;
        if self.peek() == Some('^') {
            self.i += 1;
            self.ops += 1;
            let e = self.unary()?;
            return base.pow(e);
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<Xf> {
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
            return Xf::from_f64(t.parse::<f64>().ok()?);
        }
        if c.is_alphabetic() {
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i].is_alphabetic() {
                self.i += 1;
            }
            let name: String = self.s[st..self.i].iter().collect::<String>().to_lowercase();
            match name.as_str() {
                "pi" => return Xf::from_f64(std::f64::consts::PI),
                "e" => return Xf::from_f64(std::f64::consts::E),
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
            return apply_fn(&name, a);
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

fn fmt_x(x: Xf) -> String {
    if x.m == 0.0 {
        return "0".to_string();
    }
    if let Some(v) = x.to_f64() {
        if v.abs() >= 1e-6 && v.abs() < 1e15 {
            return fmt_num(v);
        }
    }
    let mut m = (x.m * 1e11).round() / 1e11;
    let mut e = x.e;
    if m.abs() >= 10.0 {
        m /= 10.0;
        e += 1;
    }
    let ms = format!("{:.11}", m);
    let ms = ms.trim_end_matches('0').trim_end_matches('.');
    format!("{ms}e{e}")
}

fn calc(q: &str) -> Option<String> {
    let mut q = q.trim().trim_start_matches('=').trim_start();
    if let Some(rest) = q.strip_prefix(|c: char| c == 'c' || c == 'C') {
        let t = rest.trim_start();
        if matches!(t.chars().next(), Some(ch) if ch.is_ascii_digit() || matches!(ch, '(' | '.' | '-' | '+')) {
            q = t;
        }
    }
    let mut p = CalcParser { s: q.chars().collect(), i: 0, ops: 0 };
    let v = p.expr()?;
    if p.peek().is_some() || p.ops == 0 {
        return None;
    }
    Some(fmt_x(v))
}

struct FileEnt {
    name_lc: Box<[char]>,
    path: PathBuf,
}

struct FileIndex {
    ents: Vec<FileEnt>,
    built: Instant,
}

static FILE_INDEX: RwLock<Option<Arc<FileIndex>>> = RwLock::new(None);
static FILE_INDEX_BUSY: AtomicBool = AtomicBool::new(false);

fn lower_priority() {
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

fn build_file_index() -> FileIndex {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((home, 0));
    let mut ents: Vec<FileEnt> = Vec::new();
    let mut visited = 0_usize;

    'walk: while let Some((dir, depth)) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            visited += 1;
            if visited > FILE_SCAN_LIMIT {
                break 'walk;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let path = e.path();
            if ft.is_dir() && depth < FILE_DEPTH && !FILE_SKIP_DIRS.contains(&&*name) {
                queue.push_back((path.clone(), depth + 1));
            }
            ents.push(FileEnt {
                name_lc: lower_chars(&name).into_boxed_slice(),
                path,
            });
        }
    }
    FileIndex { ents, built: Instant::now() }
}

fn refresh_file_index(force: bool) {
    if !FILES_ENABLED {
        return;
    }
    if FILE_INDEX_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    let fresh = !force
        && FILE_INDEX
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|i| i.built.elapsed() < FILE_INDEX_TTL))
            .unwrap_or(false);
    if fresh {
        FILE_INDEX_BUSY.store(false, Ordering::SeqCst);
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("altdot-file-index".to_string())
        .spawn(|| {
            lower_priority();
            let idx = Arc::new(build_file_index());
            if let Ok(mut g) = FILE_INDEX.write() {
                *g = Some(idx);
            }
            FILE_INDEX_BUSY.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        FILE_INDEX_BUSY.store(false, Ordering::SeqCst);
    }
}

fn search_files(q: &str, generation: &AtomicU64, mine: u64) -> Vec<PathBuf> {
    if !FILES_ENABLED {
        return Vec::new();
    }
    let idx = FILE_INDEX.read().ok().and_then(|g| (*g).clone());
    let Some(idx) = idx else {
        refresh_file_index(true);
        return walk_search_files(q, generation, mine);
    };

    let m = Matcher::new(q);
    let mut best: Vec<(i32, &FileEnt)> = Vec::new();
    for (i, e) in idx.ents.iter().enumerate() {
        if i & 0x1fff == 0 && generation.load(Ordering::Relaxed) != mine {
            return Vec::new();
        }
        if let Some(s) = m.relevant(&e.name_lc) {
            best.push((s, e));
            if best.len() > 400 {
                best.sort_by(|a, b| b.0.cmp(&a.0));
                best.truncate(PAGE_MAX * 4);
            }
        }
    }
    best.sort_by(|a, b| b.0.cmp(&a.0));
    best.truncate(PAGE_MAX);
    best.into_iter().map(|(_, e)| e.path.clone()).collect()
}

fn walk_search_files(q: &str, generation: &AtomicU64, mine: u64) -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((home, 0));
    let m = Matcher::new(q);
    let mut buf: Vec<char> = Vec::new();
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
            if visited % 128 == 0 && generation.load(Ordering::Relaxed) != mine {
                return Vec::new();
            }
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
            lower_into(&mut buf, &name);
            if let Some(s) = m.relevant(&buf) {
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

fn custom_icon(file: &str, fallback: &str) -> String {
    ICON_CACHE.with(|c| {
        c.borrow_mut()
            .entry(file.to_string())
            .or_insert_with(|| {
                let p = Path::new(CUSTOM_ICON_DIR).join(file);
                if p.is_file() {
                    p.to_string_lossy().to_string()
                } else {
                    fallback.to_string()
                }
            })
            .clone()
    })
}

#[derive(Clone)]
struct Hit {
    title: String,
    subtitle: String,
    icon: String,
    thumb: Option<gtk4::gdk::Texture>,
    clip_line: Option<String>,
    path: Option<PathBuf>,
    action: Rc<dyn Fn()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    Files,
    Calc,
    Clip,
    Pills,
}

impl Cat {
    const ALL: [Cat; 4] = [Cat::Files, Cat::Calc, Cat::Clip, Cat::Pills];
    const HOME: [Cat; 4] = [Cat::Calc, Cat::Files, Cat::Clip, Cat::Pills];

    fn idx(self) -> usize {
        self as usize
    }

    fn id(self) -> &'static str {
        match self {
            Cat::Files => "files",
            Cat::Calc => "calc",
            Cat::Clip => "clip",
            Cat::Pills => "pills",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Cat::Files => "Files",
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

fn file_hits(paths: Vec<PathBuf>, window: &ApplicationWindow) -> Vec<Hit> {
    paths
        .into_iter()
        .map(|p| {
            let win = window.clone();
            let target = p.clone();
            Hit {
                title: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                subtitle: pretty_dir(&p),
                icon: if p.is_dir() { "folder" } else { "text-x-generic" }.to_string(),
                thumb: None,
                clip_line: None,
                path: Some(p.clone()),
                action: Rc::new(move || open_file_dialog(&win, &target)),
            }
        })
        .collect()
}

fn thumb_pixbuf(bytes: Vec<u8>) -> Option<gtk4::gdk_pixbuf::Pixbuf> {
    use gtk4::gdk_pixbuf::{InterpType, Pixbuf};
    const TW: i32 = 128;
    const TH: i32 = 88;

    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
    let pb = Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).ok()?;
    let (w, h) = (pb.width().max(1), pb.height().max(1));

    let scale = (TW as f64 / w as f64).max(TH as f64 / h as f64);
    let sw = ((w as f64 * scale).ceil() as i32).max(TW);
    let sh = ((h as f64 * scale).ceil() as i32).max(TH);
    let scaled = pb.scale_simple(sw, sh, InterpType::Bilinear)?;
    Some(scaled.new_subpixbuf((sw - TW) / 2, (sh - TH) / 2, TW, TH))
}

#[derive(Clone)]
struct ThumbPixels {
    data: Vec<u8>,
    w: i32,
    h: i32,
    stride: usize,
    alpha: bool,
}

impl ThumbPixels {
    fn into_texture(self) -> gtk4::gdk::Texture {
        let fmt = if self.alpha {
            gtk4::gdk::MemoryFormat::R8g8b8a8
        } else {
            gtk4::gdk::MemoryFormat::R8g8b8
        };
        let (w, h, stride) = (self.w, self.h, self.stride);
        gtk4::gdk::MemoryTexture::new(w, h, fmt, &glib::Bytes::from_owned(self.data), stride)
            .upcast::<gtk4::gdk::Texture>()
    }
}

fn thumb_pixels(bytes: Vec<u8>) -> Option<ThumbPixels> {
    let flat = thumb_pixbuf(bytes)?.copy()?;
    Some(ThumbPixels {
        w: flat.width(),
        h: flat.height(),
        stride: flat.rowstride() as usize,
        alpha: flat.has_alpha(),
        data: flat.read_pixel_bytes().to_vec(),
    })
}

fn calc_hits(q: &str) -> Vec<Hit> {
    match calc(q) {
        Some(res) => vec![Hit {
            title: format!("= {res}"),
            subtitle: "Enter to copy the result".to_string(),
            icon: custom_icon("calc.svg", "accessories-calculator"),
            thumb: None,
            clip_line: None,
            path: None,
            action: Rc::new(move || copy_text(&res)),
        }],
        None => Vec::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipKind {
    Text,
    Image,
    Files,
    Clear,
}

#[derive(Clone)]
enum ThumbJob {
    Clip(u64),
    File(PathBuf),
}

struct ClipItem {
    line: String,
    kind: ClipKind,
    title: String,
    subtitle: String,
    thumb: Option<ThumbPixels>,
    job: Option<ThumbJob>,
}

enum ClipMsg {
    Items(Result<Vec<ClipItem>, String>),
    Thumbs(Vec<(String, ThumbPixels)>),
}

struct ClipEntry {
    id: u64,
    prev: String,
    line: String,
    hay: String,
}

static CLIP_LIST_CACHE: Mutex<Option<(Instant, Arc<Vec<ClipEntry>>)>> = Mutex::new(None);

fn clip_cache_clear() {
    if let Ok(mut g) = CLIP_LIST_CACHE.lock() {
        *g = None;
    }
}

static CLIP_THUMBS: std::sync::OnceLock<Mutex<HashMap<u64, ThumbPixels>>> = std::sync::OnceLock::new();

fn ensure_cliphist_watchers() {
    if !CLIP_ENABLED {
        return;
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if !(in_path("cliphist") && in_path("wl-paste")) {
            return;
        }
        let running = Command::new("pgrep")
            .args(["-f", "wl-paste.*cliphist store"])
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if running {
            return;
        }
        let mut c = Command::new("wl-paste");
        c.args(["--watch", "cliphist", "store"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = spawn_detached(&mut c);
    });
}

fn cliphist_list() -> std::io::Result<Vec<ClipEntry>> {
    let out = Command::new("cliphist")
        .args(["-preview-width", CLIP_PREVIEW_WIDTH, "list"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| {
            let (id, prev) = l.split_once('\t')?;
            let id: u64 = id.trim().parse().ok()?;
            let hay = (if parse_binary_preview(prev).is_some() {
                format!("image img picture screenshot {prev}")
            } else if looks_like_files(prev) {
                format!("file files {}", percent_decode(prev))
            } else {
                prev.to_string()
            })
            .to_lowercase();
            Some(ClipEntry { id, prev: prev.to_string(), line: l.to_string(), hay })
        })
        .collect())
}

fn cliphist_list_cached() -> std::io::Result<Arc<Vec<ClipEntry>>> {
    if let Ok(g) = CLIP_LIST_CACHE.lock() {
        if let Some((at, list)) = g.as_ref() {
            if at.elapsed() < CLIP_LIST_TTL {
                return Ok(list.clone());
            }
        }
    }
    let list = Arc::new(cliphist_list()?);
    if let Ok(mut g) = CLIP_LIST_CACHE.lock() {
        *g = Some((Instant::now(), list.clone()));
    }
    Ok(list)
}

fn cliphist_decode(line: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("cliphist")
        .arg("decode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(line.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_binary_preview(p: &str) -> Option<String> {
    let inner = p.trim().strip_prefix("[[")?.strip_suffix("]]")?.trim();
    Some(inner.strip_prefix("binary data")?.trim().to_string())
}

fn looks_like_files(p: &str) -> bool {
    let t = p.trim_start();
    t.starts_with("file://") || t.starts_with("x-special/gnome-copied-files")
}

fn parse_file_uris(text: &str) -> Vec<PathBuf> {
    text.split_whitespace()
        .filter_map(|t| t.strip_prefix("file://"))
        .map(|p| PathBuf::from(percent_decode(p)))
        .collect()
}

fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff")
    )
}

fn clip_thumb_cached(id: u64) -> Option<ThumbPixels> {
    let cache = CLIP_THUMBS.get_or_init(|| Mutex::new(HashMap::new()));
    cache.lock().ok()?.get(&id).cloned()
}

fn clip_thumb(id: u64, line: &str) -> Option<ThumbPixels> {
    if let Some(t) = clip_thumb_cached(id) {
        return Some(t);
    }
    let bytes = cliphist_decode(line)?;
    if bytes.len() > CLIP_IMG_MAX_BYTES {
        return None;
    }
    let t = thumb_pixels(bytes)?;
    let cache = CLIP_THUMBS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut c) = cache.lock() {
        if c.len() >= CLIP_THUMB_CACHE {
            c.clear();
        }
        c.insert(id, t.clone());
    }
    Some(t)
}

fn run_thumb_job(job: &ThumbJob, line: &str) -> Option<ThumbPixels> {
    match job {
        ThumbJob::Clip(id) => clip_thumb(*id, line),
        ThumbJob::File(p) => {
            let meta = std::fs::metadata(p).ok()?;
            if meta.len() as usize > CLIP_IMG_MAX_BYTES {
                return None;
            }
            thumb_pixels(std::fs::read(p).ok()?)
        }
    }
}

fn build_clip_item(id: u64, prev: &str, line: String) -> ClipItem {
    if let Some(info) = parse_binary_preview(prev) {
        let parts: Vec<&str> = info.split_whitespace().collect();
        let (title, subtitle) = if parts.len() >= 4 {
            (
                format!("Image · {} {}", parts[2], parts[3]),
                format!("{} {} · Enter to select · Del to delete", parts[0], parts[1]),
            )
        } else {
            ("Image".to_string(), format!("{info} · Enter to select · Del to delete"))
        };
        let (thumb, job) = match clip_thumb_cached(id) {
            Some(t) => (Some(t), None),
            None => (None, Some(ThumbJob::Clip(id))),
        };
        return ClipItem { line, kind: ClipKind::Image, title, subtitle, thumb, job };
    }

    if looks_like_files(prev) {
        let full = cliphist_decode(&line)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|| prev.to_string());
        let paths = parse_file_uris(&full);
        if let Some(first) = paths.first() {
            let name = first.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let (title, subtitle) = if paths.len() == 1 {
                (name, format!("{} · Enter to select · Del to delete", pretty_dir(first)))
            } else {
                (
                    format!("{name} +{} more", paths.len() - 1),
                    format!("{} files · {}", paths.len(), pretty_dir(first)),
                )
            };
            let job = (paths.len() == 1 && is_image_path(first))
                .then(|| ThumbJob::File(first.clone()));
            return ClipItem { line, kind: ClipKind::Files, title, subtitle, thumb: None, job };
        }
    }

    ClipItem {
        line,
        kind: ClipKind::Text,
        title: prev.trim().to_string(),
        subtitle: "Clipboard · Enter to select · Del to delete".to_string(),
        thumb: None,
        job: None,
    }
}

fn clear_clip() {
    std::thread::spawn(|| {
        let _ = Command::new("cliphist")
            .arg("wipe")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        clip_cache_clear();
        if let Some(cache) = CLIP_THUMBS.get() {
            if let Ok(mut c) = cache.lock() {
                c.clear();
            }
        }
    });
}

fn is_clip_query(q: &str) -> bool {
    matches!(
        q.split_whitespace().next().map(|w| w.to_lowercase()).as_deref(),
        Some("clip" | "clipboard")
    )
}

fn page_limit(c: Cat) -> usize {
    if c == Cat::Clip { CLIP_ALL_MAX } else { PAGE_MAX }
}

fn cliphist_delete(line: &str) {
    use std::io::Write;
    use std::process::Stdio;
    let Ok(mut child) = Command::new("cliphist")
        .arg("delete")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(line.as_bytes());
    }
    let _ = child.wait();
    clip_cache_clear();
}

fn search_clip(q: &str, generation: &AtomicU64, mine: u64) -> Result<Vec<ClipItem>, String> {
    if !CLIP_ENABLED {
        return Ok(Vec::new());
    }
    let list_all = is_clip_query(q);
    let mut words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
    if list_all {
        words.remove(0);
    }
    if words.is_empty() && !list_all {
        return Ok(Vec::new());
    }
    let limit = if list_all { CLIP_ALL_MAX } else { PAGE_MAX };

    let entries = match cliphist_list_cached() {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            static WARN: std::sync::Once = std::sync::Once::new();
            WARN.call_once(|| eprintln!("cliphist not found; clipboard history disabled"));
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.to_string()),
    };

    if list_all && words.len() == 1 && matches!(words[0].as_str(), "clear" | "wipe") {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![ClipItem {
            line: String::new(),
            kind: ClipKind::Clear,
            title: "Clear clipboard history".to_string(),
            subtitle: format!("Enter to delete all {} entries", entries.len()),
            thumb: None,
            job: None,
        }]);
    }

    let mut items = Vec::new();
    for (n, e) in entries.iter().enumerate() {
        if items.len() >= limit {
            break;
        }
        if n % 64 == 0 && generation.load(Ordering::SeqCst) != mine {
            return Ok(Vec::new());
        }
        if words.iter().all(|w| e.hay.contains(w.as_str())) {
            items.push(build_clip_item(e.id, &e.prev, e.line.clone()));
            if generation.load(Ordering::SeqCst) != mine {
                return Ok(Vec::new());
            }
        }
    }
    Ok(items)
}

fn clip_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.len() > 14 && b.starts_with(b"BM") && b[6..10] == [0, 0, 0, 0] {
        Some("image/bmp")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"x-special/gnome-copied-files") {
        Some("x-special/gnome-copied-files")
    } else if b.starts_with(b"file://") {
        Some("text/uri-list")
    } else {
        None
    }
}

fn restore_clip(line: &str) {
    let line = line.to_string();
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::Stdio;
        let Some(bytes) = cliphist_decode(&line) else { return };
        let mut cmd = Command::new("wl-copy");
        if let Some(m) = clip_mime(&bytes) {
            cmd.arg("--type").arg(m);
        }
        let Ok(mut child) = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(&bytes);
        }
        let _ = child.wait();
    });
}

fn clip_hits(res: Result<Vec<ClipItem>, String>) -> Vec<Hit> {
    match res {
        Err(msg) => vec![Hit {
            title: "Clipboard history unavailable".to_string(),
            subtitle: msg,
            icon: "dialog-warning".to_string(),
            thumb: None,
            clip_line: None,
            path: None,
            action: Rc::new(|| {}),
        }],
        Ok(items) => items
            .into_iter()
            .map(|it| {
                let clear = it.kind == ClipKind::Clear;
                let line = it.line;
                let action: Rc<dyn Fn()> = if clear {
                    Rc::new(clear_clip)
                } else {
                    let l = line.clone();
                    Rc::new(move || restore_clip(&l))
                };
                Hit {
                    title: it.title,
                    subtitle: it.subtitle,
                    icon: match it.kind {
                        ClipKind::Text => custom_icon("clip.svg", "edit-paste"),
                        ClipKind::Image => "image-x-generic".to_string(),
                        ClipKind::Files => "text-x-generic".to_string(),
                        ClipKind::Clear => "edit-delete".to_string(),
                    },
                    thumb: it.thumb.map(|t| t.into_texture()),
                    clip_line: if clear { None } else { Some(line) },
                    path: None,
                    action,
                }
            })
            .collect(),
    }
}

fn pill_hits(q: &str) -> Vec<Hit> {
    let m = Matcher::new(q);
    let mut v: Vec<(i32, Hit)> = PILLS
        .iter()
        .filter_map(|(name, icon, cmd)| {
            let s = m.relevant(&lower_chars(name))?;
            let cmd = cmd.to_string();
            Some((
                s,
                Hit {
                    title: name.to_string(),
                    subtitle: cmd.clone(),
                    icon: icon.to_string(),
                    thumb: None,
                    clip_line: None,
                    path: None,
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

fn thumb_picture(tex: &gtk4::gdk::Texture) -> Picture {
    let pic = Picture::for_paintable(tex);
    pic.set_can_shrink(true);
    pic.set_size_request(64, 44);
    pic.set_valign(gtk4::Align::Center);
    pic.add_css_class("hit-thumb");
    pic.set_overflow(gtk4::Overflow::Hidden);
    pic
}

fn set_row_thumb(row: &gtk4::Widget, tex: &gtk4::gdk::Texture) {
    let Some(btn) = row.downcast_ref::<Button>() else {
        return;
    };
    let Some(inner) = btn.child().and_then(|c| c.downcast::<GtkBox>().ok()) else {
        return;
    };
    if let Some(old) = inner.first_child() {
        inner.remove(&old);
    }
    inner.prepend(&thumb_picture(tex));
}

fn hit_button(hit: &Hit, window: &ApplicationWindow) -> Button {
    let lead: gtk4::Widget = match &hit.thumb {
        Some(tex) => thumb_picture(tex).upcast(),
        None => {
            let icon = if hit.icon.starts_with('/') {
                let file = gio::File::for_path(&hit.icon);
                Image::from_gicon(&gio::FileIcon::new(&file))
            } else {
                Image::from_icon_name(&hit.icon)
            };
            icon.set_pixel_size(HIT_ICON_SIZE);
            icon.upcast()
        }
    };

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
    row.append(&lead);
    row.append(&text);

    let button = Button::builder().child(&row).hexpand(true).build();
    button.add_css_class("flat");
    button.add_css_class("hit-row");

    let action = hit.action.clone();
    let win = window.clone();
    button.connect_clicked(move |_| {
        action();
        if picker_widget(&win).is_none() {
            win.set_visible(false);
        }
    });
    button
}

fn help_chip(text: &str) -> Label {
    let l = Label::new(Some(text));
    l.add_css_class("help-chip");
    l
}

fn help_chips(items: &[&str]) -> FlowBox {
    let fb = FlowBox::builder()
        .selection_mode(SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(1)
        .max_children_per_line(40)
        .row_spacing(6)
        .column_spacing(6)
        .hexpand(true)
        .valign(gtk4::Align::Center)
        .css_classes(["help-chips"])
        .build();
    for i in items {
        fb.insert(&help_chip(i), -1);
    }
    fb
}

fn help_text(t: &str) -> Label {
    let l = Label::new(Some(t));
    l.add_css_class("help-sub");
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_hexpand(true);
    l
}

fn help_row(glyph: &str, tile_class: &str, label: &str, content: &impl IsA<gtk4::Widget>) -> GtkBox {
    let tile = Label::new(Some(glyph));
    tile.add_css_class("help-tile");
    tile.add_css_class(tile_class);
    tile.set_valign(gtk4::Align::Center);

    let name = Label::new(Some(label));
    name.add_css_class("help-label");
    name.set_xalign(0.0);
    name.set_width_chars(13);

    let row = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    row.append(&tile);
    row.append(&name);
    row.append(content);
    row
}

fn calc_help() -> GtkBox {
    let root = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(14)
        .margin_top(4)
        .build();

    let title = Label::new(Some("Calculator"));
    title.add_css_class("help-title");
    title.set_xalign(0.0);
    let sub = Label::new(Some("Type c or = followed by the expression"));
    sub.add_css_class("help-sub");
    sub.set_xalign(0.0);
    let titles = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(2)
        .valign(gtk4::Align::Center)
        .build();
    titles.append(&title);
    titles.append(&sub);
    let header = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_start(2)
        .build();
    header.append(&titles);
    root.append(&header);

    let example = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .valign(gtk4::Align::Center)
        .build();
    example.append(&help_chip("c23+53*(2334^872)"));
    example.append(&help_text("→  5.1076e2938"));

    let rows: Vec<GtkBox> = vec![
        help_row("≈", "tile-teal", "Example", &example),
        help_row("c", "tile-blue", "Start with", &help_chips(&["c", "="])),
        help_row("±", "tile-orange", "Operators", &help_chips(&["+", "-", "*", "/", "%", "^", "( )"])),
        help_row("π", "tile-purple", "Constants", &help_chips(&["pi", "e"])),
        help_row(
            "ƒ",
            "tile-green",
            "Functions",
            &help_chips(&["sqrt", "sin", "cos", "tan", "ln", "log", "exp", "abs", "floor", "ceil", "round"]),
        ),
    ];

    let group = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["help-group"])
        .build();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            let div = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            div.add_css_class("help-div");
            group.append(&div);
        }
        group.append(row);
    }
    root.append(&group);
    root
}

fn help_panel(glyph: &str, title: &str, sub: &str, rows: Vec<GtkBox>) -> GtkBox {
    let root = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(14)
        .margin_top(4)
        .build();

    let title = Label::new(Some(title));
    title.add_css_class("help-title");
    title.set_xalign(0.0);
    let sub = Label::new(Some(sub));
    sub.add_css_class("help-sub");
    sub.set_xalign(0.0);
    let titles = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(2)
        .valign(gtk4::Align::Center)
        .build();
    titles.append(&title);
    titles.append(&sub);
    let header = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_start(2)
        .build();
    header.append(&titles);
    root.append(&header);

    let group = GtkBox::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["help-group"])
        .build();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            let div = gtk4::Separator::new(gtk4::Orientation::Horizontal);
            div.add_css_class("help-div");
            group.append(&div);
        }
        group.append(row);
    }
    root.append(&group);
    root
}

fn clip_help() -> GtkBox {
    let clear = GtkBox::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .hexpand(true)
        .valign(gtk4::Align::Center)
        .build();
    clear.append(&help_chip("clip clear"));
    clear.append(&help_text("then Enter wipes the whole history"));

    help_panel(
        "⧉",
        "Clipboard",
        "Type clip to browse everything you copied",
        vec![
            help_row("⧉", "tile-blue", "Search", &help_chips(&["clip", "clipboard", "TEXT"])),
            help_row("⌫", "tile-orange", "Delete", &help_text("Select an entry with ↑ ↓, then press Del")),
            help_row("✕", "tile-red", "Clear all", &clear),
            help_row("↵", "tile-green", "Enter", &help_text("Selects in clipboard: text, images and files")),
            help_row("i", "tile-gray", "Dependencies", &help_chips(&["cliphist", "wl-clipboard"])),
        ],
    )
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

const PREVIEW_W: i32 = 380;
const PREVIEW_TEXT_BYTES: usize = 16 * 1024;
const PREVIEW_BOX_W: i32 = 356;
const PREVIEW_BOX_H: i32 = 246;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewKind {
    Text,
    Image,
    Audio,
    Video,
    Pdf,
    Other,
}

impl PreviewKind {
    fn label(self) -> &'static str {
        match self {
            PreviewKind::Text => "Text",
            PreviewKind::Image => "Image",
            PreviewKind::Audio => "Audio",
            PreviewKind::Video => "Video",
            PreviewKind::Pdf => "PDF",
            PreviewKind::Other => "File",
        }
    }
}

fn looks_like_text(p: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = Vec::with_capacity(4096);
    if f.take(4096).read_to_end(&mut buf).is_err() {
        return false;
    }
    if buf.contains(&0) {
        return false;
    }
    String::from_utf8_lossy(&buf).chars().filter(|c| *c == '\u{FFFD}').count() <= 2
}

fn preview_kind(p: &Path) -> PreviewKind {
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tif" | "tiff" | "ico" | "avif" => {
            PreviewKind::Image
        }
        "mp3" | "wav" | "ogg" | "oga" | "flac" | "m4a" | "aac" | "opus" | "wma" => PreviewKind::Audio,
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "ogv" => PreviewKind::Video,
        "pdf" => PreviewKind::Pdf,
        _ => {
            if looks_like_text(p) {
                PreviewKind::Text
            } else {
                PreviewKind::Other
            }
        }
    }
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn read_text_preview(p: &Path) -> String {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(p) else {
        return "Can't read this file".to_string();
    };
    let mut buf = Vec::with_capacity(PREVIEW_TEXT_BYTES);
    if f.take(PREVIEW_TEXT_BYTES as u64 + 1).read_to_end(&mut buf).is_err() {
        return "Can't read this file".to_string();
    }
    let truncated = buf.len() > PREVIEW_TEXT_BYTES;
    buf.truncate(PREVIEW_TEXT_BYTES);
    let mut text = String::from_utf8_lossy(&buf).to_string();
    if text.is_empty() {
        return "(empty file)".to_string();
    }
    if truncated {
        text.push_str("\n\n…");
    }
    text
}

fn pixbuf_to_thumb(pb: gtk4::gdk_pixbuf::Pixbuf) -> Option<ThumbPixels> {
    let flat = pb.copy()?;
    Some(ThumbPixels {
        w: flat.width(),
        h: flat.height(),
        stride: flat.rowstride() as usize,
        alpha: flat.has_alpha(),
        data: flat.read_pixel_bytes().to_vec(),
    })
}

fn load_scaled(p: &Path, w: i32, h: i32) -> Option<ThumbPixels> {
    let pb = gtk4::gdk_pixbuf::Pixbuf::from_file_at_scale(p, w, h, true).ok()?;
    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
    pixbuf_to_thumb(pb)
}

fn load_cover(p: &Path, w: i32, h: i32) -> Option<ThumbPixels> {
    use gtk4::gdk_pixbuf::Pixbuf;
    let Some((_, iw, ih)) = Pixbuf::file_info(p) else {
        return load_scaled(p, w, h);
    };
    let (iw, ih) = (iw.max(1), ih.max(1));
    let scale = (w as f64 / iw as f64).max(h as f64 / ih as f64);
    let sw = ((iw as f64 * scale).ceil() as i32).max(w);
    let sh = ((ih as f64 * scale).ceil() as i32).max(h);
    let pb = Pixbuf::from_file_at_scale(p, sw, sh, false).ok()?;
    let pb = pb.apply_embedded_orientation().unwrap_or(pb);
    let (pw, ph) = (pb.width(), pb.height());
    let (cw, ch) = (w.min(pw), h.min(ph));
    let sub = pb.new_subpixbuf((pw - cw) / 2, (ph - ch) / 2, cw, ch);
    pixbuf_to_thumb(sub)
}

fn render_pdf_first_page(p: &Path, id: u64) -> Result<ThumbPixels, String> {
    let base = std::env::temp_dir().join(format!("altdot-pdf-{}-{}", std::process::id(), id));
    let png = base.with_extension("png");
    let status = Command::new("pdftoppm")
        .args(["-png", "-singlefile", "-f", "1", "-l", "1", "-r", "90"])
        .arg(p)
        .arg(&base)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let ok = match status {
        Ok(s) => s.success(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("Install poppler-utils (pdftoppm) to preview PDFs".to_string());
        }
        Err(_) => false,
    };
    let res = if ok {
        load_cover(&png, PREVIEW_BOX_W, PREVIEW_BOX_H)
    } else {
        None
    };
    let _ = std::fs::remove_file(&png);
    res.ok_or_else(|| "Can't render this PDF".to_string())
}

struct PreviewPane {
    root: GtkBox,
    meta: Label,
    content: GtkBox,
    media: RefCell<Option<MediaStream>>,
    audio: RefCell<Option<(OutputStream, Sink)>>,
    current: RefCell<Option<PathBuf>>,
    gen: Cell<u64>,
}

impl PreviewPane {
    fn new() -> Rc<Self> {
        let root = GtkBox::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(6)
            .margin_top(10)
            .margin_end(20)
            .margin_bottom(14)
            .css_classes(["preview-pane"])
            .visible(false)
            .build();
        root.set_size_request(PREVIEW_W, -1);

        let meta = Label::new(None);
        meta.add_css_class("preview-meta");
        meta.set_xalign(0.0);

        let content = GtkBox::builder()
            .orientation(gtk4::Orientation::Vertical)
            .vexpand(false)
            .hexpand(false)
            .overflow(gtk4::Overflow::Hidden)
            .valign(gtk4::Align::Start)
            .build();
        content.set_size_request(PREVIEW_BOX_W, PREVIEW_BOX_H);

        root.append(&meta);
        root.append(&content);

        Rc::new(Self {
            root,
            meta,
            content,
            media: RefCell::new(None),
            audio: RefCell::new(None),
            current: RefCell::new(None),
            gen: Cell::new(0),
        })
    }

    fn stop_media(&self) {
        if let Some(m) = self.media.borrow_mut().take() {
            m.pause();
            if let Some(mf) = m.downcast_ref::<MediaFile>() {
                mf.clear();
            }
        }
        if let Some((_stream, sink)) = self.audio.borrow_mut().take() {
            sink.stop();
        }
    }

    fn clear(&self) {
        self.gen.set(self.gen.get() + 1);
        self.stop_media();
        self.current.borrow_mut().take();
        clear_box(&self.content);
        self.root.set_visible(false);
    }

    fn set_content(&self, w: &gtk4::Widget) {
        self.stop_media();
        clear_box(&self.content);
        if !w.is::<Picture>() {
            w.set_hexpand(true);
            w.set_vexpand(true);
        }
        self.content.append(w);
    }

    fn message(&self, text: &str) -> Label {
        let l = Label::new(Some(text));
        l.add_css_class("preview-note");
        l.set_wrap(true);
        l.set_justify(gtk4::Justification::Center);
        l
    }

    fn spawn_load<T: Send + 'static>(
        self: &Rc<Self>,
        id: u64,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&Rc<Self>, T) + 'static,
    ) {
        let (tx, rx) = async_channel::bounded::<T>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(work());
        });
        let me = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(v) = rx.recv().await {
                if me.gen.get() == id {
                    done(&me, v);
                }
            }
        });
    }

    fn show_thumb(&self, px: Option<ThumbPixels>, err: &str) {
        match px {
            Some(px) => {
                let (pw, ph) = (px.w, px.h);
                let pic = Picture::for_paintable(&px.into_texture());
                pic.set_can_shrink(true);
                pic.set_size_request(pw, ph);
                pic.set_halign(gtk4::Align::Center);
                pic.set_valign(gtk4::Align::Center);
                pic.set_focusable(false);
                pic.add_css_class("preview-img");
                pic.set_overflow(gtk4::Overflow::Hidden);
                self.set_content(pic.upcast_ref());
            }
            None => self.set_content(self.message(err).upcast_ref()),
        }
    }

    fn toggle_audio(&self, path: &Path, btn: &Button) {
        let mut slot = self.audio.borrow_mut();
        if let Some((_, sink)) = slot.as_ref() {
            if !sink.empty() {
                if sink.is_paused() {
                    sink.play();
                    btn.set_label("⏸");
                } else {
                    sink.pause();
                    btn.set_label("▶");
                }
                return;
            }
        }
        *slot = None;
        let (stream, handle) = match OutputStream::try_default() {
            Ok(v) => v,
            Err(_) => {
                btn.set_label("No audio device");
                return;
            }
        };
        let sink = match Sink::try_new(&handle) {
            Ok(s) => s,
            Err(_) => {
                btn.set_label("No audio device");
                return;
            }
        };
        let source = File::open(path)
            .ok()
            .and_then(|f| Decoder::new(BufReader::new(f)).ok());
        match source {
            Some(src) => {
                sink.append(src);
                *slot = Some((stream, sink));
                btn.set_label("⏸");
            }
            None => btn.set_label("Can't play this format"),
        }
    }

    fn start_video(&self, path: &Path) {
        let mf = MediaFile::for_filename(path);
        mf.set_loop(true);
        mf.set_muted(true);
        let video = Picture::for_paintable(&mf);
        video.set_focusable(false);
        video.set_overflow(gtk4::Overflow::Hidden);
        video.set_size_request(PREVIEW_BOX_W, PREVIEW_BOX_H);
        let clip = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::External)
            .vscrollbar_policy(gtk4::PolicyType::External)
            .min_content_width(PREVIEW_BOX_W)
            .max_content_width(PREVIEW_BOX_W)
            .min_content_height(PREVIEW_BOX_H)
            .max_content_height(PREVIEW_BOX_H)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .focusable(false)
            .overflow(gtk4::Overflow::Hidden)
            .child(&video)
            .build();

        let overlay = Overlay::new();
        overlay.set_child(Some(&clip));
        overlay.set_overflow(gtk4::Overflow::Hidden);
        overlay.add_css_class("preview-video");

        let play_btn = Button::with_label("⏸");
        play_btn.add_css_class("video-play-btn");
        play_btn.set_focusable(false);
        play_btn.set_halign(gtk4::Align::End);
        play_btn.set_valign(gtk4::Align::End);
        play_btn.set_margin_bottom(10);
        play_btn.set_margin_end(10);
        overlay.add_overlay(&play_btn);

        let weak_mf = mf.downgrade();
        let toggle = move || {
            if let Some(mf) = weak_mf.upgrade() {
                if mf.is_playing() {
                    mf.pause();
                } else {
                    mf.play();
                }
            }
        };

        {
            let toggle = toggle.clone();
            play_btn.connect_clicked(move |_| toggle());
        }
        let click = gtk4::GestureClick::new();
        click.connect_released(move |_, _, _, _| toggle());
        clip.add_controller(click);

        {
            let weak_btn = play_btn.downgrade();
            mf.connect_playing_notify(move |m| {
                if let Some(btn) = weak_btn.upgrade() {
                    btn.set_label(if m.is_playing() { "⏸" } else { "▶" });
                }
            });
        }

        self.set_content(overlay.upcast_ref());
        *self.media.borrow_mut() = Some(mf.clone().upcast());
        mf.play();
    }

    fn show(self: &Rc<Self>, path: &Path) {
        if self.current.borrow().as_deref() == Some(path) {
            return;
        }
        let id = self.gen.get() + 1;
        self.gen.set(id);
        *self.current.borrow_mut() = Some(path.to_path_buf());

        let kind = preview_kind(path);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        self.meta.set_text(&format!("{} · {}", kind.label(), human_size(size)));
        self.root.set_visible(true);

        let p = path.to_path_buf();
        match kind {
            PreviewKind::Text => {
                self.set_content(self.message("Loading…").upcast_ref());
                self.spawn_load(id, move || read_text_preview(&p), |me, text| {
                    let tv = TextView::builder()
                        .editable(false)
                        .cursor_visible(false)
                        .wrap_mode(WrapMode::WordChar)
                        .left_margin(8)
                        .right_margin(8)
                        .top_margin(6)
                        .bottom_margin(6)
                        .focusable(false)
                        .css_classes(["preview-text"])
                        .build();
                    tv.buffer().set_text(&text);
                    let sw = ScrolledWindow::builder()
                        .hscrollbar_policy(gtk4::PolicyType::Never)
                        .focusable(false)
                        .child(&tv)
                        .build();
                    me.set_content(sw.upcast_ref());
                });
            }
            PreviewKind::Image => {
                self.set_content(self.message("Loading…").upcast_ref());
                self.spawn_load(
                    id,
                    move || load_cover(&p, PREVIEW_BOX_W, PREVIEW_BOX_H),
                    |me, px| me.show_thumb(px, "Can't preview this image"),
                );
            }
            PreviewKind::Pdf => {
                self.set_content(self.message("Rendering…").upcast_ref());
                self.spawn_load(id, move || render_pdf_first_page(&p, id), |me, res| match res {
                    Ok(px) => me.show_thumb(Some(px), ""),
                    Err(msg) => me.set_content(me.message(&msg).upcast_ref()),
                });
            }
            PreviewKind::Video => {
                self.set_content(self.message("Loading…").upcast_ref());
                let me = self.clone();
                glib::timeout_add_local_once(Duration::from_millis(180), move || {
                    if me.gen.get() != id {
                        return;
                    }
                    me.start_video(&p);
                });
            }
            PreviewKind::Audio => {
                let icon = Image::from_file("/var/lib/cynager/icons/audio.svg");
                icon.set_pixel_size(96);
                icon.add_css_class("preview-note");
                icon.set_vexpand(true);
                icon.set_valign(gtk4::Align::Center);
                let btn = Button::with_label("▶");
                btn.add_css_class("video-play-btn");
                btn.set_focusable(false);
                btn.set_halign(gtk4::Align::End);
                btn.set_valign(gtk4::Align::End);
                btn.set_margin_bottom(10);
                btn.set_margin_end(10);
                let me = self.clone();
                let b2 = btn.clone();
                btn.connect_clicked(move |_| me.toggle_audio(&p, &b2));
                let b = GtkBox::builder()
                    .orientation(gtk4::Orientation::Vertical)
                    .spacing(14)
                    .vexpand(true)
                    .height_request(200)
                    .valign(gtk4::Align::End)
                    .build();
                b.append(&icon);
                b.append(&btn);
                self.set_content(b.upcast_ref());
            }
            PreviewKind::Other => {
                self.set_content(self.message("No preview available").upcast_ref());
            }
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
    generation: Arc<AtomicU64>,
    apps_box: GtkBox,
    hits_scroller: ScrolledWindow,
    sel: Rc<Cell<Option<usize>>>,
    focused: Rc<Cell<bool>>,
    calc_help: GtkBox,
    clip_help: GtkBox,
    dirty: Rc<Cell<u32>>,
    preview: Rc<PreviewPane>,
}

const DIRTY_HOME: u32 = 1 << 5;
const DIRTY_ALL: u32 = 0x3f;

impl ModePanel {
    fn new(window: &ApplicationWindow, host: &GtkBox, _display: &Display, apps_box: &GtkBox) -> Self {
        ensure_cliphist_watchers();

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
            .margin_bottom(5)
            .margin_top(12)
            .css_classes(["mode-tabs"])
            .build();

        let mut tabs: Vec<(&'static str, Button)> = Vec::new();
        let labels = std::iter::once(("home", "Top Results")).chain(Cat::ALL.iter().map(|c| (c.id(), c.title())));
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

        let preview = PreviewPane::new();
        scroller.set_hexpand(true);
        let body = GtkBox::builder().orientation(gtk4::Orientation::Horizontal).build();
        body.append(&scroller);
        body.append(&preview.root);

        host.append(&tabbar);
        host.append(&body);

        {
            let pv = preview.clone();
            window.connect_visible_notify(move |w| {
                if !w.is_visible() {
                    pv.clear();
                }
            });
        }

        let panel = Self {
            window: window.clone(),
            stack,
            tabs,
            home,
            pages,
            results: Rc::new(RefCell::new(vec![Vec::new(); Cat::ALL.len()])),
            generation: Arc::new(AtomicU64::new(0)),
            apps_box: apps_box.clone(),
            hits_scroller: scroller,
            sel: Rc::new(Cell::new(None)),
            focused: Rc::new(Cell::new(false)),
            calc_help: calc_help(),
            clip_help: clip_help(),
            dirty: Rc::new(Cell::new(DIRTY_ALL)),
            preview,
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
        self.flush();
    }

    fn mode_focused(&self) -> bool {
        self.focused.get()
    }

    fn visible_hits(&self) -> Vec<Hit> {
        let name = self.stack.visible_child_name().map(|n| n.to_string()).unwrap_or_default();
        let res = self.results.borrow();
        if name.is_empty() || name == "home" {
            Cat::HOME.iter().filter_map(|c| res[c.idx()].first().cloned()).collect()
        } else {
            Cat::ALL
                .iter()
                .find(|c| c.id() == name)
                .map(|c| res[c.idx()].iter().take(page_limit(*c)).cloned().collect())
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
        self.update_preview();
    }

    fn update_preview(&self) {
        let on_files = self.stack.visible_child_name().map_or(false, |n| n == "files");
        let path = if on_files && self.focused.get() {
            self.visible_hits()
                .get(self.sel.get().unwrap_or(0))
                .and_then(|h| h.path.clone())
        } else {
            None
        };
        match path {
            Some(p) if p.is_file() => self.preview.show(&p),
            _ => self.preview.clear(),
        }
    }

    fn focus_apps(&self) {
        self.focused.set(false);
        self.sel.set(None);
        self.apply_selection();
    }

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

    fn search_fast(&self, q: &str) {
        let q = q.trim().to_string();
        let mine = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.focused.set(false);
        self.sel.set(None);

        if is_clip_query(&q)
            && self.stack.visible_child_name().map(|n| n.as_str() != "clip").unwrap_or(true)
        {
            self.select("clip");
        }

        {
            let mut r = self.results.borrow_mut();
            r[Cat::Files.idx()].clear();
            r[Cat::Calc.idx()] = calc_hits(&q);
            r[Cat::Clip.idx()].clear();
            r[Cat::Pills.idx()] = pill_hits(&q);
        }
        self.render();

        if FILES_ENABLED && q.chars().count() >= FILE_MIN_QUERY {
            let (tx, rx) = async_channel::bounded::<Vec<PathBuf>>(1);
            let generation = self.generation.clone();
            let qq = q.clone();
            let _ = std::thread::Builder::new()
                .name("altdot-file-search".to_string())
                .spawn(move || {
                    let found = search_files(&qq, &generation, mine);
                    let _ = tx.send_blocking(found);
                });

            let panel = self.clone();
            glib::spawn_future_local(async move {
                if let Ok(paths) = rx.recv().await {
                    if panel.generation.load(Ordering::SeqCst) == mine {
                        let hits = file_hits(paths, &panel.window);
                        let unchanged = hits.is_empty()
                            && panel.results.borrow()[Cat::Files.idx()].is_empty();
                        if !unchanged {
                            panel.results.borrow_mut()[Cat::Files.idx()] = hits;
                            panel.render_cats(&[Cat::Files]);
                        }
                    }
                }
            });
        }
    }

    fn search_slow(&self, q: &str) {
        let q = q.trim().to_string();
        if q.is_empty() {
            return;
        }
        let mine = self.generation.load(Ordering::SeqCst);

        if CLIP_ENABLED {
            let (tx, rx) = async_channel::unbounded::<ClipMsg>();
            let generation = self.generation.clone();
            let qq = q.clone();
            let _ = std::thread::Builder::new()
                .name("altdot-clipboard-search".to_string())
                .spawn(move || {
                    let result = search_clip(&qq, &generation, mine);
                    let jobs: Vec<(String, ThumbJob)> = match &result {
                        Ok(items) => items
                            .iter()
                            .filter_map(|it| it.job.clone().map(|j| (it.line.clone(), j)))
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                    if tx.send_blocking(ClipMsg::Items(result)).is_err() {
                        return;
                    }

                    let mut batch: Vec<(String, ThumbPixels)> = Vec::new();
                    let mut last = Instant::now();
                    for (line, job) in jobs {
                        if generation.load(Ordering::SeqCst) != mine {
                            return;
                        }
                        if let Some(t) = run_thumb_job(&job, &line) {
                            batch.push((line, t));
                        }
                        if !batch.is_empty() && last.elapsed() >= Duration::from_millis(200) {
                            if tx.send_blocking(ClipMsg::Thumbs(std::mem::take(&mut batch))).is_err() {
                                return;
                            }
                            last = Instant::now();
                        }
                    }
                    if !batch.is_empty() {
                        let _ = tx.send_blocking(ClipMsg::Thumbs(batch));
                    }
                });

            let panel = self.clone();
            glib::spawn_future_local(async move {
                while let Ok(msg) = rx.recv().await {
                    if panel.generation.load(Ordering::SeqCst) != mine {
                        break;
                    }
                    match msg {
                        ClipMsg::Items(res) => {
                            let hits = clip_hits(res);
                            if !hits.is_empty() {
                                panel.results.borrow_mut()[Cat::Clip.idx()] = hits;
                                panel.render_cats(&[Cat::Clip]);
                            }
                        }
                        ClipMsg::Thumbs(batch) => panel.apply_thumbs(batch),
                    }
                }
            });
        }

    }

    fn apply_thumbs(&self, batch: Vec<(String, ThumbPixels)>) {
        let idx = Cat::Clip.idx();
        let built = self.dirty.get() & (1u32 << idx) == 0;
        let mut rows: Vec<gtk4::Widget> = Vec::new();
        if built {
            let mut c = self.pages[idx].first_child();
            while let Some(w) = c {
                c = w.next_sibling();
                rows.push(w);
            }
        }

        {
            let mut res = self.results.borrow_mut();
            let hits = &mut res[idx];
            for (line, t) in batch {
                let tex = t.into_texture();
                for (i, h) in hits.iter_mut().enumerate() {
                    if h.clip_line.as_deref() == Some(line.as_str()) {
                        h.thumb = Some(tex.clone());
                        if let Some(row) = rows.get(i) {
                            set_row_thumb(row, &tex);
                        }
                    }
                }
            }
        }
        self.mark_dirty(DIRTY_HOME);
        self.flush();
    }

    fn mark_dirty(&self, bits: u32) {
        self.dirty.set(self.dirty.get() | bits);
    }

    fn build_page(&self, c: Cat) {
        let res = self.results.borrow();
        let hits = &res[c.idx()];
        let page = &self.pages[c.idx()];
        clear_box(page);

        if hits.is_empty() {
            if c == Cat::Calc {
                page.append(&self.calc_help);
            } else if c == Cat::Clip {
                page.append(&self.clip_help);
            } else {
                let none = Label::new(Some("Nothing found"));
                none.add_css_class("sugg-empty-sub");
                none.set_halign(gtk4::Align::Start);
                page.append(&none);
            }
        } else {
            for h in hits.iter().take(page_limit(c)) {
                page.append(&hit_button(h, &self.window));
            }
        }
    }

    fn build_home(&self) {
        let res = self.results.borrow();
        clear_box(&self.home);
        for c in Cat::HOME {
            if let Some(top) = res[c.idx()].first() {
                let head = Label::new(Some(c.title()));
                head.add_css_class("hit-cat");
                head.set_halign(gtk4::Align::Start);
                self.home.append(&head);
                self.home.append(&hit_button(top, &self.window));
            }
        }
    }

    fn flush(&self) {
        let name = self
            .stack
            .visible_child_name()
            .map(|n| n.to_string())
            .unwrap_or_default();
        let mut d = self.dirty.get();
        if name.is_empty() || name == "home" {
            if d & DIRTY_HOME != 0 {
                self.build_home();
                d &= !DIRTY_HOME;
            }
        } else if let Some(c) = Cat::ALL.iter().copied().find(|c| c.id() == name) {
            let bit = 1u32 << c.idx();
            if d & bit != 0 {
                self.build_page(c);
                d &= !bit;
            }
        }
        self.dirty.set(d);
        self.apply_selection();
    }

    fn render_cats(&self, cats: &[Cat]) {
        for c in cats {
            self.mark_dirty(1u32 << c.idx());
        }
        self.mark_dirty(DIRTY_HOME);
        self.flush();
    }

    fn render(&self) {
        self.mark_dirty(DIRTY_ALL);
        self.flush();
    }

    fn delete_current(&self) -> bool {
        if !self.focused.get() {
            return false;
        }
        let hits = self.visible_hits();
        let idx = self.sel.get().unwrap_or(0);
        let Some(line) = hits.get(idx).and_then(|h| h.clip_line.clone()) else {
            return false;
        };

        {
            let mut res = self.results.borrow_mut();
            for v in res.iter_mut() {
                v.retain(|h| h.clip_line.as_deref() != Some(line.as_str()));
            }
        }
        self.sel.set(Some(idx));
        self.render();

        std::thread::spawn(move || cliphist_delete(&line));
        true
    }

    fn activate_current(&self) -> bool {
        let hits = self.visible_hits();
        let idx = self.sel.get().unwrap_or(0);
        match hits.get(idx).or(hits.first()) {
            Some(h) => {
                let a = h.action.clone();
                a();
                if picker_widget(&self.window).is_none() {
                    self.window.set_visible(false);
                }
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

    for info in cached_apps().iter() {
        grid.insert(&make_app_button(info.clone(), window), -1);
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
    _apps: Rc<Vec<AppInfo>>,
) -> (ApplicationWindow, ApplicationWindow, FlowBox, GtkBox, SearchUi) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("alt.")
        .default_width(800)
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

    let bd_click = gtk4::GestureClick::new();
    {
        let win = window.clone();
        bd_click.connect_pressed(move |_, _, _, _| win.set_visible(false));
    }
    bd.add_controller(bd_click);

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
        .hexpand(true)
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
                    gtk4::gdk::Key::Down => {
                        ui.panel.move_hit(1);
                        return glib::Propagation::Stop;
                    }
                    gtk4::gdk::Key::Up => {
                        ui.panel.move_hit(-1);
                        return glib::Propagation::Stop;
                    }
                    gtk4::gdk::Key::Delete | gtk4::gdk::Key::KP_Delete => {
                        if ui.panel.delete_current() {
                            return glib::Propagation::Stop;
                        }
                    }
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
        let query_generation = Rc::new(Cell::new(0_u64));
        search.entry.connect_changed(move |e| {
            if !ui.active() {
                return;
            }
            let q = e.text().to_string();
            let generation = query_generation.get().wrapping_add(1);
            query_generation.set(generation);

            if q.trim().is_empty() {
                ui.panel.generation.fetch_add(1, Ordering::SeqCst);
                ui.exit();
                refresh_suggestions(&sg, &w, &u);
                return;
            }

            {
                let ui = ui.clone();
                let sg = sg.clone();
                let qg = query_generation.clone();
                let w = w.clone();
                let q = q.clone();
                glib::timeout_add_local_once(Duration::from_millis(FAST_DEBOUNCE_MS), move || {
                    if qg.get() != generation || !ui.active() {
                        return;
                    }
                    show_results(&sg, &w, &q);
                });
            }

            {
                let ui = ui.clone();
                let qg = query_generation.clone();
                glib::timeout_add_local_once(Duration::from_millis(MODE_DEBOUNCE_MS), move || {
                    if qg.get() != generation || !ui.active() {
                        return;
                    }
                    ui.panel.search_fast(&q);
                    ui.panel.search_slow(&q);
                });
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
            background-color: rgba(24, 24, 24, 0.61);
            border: 1px solid rgba(255, 255, 255, 0.13);
            border-radius: 35px;
            opacity: 0;
        }

        .bd-fill {
            background: linear-gradient(to bottom, #04040400, transparent);
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
            background-color: rgba(254, 252, 252, 0.16);
            border-radius: 5px;
            font-size: 11px;
            font-weight: 600;
            color: rgba(253, 253, 253, 0.56);
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

        .help-hero {
            min-width: 44px;
            min-height: 44px;
            border-radius: 12px;
            background: linear-gradient(#fff2, #0001),  #ff7c7c;
            color: white;
            font-size: 26px;
            font-weight: 700;
        }
        .help-title {
            font-size: 17px;
            font-weight: 700;
            color: rgba(253, 253, 253, 0.9);
        }
        .help-sub {
            font-size: 12px;
            color: rgba(253, 253, 253, 0.5);
        }
        .help-group {
            background-color: rgba(255, 255, 255, 0.06);
            border-radius: 14px;
        }
        .help-div {
            min-height: 1px;
            margin-left: 50px;
            background-color: rgba(255, 255, 255, 0.08);
        }
        .help-label {
            font-size: 13px;
            font-weight: 600;
            color: rgba(253, 253, 253, 0.8);
        }
        .help-tile {
            min-width: 26px;
            min-height: 26px;
            border-radius: 50px;
            font-size: 14px;
            font-weight: 700;
            color: white;
        }
        .tile-blue { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #0a84ff; }
        .tile-orange { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #ff9f0a; }
        .tile-purple { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #bf5af2; }
        .tile-green { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #30d158; }
        .tile-gray { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #8e8e93; }
        .tile-red { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #ff453a; }
        .tile-teal { background: linear-gradient(#fff2, rgba(0, 0, 0, 0.28)),  #40c8e0; }
        .help-chip {
            padding: 2px 9px;
            border-radius: 20px;
            background-color: rgba(255, 255, 255, 0.1);
            font-size: 16px;
            color: rgba(253, 253, 253, 0.85);
        }
        .help-chips flowboxchild {
            padding: 0;
            margin: 0;
            min-width: 0;
            min-height: 0;
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

        .hit-thumb {
            border-radius: 8px;
        }

        .preview-pane {
            padding: 12px;
            border-radius: 16px;
            background-color: rgba(255, 255, 255, 0.05);
        }
        .preview-title {
            font-size: 14px;
            font-weight: 700;
            color: rgba(253, 253, 253, 0.9);
        }
        .preview-meta {
            font-size: 12px;
            color: rgba(253, 253, 253, 0.45);
            margin-bottom: 4px;
        }
        .preview-note {
            font-size: 13px;
            color: rgba(253, 253, 253, 0.45);
        }
        textview.preview-text, textview.preview-text text {
            background-color: transparent;
            color: rgba(253, 253, 253, 0.67);
            font-size: 12px;
        }
        .preview-img {
            border-radius: 10px;
        }
        .preview-video {
            border-radius: 12px;
            background-color: rgba(255, 255, 255, 0.13);
        }
        .video-play-btn {
            min-width: 36px;
            min-height: 36px;
            padding: 0;
            border-radius: 18px;
            font-size: 14px;
            color: rgba(253, 253, 253, 0.95);
            background-color: rgba(0, 0, 0, 0.55);
        }
        .video-play-btn:hover {
            background-color: rgba(0, 0, 0, 0.75);
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
        let apps = Rc::new(apps);
        seed_app_cache(apps.clone());
        watch_app_changes();
        refresh_file_index(true);
        glib::idle_add_local_once(|| {
            let _ = app_index();
        });
        let usage: UsageMap = Arc::new(Mutex::new(load_usage()));
        spawn_tracker(resolver, usage.clone());

        let (window, bd, grid, suggestionbox, search) = build_window(app, &usage, apps.clone());

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
                    if take_grid_stale() {
                        populate(&grid, &window);
                    }
                    clip_cache_clear();
                    refresh_file_index(false);
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
        {
            let t = toggle.clone();
            let window = window.clone();
            let search = search.clone();
            let rx = spawn_signal_listener();
            glib::spawn_future_local(async move {
                while let Ok(sig) = rx.recv().await {
                    if sig == libc::SIGUSR1 {
                        t();
                    } else if sig == libc::SIGUSR2 {
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
                }
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