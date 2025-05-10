use gtk4::prelude::*;
use vte4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, CssProvider, Entry, Label, Orientation,
    Revealer, RevealerTransitionType, ScrolledWindow, gio::Cancellable,
};
use gtk4::gdk::Display;
use gtk4::glib;
use gtk4::glib::{SpawnFlags,Pid,Error};
use std::cell::RefCell;
use std::fs;
use std::rc::Rc;
use std::process::{Command, exit};
use vte4::Terminal;
use vte4::PtyFlags;

fn main() {
    let app = Application::builder()
        .application_id("com.example.launcher")
        .build();

    app.connect_activate(build_ui);
    Command::new("bash").arg("-c").arg("cd").arg("~/");
    app.run();
}

#[derive(Clone, PartialEq)]
enum Mode {
    App,
    Notes,
    Ai
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("moss")
        .default_width(1000)
        .default_height(400)
        .resizable(false)
        .decorated(false)
        .build();

    let event_controller = gtk4::EventControllerKey::new();
    window.add_controller(event_controller.clone());

    let scroll_controller = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
    window.add_controller(scroll_controller.clone());

    let vbox = GtkBox::new(Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let entry = Entry::builder()
        .placeholder_text("Search apps, open marker or use alterAi")
        .hexpand(false)
        .activates_default(true)
        .build();

    let result = Label::new(Some(" Moss \ntype `/` to get started"));
    result.set_widget_name("result-card");
    result.set_margin_top(20);
    result.set_margin_bottom(10);
    result.hexpands();

    let result_revealer = Revealer::builder()
        .transition_type(RevealerTransitionType::SlideUp)
        .transition_duration(300)
        .child(&result)
        .reveal_child(true)
        .build();

    let terminal = Terminal::new();
    terminal.set_vexpand(true);
    terminal.set_hexpand(true);

    let terminal_box = GtkBox::new(Orientation::Vertical, 0);
    terminal_box.set_widget_name("tbox");
    terminal_box.set_vexpand(true);
    terminal_box.set_hexpand(true);
    terminal_box.set_size_request(1500, 800);
    terminal_box.append(&terminal);
    terminal_box.set_visible(false);

    let vbox_inner = GtkBox::new(Orientation::Vertical, 12);
    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_child(Some(&vbox_inner));

    vbox.append(&entry);
    vbox.append(&result_revealer);
    vbox.append(&scroller);
    vbox.append(&terminal_box);
    window.set_child(Some(&vbox));

    let result = Rc::new(result);
    let result_revealer = Rc::new(result_revealer);
    let vbox_opt = Rc::new(vbox_inner);
    let selected_index = Rc::new(RefCell::new(0));
    let current_items = Rc::new(RefCell::new(Vec::new()));
    let current_mode = Rc::new(RefCell::new(Mode::App));
    let terminal = Rc::new(terminal);
    let terminal_box = Rc::new(terminal_box);


    {
        let entry = entry.clone();
        let vbox_opt = vbox_opt.clone();
        let result = result.clone();
        let result_revealer = result_revealer.clone();
        let selected_index = selected_index.clone();
        let current_items = current_items.clone();
        let current_mode = current_mode.clone();

        entry.connect_changed(move |e| {
            let text = e.text().to_string();
            let (mode, items) = lister(&text);
            *current_mode.borrow_mut() = mode.clone();
            *current_items.borrow_mut() = items.clone();
            *selected_index.borrow_mut() = 0;

            result_revealer.set_reveal_child(text.is_empty());

            // Clear and update vbox_opt
            while let Some(widget) = vbox_opt.first_child() {
                vbox_opt.remove(&widget);
            }

            for (i, item) in items.iter().enumerate() {
                let label = Label::new(Some(item));
                label.set_widget_name(if i == 0 { "highlighted" } else { "" });
                label.set_halign(gtk4::Align::Start);
                vbox_opt.append(&label);
            }

            result.set_text(
                items.get(0)
                    .map(|s| s.as_str())
                    .unwrap_or("No match"),
            );
        });
    }

    {
        let result = result.clone();
        let entry = entry.clone();
        let vbox_opt = vbox_opt.clone();
        let selected_index = selected_index.clone();
        let current_items = current_items.clone();
        let current_mode = current_mode.clone();

        event_controller.connect_key_pressed(move |_, key, _, _| {
            let mut index = selected_index.borrow_mut();
            let items = current_items.borrow();

            match key {
                gtk4::gdk::Key::Down => {
                    if *index + 1 < items.len() {
                        *index += 1;
                    }
                }
                gtk4::gdk::Key::Up => {
                    if *index > 0 {
                        *index -= 1;
                    }
                }
                gtk4::gdk::Key::KP_Enter | gtk4::gdk::Key::Return => {
                    // Get the selected item
                    if let Some(selected) = items.get(*index) {
                        match *current_mode.borrow() {
                            Mode::App => {
                                let path = format!("/usr/share/applications/{}.desktop", selected);
                                if let Ok(contents) = fs::read_to_string(&path) {
                                    let mut exec_line = None;
                                    let mut terminal_flag = false;

                                    for line in contents.lines() {
                                        if line.starts_with("Terminal=") {
                                            terminal_flag = line.trim_start_matches("Terminal=").trim() == "true";
                                        }
                                        if line.starts_with("Exec=") {
                                            exec_line = Some(line.trim_start_matches("Exec=").trim().to_string());
                                        }
                                    }

                                    if let Some(exec) = exec_line {
                                        if terminal_flag {
                                            entry.set_visible(false);
                                            scroller.set_visible(false);
                                            terminal_box.set_visible(true);

                                            let command = &exec;
                                            let argv = ["sh", "-c", command];

                                            terminal.spawn_async(
                                                PtyFlags::DEFAULT,
                                                None,                    // working directory
                                                &argv,                   // command to run
                                                &[],                     // environment vars
                                                SpawnFlags::DEFAULT,
                                                || {},                   // child setup (no-op)
                                                -1,                      // timeout (-1 means no timeout)
                                                None::<&Cancellable>,    // no cancellation
                                                move |res: Result<Pid, Error>| {
                                                    if let Err(e) = res {
                                                        eprintln!("Failed to spawn terminal process: {}", e);
                                                    }
                                                },
                                            );
                                            let terminal_box_clone = terminal_box.clone();
                                            let entry_clone = entry.clone();
                                            let scroller_clone=scroller.clone();

                                            terminal.connect_child_exited(move |_terminal, _status| {
                                                terminal_box_clone.set_visible(false);
                                                entry_clone.set_visible(true);
                                                scroller_clone.set_visible(true);
                                            });
                                        } else {
                                            let command = exec.split_whitespace().next().unwrap_or("");
                                            if !command.is_empty() && command != "bash" {
                                                if let Err(e) = Command::new(command).spawn() {
                                                    eprintln!("Failed to start GUI app: {}", e);
                                                }
                                                exit(0);
                                            } else {
                                                eprintln!("Invalid or unsupported command in Exec=");
                                            }
                                        }
                                    } else {
                                        eprintln!("No Exec line found");
                                    }
                                } else {
                                    eprintln!("Failed to read .desktop file: {}", path);
                                }
                            }
                            Mode::Notes => {
                                // insert code here
                            }
                            Mode::Ai => {
                                if let Err(e) = Command::new("xdg-open")
                                    .arg(format!("https://www.google.com/search?q={}", selected))
                                    .spawn()
                                {
                                    eprintln!("Failed to open web search: {}", e);
                                }
                                exit(0);
                            }
                        }
                    } else {
                        eprint!("error nothing selected")
                    }
                }
                gtk4::gdk::Key::slash => {
                    entry.grab_focus();
                }
                gtk4::gdk::Key::Escape => {
                    std::process::exit(0);
                }
                _ => return glib::Propagation::Proceed,
            }

            // Update labels
            let mut child = vbox_opt.first_child();
            let mut i = 0;
            while let Some(widget) = child {
                child = widget.next_sibling();
                if let Some(label) = widget.downcast_ref::<Label>() {
                    label.set_widget_name(if i == *index { "highlighted" } else { "" });
                }
                i += 1;
            }

            if let Some(selected) = items.get(*index) {
                result.set_text(selected);
            }

            glib::Propagation::Stop
        });
    }

    {
        let result_revealer = result_revealer.clone();
        scroll_controller.connect_scroll(move |_, _, _| {
            result_revealer.set_reveal_child(false);
            glib::Propagation::Stop
        });
    }

    let css = r#"
        window {
            background-color: rgba(30, 30, 30, 0.6);
            border-radius: 12px;
            border-style: solid;
            border-width: 2px ;
            border-color: rgba(139, 139, 139, 0.59);
        }
        #result-card {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 100px;
            border-radius: 12px;
            font-size: 50px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 700;
        }
        entry {
            border:none;
            padding: 10px;
            padding-left: 15px;
            border-radius: 10px;
            background-color: rgba(0, 0, 0, 0.1);
            color: white;
        }
        label {
            color: white;
            font-size: 18px;
            font-family: "Cantarell";
        }
        #highlighted {
            background-color: rgba(124, 124, 124, 0.14);
            padding:10px ;
            border-radius: 5px;
        }
        #tbox {
            border-radius: 10px;
            background-color:rgb(0, 0, 0);
            padding: 5px;
            border-style: solid;
            border-width: 1px ;
            border-color: rgba(139, 139, 139, 0.5);
        }
    "#;

    let provider = CssProvider::new();
    provider.load_from_data(css);
    gtk4::style_context_add_provider_for_display(
        &Display::default().unwrap(),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    window.show();
}

fn lister(input: &str) -> (Mode, Vec<String>) {
    if input.starts_with('`') {
        // dummy return
        let entries:Vec<String> = Vec::new();
        return (Mode::Notes, entries);
    }

    if input.starts_with('!') {
        let query = input.trim_start_matches('!');
        if query.is_empty() {
            return (Mode::Ai, vec![]);
        }
        return (
            Mode::Ai,
            vec![
                format!("Search: {}", query),
                format!("{} : Wikipedia", query),
            ],
        );
    }

    // App mode with fuzzy matching
    let apps = Command::new("bash")
        .arg("-c")
        .arg("find /usr/share/applications -maxdepth 1 -type f -name '*.desktop' -printf '%f\n' | sed 's/\\.desktop$//' | sort")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_default();

    let filtered = apps
        .into_iter()
        .filter(|s| fuzzy_match(s, input))
        .collect();

    (Mode::App, filtered)
}

fn fuzzy_match(text: &str, pattern: &str) -> bool {
    let mut t_chars = text.chars();
    for pc in pattern.chars() {
        if let Some(_) = t_chars.find(|&tc| tc == pc) {
            continue;
        } else {
            return false;
        }
    }
    true
}
