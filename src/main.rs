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
use groq_api_rust::{GroqClient, ChatCompletionRequest, ChatCompletionRoles, ChatCompletionMessage};
fn main() {
    let app = Application::builder()
        .application_id("ekah.scu.moss")
        .build();

    app.connect_activate(build_ui);
    // Command::new("bash").arg("-c").arg("cd").arg("~/");
    app.run();
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
    let key_path = format!("{}/.config/moss/key.dat", home_dir);
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

    let entry = Entry::builder()
        .placeholder_text(" Search apps, open marker or use alterAi")
        .hexpand(true)
        .activates_default(false)
        .build();
    entry.set_visible(false);

    let info_lable = Label::new(Some(""));
    info_lable.set_markup(" <i>Moss</i> \ntype `/` to get started");
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

    let alterai_closure = GtkBox::new(Orientation::Vertical, 12);
    let aiscroller = ScrolledWindow::new();
    let reply = Label::new(Some(""));
    let aiinfo = Label::new(Some("alterAi has memory loss,\n so it will not be able to remember conversations."));
    aiscroller.set_vexpand(true);
    aiscroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    
    alterai_closure.set_widget_name("alterai_box");
    alterai_closure.set_margin_start(10);
    alterai_closure.set_margin_end(10);
    alterai_closure.set_margin_top(10);
    alterai_closure.set_margin_bottom(10);
    alterai_closure.set_vexpand(false);

    reply.set_widget_name("ai_reply");
    reply.set_wrap(true);
    reply.set_max_width_chars(50);
    reply.set_xalign(0.0);
    reply.set_margin_start(20);
    reply.set_margin_end(20);
    reply.set_margin_top(20);
    reply.set_margin_bottom(20);
    reply.set_visible(false);

    let inpute = Entry::builder()
        .placeholder_text("ask alterAi")
        .hexpand(true)
        .build();
    inpute.set_widget_name("aitry");

    alterai_closure.append(&aiinfo);
    alterai_closure.append(&reply);
    alterai_closure.append(&inpute);
    aiscroller.set_child(Some(&alterai_closure));
    aiscroller.set_visible(false);

    let vbox_inner = GtkBox::new(Orientation::Vertical, 12);
    let scroller = ScrolledWindow::new();
    scroller.set_vexpand(true);
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_child(Some(&vbox_inner));

    vbox.append(&entry);
    vbox.append(&info_lable_revealer);
    vbox.append(&scroller);
    vbox.append(&terminal_box);
    vbox.append(&aiscroller);
    window.set_child(Some(&vbox));

    let info_lable_revealer = Rc::new(info_lable_revealer);
    let vbox_opt = Rc::new(vbox_inner);
    let selected_index = Rc::new(RefCell::new(0));
    let current_items = Rc::new(RefCell::new(Vec::new()));
    let current_mode = Rc::new(RefCell::new(Mode::App));
    let current_file_path_name= Rc::new(RefCell::new(Vec::new()));
    let terminal = Rc::new(terminal);
    let terminal_box = Rc::new(terminal_box);
    let _ai = false;


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
            } else if text_in_entry == "!"{
                info.set_text("");
                typing_effect(&info, "Ask\nalterAi", 50);
                _res_flag = true;
                info.set_widget_name("info_lable-card");
            } else if text_in_entry.contains("!"){
                 _res_flag = true;
            } else {
                info.set_text(" type\n` for marker\n ! for alterAi");
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
        let current_mode = current_mode.clone();
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
                            } else if matches!(*current_mode.borrow(), Mode::Ai) {
                                entry.set_visible(false);
                                scroller.set_visible(false);
                                info_lable.set_visible(false);
                                aiscroller.set_visible(true);
                                inpute.grab_focus();
                                let qs = Rc::new(RefCell::new(String::new()));
                                let qs_clone = qs.clone();
                                let inp = inpute.clone();
                                let rep = reply.clone();
                                let sr = scroller.clone();
                                let ent = entry.clone();
                                let ai = aiscroller.clone();
                                let info = info_lable.clone();
                                let clo= alterai_closure.clone();
                                let aiinfo = aiinfo.clone();
                                inpute.connect_activate(move |e| {
                                    aiinfo.set_visible(true);
                                    let input = e.text().to_string();
                                    *qs_clone.borrow_mut() = input.clone();
                                    let qs_later = qs.clone();
                                    let api_key = read_api_key().to_string();
                                    let qss = qs_later.borrow();
                                    if qss.starts_with("exit"){
                                        ent.set_visible(true);
                                        sr.set_visible(true);
                                        info.set_visible(true);
                                        ai.set_visible(false);
                                    }
                                    info.set_text("Thinking");
                                    let client = GroqClient::new(api_key.to_string(), None);
                                    let messages = vec![ChatCompletionMessage {
                                        role: ChatCompletionRoles::User,
                                        content: qss.to_string(),
                                        name: None,
                                    }];
                                    let request = ChatCompletionRequest::new("llama3-70b-8192", messages);
                                    let response = client.chat_completion(request).unwrap();
                                    info.set_visible(false);
                                    rep.set_visible(true);
                                    ai_typing_effect(&rep, &strip_markdown_symbols(&response.choices[0].message.content), 5, &ai,&clo);
                                    assert!(!response.choices.is_empty());
                                    inp.set_text("");
                                });
                                
                            } else {
                                eprintln!("Failed to read .desktop file: {}", path_str);
                            }
                        } else {
                            eprintln!("Failed to read application path")
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
        #alterai_box {
            background-color: rgba(139, 139, 139, 0.14);
            border-radius: 12px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
        }
        #ai_reply {
            background-color: rgba(139, 139, 139, 0.14);
            padding: 10px;
            border-radius: 18px;
            font-size: 16px;
            border: 0.5px solid rgba(139, 139, 139, 0.59);
            font-family: "Cantarell";
            font-weight: 500;
        }
        #aitry{
            border-top: 1px solid rgba(139, 139, 139, 0.59);
            padding: 10px;
            padding-left: 15px;
            border-bottom-right-radius: 12px;
            border-bottom-left-radius: 12px;
            background-color: rgba(0, 0, 0, 0.1);
            color: rgba(224, 222, 222, 0.9);
            box-shadow:none;
            font-family: "Cantarell";
            font-weight: 400; 
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
        return (
            Mode::Ai,
            vec![
                format!("Ask alterAi."),
            ],
            vec![
                input.to_string()
            ]
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

    if input.starts_with('~') {
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
