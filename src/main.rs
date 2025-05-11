use gtk4::prelude::*;
use vte4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, CssProvider, Entry, Label, Orientation,
    Revealer, RevealerTransitionType, ScrolledWindow, gio::Cancellable, prelude::WidgetExt
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
use std::path::Path;
use chrono::{Local, DateTime};

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
    Ai,
    None
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
    // vbox.set_margin_top(20);
    // vbox.set_margin_bottom(20);
    // vbox.set_margin_start(20);
    // vbox.set_margin_end(20);

    let entry = Entry::builder()
        .placeholder_text(" Search apps, open marker or use alterAi")
        .hexpand(true)
        .activates_default(false)
        .build();
    entry.set_visible(false);

    let info_lable = Label::new(Some(" Moss \ntype `/` to get started"));
    info_lable.set_widget_name("info_lable-card");
    info_lable.set_margin_top(20);
    info_lable.set_margin_bottom(10);
    info_lable.set_margin_end(20);
    info_lable.set_margin_start(20);
    info_lable.hexpands();

    let info_lable_revealer = Revealer::builder()
        .transition_type(RevealerTransitionType::SlideUp)
        .transition_duration(300)
        .child(&info_lable)
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
    terminal_box.set_margin_start(4);
    terminal_box.set_margin_end(4);
    terminal_box.set_margin_top(4);
    terminal_box.set_margin_bottom(4);
    terminal_box.append(&terminal);
    terminal_box.set_visible(false);

    let vbox_inner = GtkBox::new(Orientation::Vertical, 12);
    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_child(Some(&vbox_inner));

    vbox.append(&entry);
    vbox.append(&info_lable_revealer);
    vbox.append(&scroller);
    vbox.append(&terminal_box);
    window.set_child(Some(&vbox));

    let info_lable_revealer = Rc::new(info_lable_revealer);
    let vbox_opt = Rc::new(vbox_inner);
    let selected_index = Rc::new(RefCell::new(0));
    let current_items = Rc::new(RefCell::new(Vec::new()));
    let current_mode = Rc::new(RefCell::new(Mode::App));
    let current_file_path_name= Rc::new(RefCell::new(Vec::new()));
    let terminal = Rc::new(terminal);
    let terminal_box = Rc::new(terminal_box);


    {
        let entry = entry.clone();
        let vbox_opt = vbox_opt.clone();
        let info_lable_revealer = info_lable_revealer.clone();
        let selected_index = selected_index.clone();
        let current_items = current_items.clone();
        let current_mode = current_mode.clone();
        let current_file_path_name=current_file_path_name.clone();
        let info = info_lable.clone(); 

        entry.connect_changed(move |e| {
            let text = e.text().to_string();
            let (mode, items, file_path_name) = lister(&text);
            let mut _res_flag = false;
            *current_mode.borrow_mut() = mode.clone();
            *current_items.borrow_mut() = items.clone();
            *current_file_path_name.borrow_mut() = file_path_name.clone();
            *selected_index.borrow_mut() = 0;
            
            // info flags
            if text == ">t"{
                let now: DateTime<Local> = Local::now();
                let datetime_am_pm = now.format(" %B %e,\n%l:%M %P").to_string();
                info.set_text(&datetime_am_pm);
                _res_flag = true

            } else {
                info.set_text(" type\n` for marker\n ! for alterAi");
                _res_flag = false
            }

            info_lable_revealer.set_reveal_child(text.is_empty() || _res_flag);

            if text.is_empty() || _res_flag {
                vbox_opt.set_visible(false);
            }else {
                vbox_opt.set_visible(true);
            }

            // Clear and update vbox_opt
            while let Some(widget) = vbox_opt.first_child() {
                vbox_opt.remove(&widget);
            }

            for (i, item) in items.iter().enumerate() {
                let label = Label::new(Some(item));
                label.set_widget_name(if i == 0 { "highlighted" } else { "" });
                label.set_halign(gtk4::Align::Start);
                label.set_margin_start(20);
                label.set_margin_end(20);
                label.set_margin_bottom(20);
                vbox_opt.append(&label);
            }

        });
    }

    {
        let entry = entry.clone();
        let vbox_opt = vbox_opt.clone();
        let selected_index = selected_index.clone();
        let current_items = current_items.clone();
        //let current_mode = current_mode.clone();
        let current_file_path_name=current_file_path_name.clone();

        event_controller.connect_key_pressed(move |_, key, _, _| {
            let mut index = selected_index.borrow_mut();
            let items = current_items.borrow();
            let file_path_name=current_file_path_name.borrow();

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
                        if let Some(file_name) = file_path_name.get(*index) {
                            let path_str = format!("{}",&file_name);  
                            let path = Path::new(&path_str);
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
                                eprintln!("Failed to read .desktop file: {}", path_str);
                            }
                            
                        } else {
                                eprintln!("No pathname returned for selected index");
                        }
                    
                }
                gtk4::gdk::Key::slash => {
                    entry.set_visible(true);
                    entry.grab_focus();
                    info_lable.set_text(" type\n` for marker\n ! for alterAi");
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
            
            glib::Propagation::Stop
        });
    }

    {
        let info_lable_revealer = info_lable_revealer.clone();
        scroll_controller.connect_scroll(move |_, _, _| {
            info_lable_revealer.set_reveal_child(false);
            glib::Propagation::Stop
        });
    }

    let css = r#"
        window {
            background-color: rgba(20, 37, 27, 0.6);
            border-radius: 12px;
            border-style: solid;
            border-width: 2px ;
            border-color: rgba(73, 73, 73, 0.59);
        }
        #info_lable-card {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 100px;
            border-radius: 12px;
            font-size: 50px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 700;
        }
        entry {
            border-bottom: 1px solid rgba(73, 73, 73, 0.59);
            padding: 10px;
            padding-left: 15px;
            border-radius: 0px;
            background-color: rgba(0, 0, 0, 0.1);
            color: rgba(207, 207, 207, 0.9);
            box-shadow:none;
            font-family: "Cantarell";
            font-weight: 400;
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
    // I wrote thiss yeeey ekahPruthvi <ekahpdp@gmail.com>
    gtk4::style_context_add_provider_for_display(
        &Display::default().unwrap(),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    window.show();
}

fn lister(input: &str) -> (Mode, Vec<String>, Vec<String>) {
    if input.starts_with('`') {
        return (Mode::Notes, Vec::new(), Vec::new());
    }

    if input.starts_with('!') {
        let query = input.trim_start_matches('!');
        if query.is_empty() {
            return (Mode::Ai, vec![], Vec::new());
        }
        return (
            Mode::Ai,
            vec![
                format!("Search: {}", query),
                format!("{} : Wikipedia", query),
            ],
            Vec::new()
        );
    }

    let mut pairs = Vec::new(); // Vec<(app_name, path_str)>

    if let Ok(read_dir) = fs::read_dir("/usr/share/applications") {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }

            if let Ok(content) = fs::read_to_string(&path) {
                if content.lines().any(|line| line.trim() == "NoDisplay=true") {
                    continue;
                }

                let mut in_desktop_entry = false;
                let mut app_name = None;

                for line in content.lines() {
                    let line = line.trim();

                    if line.starts_with('[') {
                        in_desktop_entry = line == "[Desktop Entry]";
                    }

                    if in_desktop_entry && line.starts_with("Name=") {
                        app_name = Some(line.trim_start_matches("Name=").to_string());
                        break;
                    }
                }

                if let (Some(name), Some(path_str)) = (app_name, path.to_str()) {
                    pairs.push((name, path_str.to_string()));
                }
            }
        }
    }

    if input.starts_with('>') {
        // check for info flages
        if input.len() > 1  {
            return (Mode::None,Vec::new(),Vec::new());
        }

        let (entries, paths): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        return (Mode::App, entries, paths);
    }

    let filtered: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(name, _)| fuzzy_match(name, input))
        .collect();

    let (entries, paths): (Vec<_>, Vec<_>) = filtered.into_iter().unzip();
    (Mode::App, entries, paths)
}

fn fuzzy_match(text: &str, pattern: &str) -> bool {
    let binding = text.to_lowercase();
    let mut t_chars = binding.chars();
    for pc in pattern.to_lowercase().chars() {
        if let Some(_) = t_chars.find(|&tc| tc == pc) {
            continue;
        } else {
            return false;
        }
    }
    true
}
