use gtk4::prelude::*;
use vte4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, CssProvider, Entry, Label, Orientation,
    Revealer, RevealerTransitionType, ScrolledWindow, gio::Cancellable, prelude::WidgetExt, TextView, TextBuffer, TextTag, TextTagTable, Button, Image,
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
use groq_api_rust::{AsyncGroqClient, ChatCompletionRequest, ChatCompletionRoles, ChatCompletionMessage};
use tokio::runtime::Runtime;
use std::cell::Cell;
use std::fs::{create_dir_all, write, OpenOptions};
use std::io::{ Write, Read };

async fn run_gtk_app() {
    let app = Application::builder()
        .application_id("ekah.scu.alt")
        .build();

    app.connect_activate(build_ui);

    app.run();
}

fn main() {
    let rt = Runtime::new().unwrap();
    
    rt.block_on(run_gtk_app());
}

#[derive(Clone, PartialEq)]

enum Mode {
    App,
    Notes,
    Ai,
    None
}

fn read_api_key() -> String {
    let home_dir = std::env::var("HOME").unwrap_or_default();
    let key_path = format!("{}/.config/alt/key.dat", home_dir);
    fs::read_to_string(key_path)
        .expect("Failed to read API key file")
        .trim()
        .to_string()
}

fn typing_effect(label: &Label, text: &str, delay_ms: u64) {
    let label = label.clone();
    let chars: Vec<char> = text.chars().collect();
    let index = Rc::new(RefCell::new(0));

    let chars_rc = Rc::new(chars);

    glib::timeout_add_local(std::time::Duration::from_millis(delay_ms), move || {
        let i = *index.borrow();

        if i < chars_rc.len() {
            let current_text = chars_rc.iter().take(i + 1).collect::<String>();
            label.set_text(&current_text);
            *index.borrow_mut() += 1;
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

fn ai_typing_effect(label: &Label, text: &str, delay_ms: u64, scr: &ScrolledWindow, boxx: &GtkBox ) {
    let label = label.clone();
    let chars: Vec<char> = text.chars().collect();
    let index = Rc::new(RefCell::new(0));

    let chars_rc = Rc::new(chars);
    let boxx = boxx.clone();
    let scr = scr.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(delay_ms), move || {
        let i = *index.borrow();

        if i < chars_rc.len() {
            let current_text = chars_rc.iter().take(i + 1).collect::<String>();
            label.set_text(&current_text);
            *index.borrow_mut() += 1;
            if boxx.height() < 800 {
                scr.set_size_request(1000, boxx.height());
            } else {
                scr.set_size_request(1000, 800); 
            }
            let vadj = scr.vadjustment();
            vadj.set_value(vadj.upper());
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

fn strip_markdown_symbols(text: &str) -> String {
    text.replace("**", "")
        .replace("__", "")
        .replace("*", "")
        .replace("`", "")
}

fn torq_marker(notescroller: &ScrolledWindow) {
    let home_dir = std::env::var("HOME").unwrap_or_default();
    let notes_path: std::path::PathBuf = [home_dir.as_str(), ".config/alt/markers/notes.algae"]
        .iter()
        .collect();

    notescroller.set_hexpand(true);
    notescroller.set_vexpand(true);
    notescroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    
    
    let tag_table = TextTagTable::new();
    let buffer = TextBuffer::new(Some(&tag_table));
    let text_view = TextView::with_buffer(&buffer);
    text_view.set_editable(true);
    text_view.set_monospace(true);
    text_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    text_view.set_justification(gtk4::Justification::Fill);
    text_view.style_context().add_class("textview-style");
    text_view.set_hexpand(true);
    text_view.set_vexpand(true);
    text_view.set_margin_bottom(5);
    text_view.set_margin_top(5);
    text_view.set_margin_start(5);
    text_view.set_margin_end(5);
    text_view.set_opacity(0.7);
    notescroller.set_child(Some(&text_view));

    let tag_large = TextTag::builder()
        .name("large")
        .scale(gtk4::pango::SCALE_LARGE)
        .weight(900)
        .letter_spacing(1)
        .font("Cantarell Heavy 18")
        .build();

    let tag_hidden = TextTag::builder()
        .name("hidden")
        .invisible(true)
        .build();

    let tag_code_prefix = TextTag::builder()
        .name("codeprefix")
        .background("rgba(153, 255, 158, 0.55)")
        .font("FreeMono 13")
        .weight(200)
        .build();

    buffer.tag_table().add(&tag_large);
    buffer.tag_table().add(&tag_hidden);
    buffer.tag_table().add(&tag_code_prefix);

    if let Ok(content) = std::fs::read_to_string(&notes_path) {
        buffer.set_text(&content);
    }

    let buffer_rc = Rc::new(buffer);
    let is_formatting = Rc::new(Cell::new(false));
    let apply_formatting = {
        let buffer = buffer_rc.clone();
        let is_formatting = is_formatting.clone();

        move || {
            if is_formatting.get() {
                return;
            }
            is_formatting.set(true);

            buffer.remove_all_tags(&buffer.start_iter(), &buffer.end_iter());

            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .as_str()
                .to_string();

            let mut offset: i32 = 0;
            let mut inside_hash = false;
            let mut inside_code = false;

            for line in text.lines() {
                let line_len = line.len() as i32;
                let start = buffer.iter_at_offset(offset);
                let end = buffer.iter_at_offset(offset + line_len);

                if line.trim() == "#" {
                    inside_hash = !inside_hash;
                    if let Some(tag) = buffer.tag_table().lookup("hidden") {
                        buffer.apply_tag(&tag, &start, &end);
                    }
                } else if line.starts_with(">") {
                    inside_code = !inside_code;
                    if let Some(tag) = buffer.tag_table().lookup("hidden") {
                        let mut char_end = start.clone();
                        char_end.forward_char();
                        buffer.apply_tag(&tag, &start, &char_end);
                    }
                } else if inside_code {
                    if let Some(tag) = buffer.tag_table().lookup("codeprefix") {
                        buffer.apply_tag(&tag, &start, &end);
                    }
                } else if inside_hash {
                    if let Some(tag) = buffer.tag_table().lookup("large") {
                        buffer.apply_tag(&tag, &start, &end);
                    }
                }

                offset += line_len + 1;
            }

            // Autosave
            let _ = create_dir_all(format!("{}/.config/alt",&home_dir));
            let _ = write(&notes_path, &text);

            is_formatting.set(false);
        }
    };

    // -- Connect signal
    {
        let apply_formatting_clone = apply_formatting.clone();
        buffer_rc.connect_changed(move |_| {
            apply_formatting_clone();
        });
    }

    let buffer = buffer_rc.clone();

    buffer.connect_mark_set(move |_buffer, iter, mark| {
        if mark.name() == Some("insert".into()) {
            let mut iter = iter.clone();
            text_view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.0);
        }
    });
    apply_formatting();
    

}

fn create_icon_button(icon_name: &str, exec_command: String) -> Button {
    let image = Image::from_icon_name(icon_name);
    image.set_icon_size(gtk4::IconSize::Normal);

    let button = Button::builder()
        .child(&image)
        .tooltip_text(&exec_command)
        .build();

    button.connect_clicked(move |_| {
        let _ = Command::new("sh")
            .arg("-c")
            .arg(&exec_command)
            .spawn();
        exit(1)
    });

    button
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("alt.")
        .default_width(1000)
        .default_height(400)
        .resizable(false)
        .decorated(false)
        .build();
    
    let window1 = window.clone();
    let event_controller = gtk4::EventControllerKey::new();
    window.add_controller(event_controller.clone());

    let vbox = GtkBox::new(Orientation::Vertical, 5);

    let entry = Entry::builder()
        .placeholder_text(" Search apps, open marker or use alterAi")
        .hexpand(true)
        .activates_default(false)
        .build();
    entry.set_visible(false);

    let info_lable = Label::new(Some(""));
    info_lable.set_markup("<i>welcome to Alt.</i> \ntype `/` to get started");
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
    terminal_box.set_margin_start(7);
    terminal_box.set_margin_end(7);
    terminal_box.set_margin_top(4);
    terminal_box.set_margin_bottom(4);
    terminal_box.append(&terminal);
    terminal_box.set_visible(false);

    let alterai = GtkBox::new(Orientation::Vertical, 12);
    let alterai_closure = GtkBox::new(Orientation::Vertical, 12);
    let aiscroller = ScrolledWindow::new();
    aiscroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Never);
    aiscroller.add_css_class("no");
    let aiinfo = Label::new(Some("  alterAi has volatile memory,\n so it will forget your conversations when the application is closed"));
    let dummy = GtkBox::new(Orientation::Vertical, 0);
    dummy.set_vexpand(true);
    dummy.set_hexpand(true);

    aiinfo.set_margin_start(20);
    aiinfo.set_margin_end(20);
    aiinfo.set_margin_top(10);
    aiinfo.set_margin_bottom(30);
    aiinfo.set_widget_name("aiinfo");

    aiscroller.set_vexpand(true);
    aiscroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    
    alterai_closure.set_widget_name("alterai_box");
    alterai_closure.set_margin_start(10);
    alterai_closure.set_margin_end(10);
    alterai_closure.set_margin_top(10);
    alterai_closure.set_margin_bottom(10);
    alterai_closure.set_vexpand(false);

    let inpute = Entry::builder()
        .placeholder_text(" ask alterAi")
        .hexpand(true)
        .build();
    inpute.set_widget_name("aitry");

    alterai.append(&dummy);
    alterai.append(&aiinfo);
    alterai_closure.append(&alterai);
    alterai_closure.append(&inpute);
    aiscroller.set_child(Some(&alterai_closure));
    aiscroller.set_visible(false);

    let vbox_inner = GtkBox::new(Orientation::Vertical, 12);
    vbox_inner.set_hexpand(true);
    
    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_child(Some(&vbox_inner));

    let notescroller = ScrolledWindow::new();
    notescroller.set_size_request(400, 800);
    notescroller.set_vexpand(true);
    notescroller.set_hexpand(true);
    notescroller.add_css_class("no");
    notescroller.set_visible(false);

    let taskbar = GtkBox::new(Orientation::Horizontal, 0);
    taskbar.set_widget_name("taskbar");

    let moss = Label::new(Some("alt ●"));
    moss.set_widget_name("tasks");
    
    let vdummyl = GtkBox::new(Orientation::Vertical, 0);
    vdummyl.set_hexpand(true);

        let vdummyr = GtkBox::new(Orientation::Vertical, 0);
    vdummyr.set_hexpand(true);

    let run = Label::new(Some("run"));
    run.set_widget_name("keylabels");

    let altkey = Label::new(Some("L_ALT"));
    altkey.set_widget_name("keys");

    
    let quicklaunch = GtkBox::new(Orientation::Horizontal, 5);
    let qlpath = format!("/var/lib/cynager/ql.dat");
    let commands: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let commands_ql_launch = commands.clone();
     if let Ok(contents) = fs::read_to_string(qlpath) {
        let mut exec = None;
        let mut icon = None;

        for line in contents.lines() {
            if line.starts_with("Exec=") {
                exec = Some(line.trim_start_matches("Exec=").to_string());
            } else if line.starts_with("Icon=") {
                icon = Some(line.trim_start_matches("Icon=").to_string());
            }

            if let (Some(exec_val), Some(icon_val)) = (&exec, &icon) {
                let exec_clone = exec_val.clone();
                commands.borrow_mut().push(exec_clone.clone());

                let button = create_icon_button(&icon_val, exec_clone);
                quicklaunch.append(&button);

                exec = None;
                icon = None;
            }
        }
    }

    let ql_revealer = Revealer::builder()
        .transition_type(RevealerTransitionType::SwingDown)
        .transition_duration(300)
        .child(&quicklaunch)
        .reveal_child(true)
        .build();

    taskbar.append(&moss);
    taskbar.append(&vdummyl);
    taskbar.append(&ql_revealer);
    taskbar.append(&vdummyr);
    taskbar.append(&run);
    taskbar.append(&altkey);

    let ql_info = Label::new(Some(""));
    ql_info.set_widget_name("ql_info");
    ql_info.set_visible(false);

    vbox.append(&entry);
    vbox.append(&ql_info);
    vbox.append(&info_lable_revealer);
    vbox.append(&scroller);
    vbox.append(&terminal_box);
    vbox.append(&aiscroller);
    vbox.append(&notescroller);
    vbox.append(&taskbar);
    window.set_child(Some(&vbox));

    let info_lable_revealer = Rc::new(info_lable_revealer);
    let vbox_opt = Rc::new(vbox_inner);
    let ql_info = ql_info.clone();
    let selected_index = Rc::new(RefCell::new(0));
    let current_items = Rc::new(RefCell::new(Vec::new()));
    let current_mode = Rc::new(RefCell::new(Mode::App));
    let current_file_path_name= Rc::new(RefCell::new(Vec::new()));
    let terminal = Rc::new(terminal);
    let terminal_box = Rc::new(terminal_box);
    let _ai = false;

    {
        let vbox_opt = vbox_opt.clone();
        let selected_index = selected_index.clone();
        let current_items = current_items.clone();
        let current_mode = current_mode.clone();
        let current_file_path_name=current_file_path_name.clone();
        let info = info_lable.clone();

        entry.connect_changed(move |e| {
            let text_in_entry = &e.text().to_string();
            let (mode, items, file_path_name) = lister(&text_in_entry);
            let mut _res_flag = false;
            *current_mode.borrow_mut() = mode.clone();
            *current_items.borrow_mut() = items.clone();
            *current_file_path_name.borrow_mut() = file_path_name.clone();
            *selected_index.borrow_mut() = 0;

            // info flags
            if text_in_entry == "~t"{
                let now: DateTime<Local> = Local::now();
                let datetime_am_pm = now.format(" %B %e,\n%l:%M %P").to_string();
                info.set_text(&datetime_am_pm);
                _res_flag = true;
            } else if text_in_entry == "~b"{
                let bat = format!("/sys/class/power_supply/BAT1/capacity");
                let path= Path::new(&bat);
                if path.exists(){
                    let cap = fs::read_to_string(bat)
                    .expect("Failed to read battery capacity")
                    .trim().to_string();
                    let n:u64=cap.parse().unwrap();
                    if n > 80 {
                        info.set_markup(&format!("Battery capacity, {}%\n<i>\"At battery nirvana.\"</i>",cap));
                        info.set_widget_name("batg");
                    } else if n < 80 && n > 50 {
                        info.set_markup(&format!("Battery capacity, {}%\n<i>\"In power harmony.\"</i>",cap));
                        info.set_widget_name("batg");
                    } else if n < 50 && n > 20 {
                        info.set_markup(&format!("Battery capacity, {}%\n<i>\"The border of zone efficiency.\"</i>",cap));
                    } else {
                        info.set_markup(&format!("Battery capacity, {}%\n<i>\"Depleted juice.\"</i>",cap));
                        info.set_widget_name("batb");
                    }
                }
                _res_flag = true;
            } else if text_in_entry == "`"{ 
                info.set_markup("<i>~t</i> for Time\n<i>~b</i> for Battery\n<i>~m</i> for aiNotes (/s in alterAi)");
                _res_flag = true;
            } else if text_in_entry == "!"{
                info.set_text("");
                typing_effect(&info, "Ask\nalterAi", 50);
                _res_flag = true;
                info.set_widget_name("info_lable-card");
            } else {
                info.set_text(" type\n ` for flags\n ! for alterAi");
                _res_flag = false;
                info.set_widget_name("info_lable-card");
            }

            info_lable_revealer.set_reveal_child(text_in_entry.is_empty() || _res_flag);

            if text_in_entry.is_empty() || _res_flag {
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
                label.set_hexpand(true);
                label.set_halign(gtk4::Align::Fill);
                label.set_xalign(0.0);
                label.set_margin_start(20);
                label.set_margin_end(20);
                label.set_margin_bottom(20);
                vbox_opt.append(&label);
            }
        });
    }

    {   
        let first_press = Cell::new(true);
        event_controller.connect_key_pressed(move |_, key, _, _| {
            let mut index = selected_index.borrow_mut();
            let items = current_items.borrow();
            let file_path_name=current_file_path_name.borrow();
            let adj = scroller.vadjustment();
            match key {
                gtk4::gdk::Key::Down => {
                    if *index + 1 < items.len() {
                        *index += 1;
                        let target_scroll = (*index as f64) * 50.0;
                        adj.set_value(target_scroll);
                    }
                }
                gtk4::gdk::Key::Up => {
                    if *index > 0 {
                        *index -= 1;
                        let target_scroll = (*index as f64) * 50.0;
                        adj.set_value(target_scroll);
                    }
                }
                gtk4::gdk::Key::_1
                | gtk4::gdk::Key::_2
                | gtk4::gdk::Key::_3
                | gtk4::gdk::Key::_4
                | gtk4::gdk::Key::_5
                | gtk4::gdk::Key::_6
                | gtk4::gdk::Key::_7
                | gtk4::gdk::Key::_8
                | gtk4::gdk::Key::_9 => {
                    let commands = commands_ql_launch.borrow();
                    if let Some(name) = key.name() {
                        let digit_str = name.trim();

                        if let Ok(index) = digit_str.parse::<usize>() {
                            println!("You pressed: {}", index);
                            if index >= 1 && index <= commands.len() {
                                let command = &commands[index - 1];
                                let _ = std::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(command)
                                    .spawn();
                                exit(1);
                            }
                        }
                    }                  
                }
                gtk4::gdk::Key::Tab => {
                    if let Some(file_name) = file_path_name.get(*index) {
                        let path_str = format!("{}",&file_name);  
                        let path = Path::new(&path_str);
                        if let Ok(contents) = fs::read_to_string(&path) {
                            let mut exec_line = None;
                            let mut in_desktop_entry = false;
                            let mut icon = None;

                            for line in contents.lines() {
                                let trimmed = line.trim();
                                if trimmed.starts_with('[') {
                                    in_desktop_entry = trimmed == "[Desktop Entry]";
                                } else if in_desktop_entry && trimmed.starts_with("Exec=") {
                                    exec_line = Some(trimmed.trim_start_matches("Exec=").to_string().split_whitespace()
                                        .map(|arg| if arg.starts_with('%') { "" } else { arg })
                                        .collect::<Vec<_>>()
                                        .join(" "));
                                } else if in_desktop_entry && trimmed.starts_with("Icon=") {
                                    icon = Some(trimmed.trim_start_matches("Icon=").to_string());
                                }
                            }


                             if let (Some(exec), Some(icon)) = (exec_line, icon) {
                                let home_dir = std::env::var("HOME").unwrap_or_default();
                                let qlpath: std::path::PathBuf = [home_dir.as_str(), ".config/alt/ql.dat"].iter().collect();
                                let ql_info = ql_info.clone();

                                let mut existing = String::new();
                                if let Ok(mut file) = OpenOptions::new().read(true).open(&qlpath) {
                                    file.read_to_string(&mut existing).unwrap_or_default();
                                }

                                let new_entry = format!("Exec={}\nIcon={}", exec, icon);

                                let mut lines: Vec<&str> = existing.lines().collect();
                                let mut i = 0;
                                while i < lines.len() {
                                    if lines[i].starts_with("Exec=") && lines[i] == format!("Exec={}", exec) {
                                        if i + 1 < lines.len() && lines[i + 1] == format!("Icon={}", icon) {
                                            lines.drain(i..=i + 1);
                                            fs::write(&qlpath, lines.join("\n")).expect("Failed to write after removing");
                                            ql_info.set_text("removed from quickLaunch");
                                            ql_info.set_visible(true);
                                            glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                                                ql_info.set_visible(false);
                                            });
                                            return glib::Propagation::Proceed;
                                        }
                                    }
                                    i += 1;
                                }

                                lines.push("");
                                lines.push(&new_entry);

                                fs::write(&qlpath, lines.join("\n")).expect("Failed to write to ql.dat");
                                ql_info.set_text("added to quickLaunch");
                                ql_info.set_visible(true);
                                glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                                    ql_info.set_visible(false);
                                });
                            }
                        }
                    }                    
                }
                gtk4::gdk::Key::Alt_L => {
                    if let Some(file_name) = file_path_name.get(*index) {
                        let path_str = format!("{}",&file_name);  
                        let path = Path::new(&path_str);
                        if let Ok(contents) = fs::read_to_string(&path) {
                            let mut exec_line = None;
                            let mut terminal_flag = false;
                            let mut in_desktop_entry = false;

                            for line in contents.lines() {
                                let trimmed = line.trim();
                                if trimmed.starts_with('[') {
                                    in_desktop_entry = trimmed == "[Desktop Entry]";
                                } else if in_desktop_entry && trimmed.starts_with("Exec=") {
                                    exec_line = Some(trimmed.trim_start_matches("Exec=").to_string());
                                } else if in_desktop_entry && line.starts_with("Terminal=") {
                                    terminal_flag = line.trim_start_matches("Terminal=").trim() == "true";
                                }
                            }
                            
                            if let Some(exec) = exec_line {
                                if terminal_flag {
                                    entry.set_visible(false);
                                    scroller.set_visible(false);
                                    terminal_box.set_visible(true);

                                    let command = &exec;
                                    let argv = ["sh", "-c", command];

                                    run.set_markup(&format!("<i>{}</i>",command));
                                    altkey.set_visible(false);

                                    terminal.spawn_async(
                                        PtyFlags::DEFAULT,
                                        None,      
                                        &argv,            
                                        &[],            
                                        SpawnFlags::DEFAULT,
                                        || {},               
                                        -1,                      
                                        None::<&Cancellable>,  
                                        move |res: Result<Pid, Error>| {
                                            if let Err(e) = res {
                                                eprintln!("Failed to spawn terminal process: {}", e);
                                            }
                                        },
                                    );
                                    let terminal_box_clone = terminal_box.clone();
                                    let entry_clone = entry.clone();
                                    let scroller_clone=scroller.clone();
                                    let run_clone = run.clone();
                                    let altkey_clone = altkey.clone();

                                    terminal.connect_child_exited(move |_terminal, _status| {
                                        terminal_box_clone.set_visible(false);
                                        entry_clone.set_visible(true);
                                        scroller_clone.set_visible(true);
                                        run_clone.set_text("run");
                                        altkey_clone.set_visible(true);
                                    });
                                } else {
                                    let sanitized_command = exec
                                        .split_whitespace()
                                        .map(|arg| if arg.starts_with('%') { "" } else { arg })
                                        .collect::<Vec<_>>()
                                        .join(" ");

                                    if !sanitized_command.is_empty() {
                                        print!("{}", sanitized_command);
                                        if let Err(e) = Command::new("sh")
                                            .arg("-c")
                                            .arg(sanitized_command)
                                            .spawn()
                                        {
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
                        } else if matches!(*current_mode.borrow(), Mode::Ai) && !aiscroller.get_visible() {
                            if entry.text() == "!!"{
                                entry.set_visible(false);
                                scroller.set_visible(false);
                                terminal_box.set_visible(true);

                                let space = r#"
                                    #!/bin/bash

                                    # Terminal size

                                    sleep 1s
                                    cols=$(tput cols)
                                    rows=$(tput lines)

                                    center_x=$((cols / 2))
                                    center_y=$((rows / 2))

                                    # Number of stars
                                    num_stars=200

                                    # Each star has a position and speed
                                    declare -a stars_x
                                    declare -a stars_y
                                    declare -a stars_z

                                    max_depth=20

                                    # Initialize stars with random positions in 3D space
                                    for ((i=0; i<num_stars; i++)); do
                                    stars_x[i]=$(( RANDOM % (cols) - center_x ))
                                    stars_y[i]=$(( RANDOM % (rows) - center_y ))
                                    stars_z[i]=$(( RANDOM % max_depth + 1 ))
                                    done

                                    # Hide cursor
                                    tput civis

                                    clear

                                    while true; do
                                    # Clear screen buffer as associative array to store characters by coords
                                    declare -A screen=()

                                    for ((i=0; i<num_stars; i++)); do
                                        # Move star closer (simulate flying forward)
                                        (( stars_z[i]-- ))

                                        # Reset star when it passes you
                                        if (( stars_z[i] <= 0 )); then
                                        stars_x[i]=$(( RANDOM % (cols) - center_x ))
                                        stars_y[i]=$(( RANDOM % (rows) - center_y ))
                                        stars_z[i]=$max_depth
                                        fi

                                        # Project 3D point to 2D screen (simple perspective projection)
                                        sx=$(( center_x + stars_x[i] * max_depth / stars_z[i] ))
                                        sy=$(( center_y + stars_y[i] * max_depth / stars_z[i] ))

                                        # Check if projected point is inside screen
                                        if (( sx >= 0 && sx < cols && sy >= 0 && sy < rows )); then
                                        # Star brightness depends on depth (closer stars are brighter)
                                        brightness=$(( (max_depth - stars_z[i]) * 3 / max_depth ))

                                        # Choose star char by brightness
                                        case $brightness in
                                            0) char='.' ;;
                                            1) char='*' ;;
                                            2) char='o' ;;
                                            3) char='@' ;;
                                            *) char='.' ;;
                                        esac

                                        screen["$sy,$sx"]=$char
                                        fi
                                    done

                                    # Render frame
                                    clear
                                    for ((y=0; y<rows; y++)); do
                                        line=""
                                        for ((x=0; x<cols; x++)); do
                                        if [[ -n "${screen[$y,$x]}" ]]; then
                                            line+=${screen[$y,$x]}
                                        else
                                            line+=" "
                                        fi
                                        done
                                        echo "$line"
                                    done

                                    # Frame delay
                                    sleep 0.03
                                    done

                                    # Show cursor on exit
                                    trap "tput cnorm; clear; exit" SIGINT SIGTERM

                                "#;

                                let command = &space;
                                let argv = ["sh", "-c", command];

                                terminal.spawn_async(
                                    PtyFlags::DEFAULT,
                                    None,      
                                    &argv,            
                                    &[],            
                                    SpawnFlags::DEFAULT,
                                    || {},               
                                    -1,                      
                                    None::<&Cancellable>,  
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
                                entry.set_visible(false);
                                scroller.set_visible(false);
                                info_lable.set_visible(false);
                                taskbar.set_visible(false);
                                aiscroller.set_visible(true);
                                inpute.grab_focus();
                                let chat_history: Rc<RefCell<Vec<ChatCompletionMessage>>> = Rc::new(RefCell::new(vec![]));
                                let inp = inpute.clone();
                                let sr = scroller.clone();
                                let ent = entry.clone();
                                let ai = aiscroller.clone();
                                let info = info_lable.clone();
                                let clo= alterai_closure.clone();
                                let aiclo = alterai.clone();
                                let aiinfo = aiinfo.clone();
                                let tb = taskbar.clone();
                                inpute.connect_activate(move |e| {
                                    let input = e.text().to_string();
                                    let history = chat_history.clone();
                                    let api_key = read_api_key().to_string();
                                    if input.to_lowercase() == "/e"{
                                        ent.set_visible(true);
                                        sr.set_visible(true);
                                        info.set_visible(true);
                                        tb.set_visible(true);
                                        ai.set_visible(false);
                                    } else if input.to_lowercase() == "/s" {
                                        if let Some(last) = chat_history.borrow().last() {
                                            let home_dir = std::env::var("HOME").unwrap_or_default();
                                            let notes_path: std::path::PathBuf = [home_dir.as_str(), ".config/alt/markers/notes.algae"]
                                                .iter()
                                                .collect();
                                            let mut make_a_note = OpenOptions::new()
                                                .create(true)  
                                                .append(true)      
                                                .open(notes_path)
                                                .expect("Failed to open file");

                                            let now: DateTime<Local> = Local::now();
                                            let datetime_am_pm = now.format("%B %e, %l:%M %P").to_string();
                                            let line="_".repeat(50);
                                            make_a_note
                                                .write_all(format!("\n{}\n>\n alterAi \n>\n{}\n\n{}\n{}\n",line, datetime_am_pm, strip_markdown_symbols(&last.content), line).as_bytes())
                                                .expect("Failed to write to file");

                                            inp.set_text("");
                                            let save_message = Label::new(Some("Saved Previous reply"));
                                            save_message.set_widget_name("save_info");
                                            save_message.set_max_width_chars(20);
                                            save_message.set_halign(gtk4::Align::Center);
                                            aiclo.append(&save_message);
                                        }
                                    } else {
                                        history.borrow_mut().push(ChatCompletionMessage {
                                            role: ChatCompletionRoles::User,
                                            content: input.clone(),
                                            name: None,
                                        });

                                        let airep = Label::new(Some(""));
                                        airep.set_wrap(true);
                                        airep.set_max_width_chars(80);
                                        airep.set_halign(gtk4::Align::Start);
                                        airep.set_margin_start(20);
                                        airep.set_margin_end(100);
                                        airep.set_margin_top(0);
                                        airep.set_margin_bottom(10);
                                        airep.set_selectable(true);
                                        airep.set_widget_name("ai_reply");
                                        
                                        let user_inp = Label::new(Some(""));
                                        user_inp.set_wrap(true);
                                        user_inp.set_max_width_chars(50);
                                        user_inp.set_halign(gtk4::Align::End);
                                        user_inp.set_margin_start(100);
                                        user_inp.set_margin_end(20);
                                        user_inp.set_margin_top(0);
                                        user_inp.set_margin_bottom(10);
                                        user_inp.set_widget_name("user_inp");
                                        user_inp.set_text(&input);

                                        aiclo.append(&user_inp);
                                        aiclo.append(&airep);
                                        
                                        let clo = clo.clone();
                                        let ai = ai.clone();
                                        let inp = inp.clone();
                                        let aiinfo = aiinfo.clone();
                                        let aiclo = aiclo.clone();
                                        glib::MainContext::default().spawn_local(async move {
                                            let messages = history.borrow().clone();
                                            let client = AsyncGroqClient::new(api_key, None).await;
                                            let request = ChatCompletionRequest::new("llama-3.3-70b-versatile", messages);

                                            match client.chat_completion(request).await {
                                                Ok(response) => {
                                                    let ai_reply = &response.choices[0].message.content;

                                                    history.borrow_mut().push(ChatCompletionMessage {
                                                        role: ChatCompletionRoles::Assistant,
                                                        content: ai_reply.clone(),
                                                        name: None,
                                                    });
                                                    

                                                    ai_typing_effect(&airep, &strip_markdown_symbols(&ai_reply), 5, &ai, &clo);

                                                }
                                                Err(err) => {
                                                    eprintln!("AI error: {}", err);
                                                    aiclo.remove(&user_inp);
                                                    aiclo.remove(&airep);
                                                    aiinfo.set_markup("Err:<i>404</i>\nconnection could not be Established");
                                                    let vadj = ai.vadjustment();
                                                    vadj.set_value(vadj.lower());
                                                    aiinfo.set_visible(true);
                                                }
                                            }
                                            inp.set_text("");
                                        });
                                    }
                                });
                            }
                        } else if matches!(*current_mode.borrow(), Mode::Notes) {
                            entry.set_visible(false);
                            scroller.set_visible(false);
                            info_lable.set_visible(false);
                            notescroller.set_visible(true);
                            altkey.set_visible(false);
                            run.set_markup("<i>marker.</i>");
                            torq_marker(&notescroller);
                            window1.set_resizable(true);
                            window1.set_title(Some("marker"));
                        } else {
                            eprintln!("Failed to read .desktop file: {}", path_str);
                        }
                    } else {
                        eprintln!("Failed to read application path")
                    }
                }
                gtk4::gdk::Key::slash => {
                    entry.set_visible(true);
                    ql_revealer.set_reveal_child(false);
                    entry.grab_focus();
                    first_press.set(false);
                    info_lable.set_text(" type\n` for flags\n ! for alterAi");
                }
                gtk4::gdk::Key::Escape => {
                    exit(0);
                }
                _ => {
                    print!("{}",first_press.get());
                    if first_press.get() {
                        ql_revealer.set_reveal_child(false);
                        first_press.set(false);
                        entry.set_visible(true);
                        entry.grab_focus();                  
                    }                      
                    return glib::Propagation::Proceed;
                }
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
    
    let css = r#"
        window {
            background-color: rgba(8, 15, 11, 0.56);
            border-radius: 15px;
            border-style: solid;
            border-width: 2px ;
            border-color: rgba(73, 73, 73, 0.59);
        }
        .textview-style {
            border-radius: 10px;
            padding: 80px;
            background-color: rgb(117, 197, 121);
            border-bottom: 2.5px solid rgb(69, 116, 71);
            color: black;
            font-size: 15px;
        }
        .textview-style text {
            background-color: rgb(117, 197, 121);
        }
        #info_lable-card {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 100px;
            border-radius: 12px;
            font-size: 50px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 900;
        }
        #image-card {
            background-color: rgba(139, 139, 139, 0.14);
            border-radius: 12px;
            opacity: 0.5;
            border: 1px solid rgba(139, 139, 139, 0.59);
        }
        #batg {
            background-color: rgba(117, 197, 121, 0.21);
            padding: 100px;
            border-radius: 12px;
            font-size: 40px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 700;
        }
        #batb {
            background-color: rgba(243, 129, 129, 0.14);
            padding: 100px;
            border-radius: 12px;
            font-size: 40px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 700;
        }
        entry {
            all: unset;
            border-bottom: 1px solid rgba(73, 73, 73, 0.59);
            padding: 20px;
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
        #alterai_box {
            background-color: rgba(0, 0, 0, 0.14);
            border-radius: 12px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
        }
        #ai_reply {
            background-color: rgba(1, 78, 14, 0.27);
            padding: 10px;
            border-radius: 18px;
            font-size: 16px;
            border: 0.5px solid rgba(217, 255, 208, 0.59);
            font-family: "Cantarell";
            font-weight: 500;
        }
        #user_inp {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 10px;
            border-radius: 18px;
            font-size: 16px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            box-shadow: 0px 0px 5px rgba(0, 0, 0, 0.26);
            font-family: "Cantarell";
            font-weight: 600;
        }
        #ql_info {
            padding: 10px;
            font-size: 12px;
            font-family: "Cantarell";
            font-weight: 600;
            background-color: rgba(139, 139, 139, 0.14);
        }
        #save_info {
            padding: 10px;
            font-size: 12px;
            font-family: "Cantarell";
            font-weight: 200;
        }
        #aitry{
            border-top: 0.5px solid rgba(139, 139, 139, 0.59);
            padding: 20px;
            padding-left: 15px;
            border-bottom-left-radius: 12px;
            border-bottom-right-radius: 12px;
            background-color: rgba(0, 0, 0, 0.1);
            color: rgba(224, 222, 222, 0.9);
            box-shadow:none;
            font-family: "Cantarell";
            font-weight: 400; 
        }
        #aiinfo {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 80px;
            border-radius: 12px;
            font-size: 30px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Adwaita Sans";
            font-weight: 700;
        }
        .no scrollbar {
            opacity: 0;
            min-width: 0;
            min-height: 0;
        }   
        #taskbar {
            border-top: 0.5px solid rgba(139, 139, 139, 0.39);
            padding: 10px;
            padding-left: 15px;
            border-bottom-left-radius: 12px;
            border-bottom-right-radius: 12px;
            background-color: rgba(139, 139, 139, 0.14);
            box-shadow:none;
        }
        #tasks {
            font-family: "Cantarell";
            font-size: 16px;
            line-height: 0.9;
            font-weight: 900; 
            color: rgba(139, 139, 139, 0.59);
        }
        #keylabels {
            font-family: "Cantarell";
            font-weight: 900; 
            color: rgba(139, 139, 139, 0.59);
            font-size: 14px;
            padding: 5px;
        }
        #keys {
            background-color: rgba(124, 124, 124, 0.14);
            font-family: "Cantarell";
            font-weight: 900; 
            color: rgba(139, 139, 139, 0.59);
            font-size: 12px;
            padding: 5px;
            border-radius: 5px;  
            border-bottom: 0.3px solid rgba(139, 139, 139, 0.59);
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

fn collect_desktop_apps<P: AsRef<Path>>(dir: P, pairs: &mut Vec<(String, String)>) {
    if let Ok(read_dir) = fs::read_dir(dir) {
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
}

fn lister(input: &str) -> (Mode, Vec<String>, Vec<String>) {
    if input.starts_with("~m") {
        return (Mode::Notes, vec![format!("Open Marker")], vec![input.to_string()]);
    }

    if input.starts_with('!') {
        return (
            Mode::Ai,
            vec![format!("Enter chat")],
            vec![input.to_string()],
        );
    }

    let mut pairs = Vec::new();
    collect_desktop_apps("/usr/share/applications", &mut pairs);
    collect_desktop_apps("/var/lib/flatpak/exports/share/applications", &mut pairs);

    if input.starts_with('~') {
        if input.len() > 1 {
            return (Mode::None, Vec::new(), Vec::new());
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
