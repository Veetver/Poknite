#![cfg_attr(windows, windows_subsystem = "windows")]
mod chat_ui;
#[cfg(target_os = "linux")]
mod dbus;
mod encryption_ui;
#[cfg(target_os = "linux")]
mod linux_tray;
mod platform;
#[cfg(windows)]
mod tray;
mod v2_ui;
#[cfg(windows)]
mod windows_identity;
use anyhow::{Context, Result, bail};
use fltk::{
    app,
    browser::HoldBrowser,
    button::{Button, CheckButton},
    enums::{Align, CallbackTrigger, Color, Event, Font, Key, Shortcut},
    frame::Frame,
    group::{Group, Tabs},
    input::{Input, MultilineInput, SecretInput},
    prelude::*,
    text::{StyleTableEntry, TextBuffer, TextDisplay},
    window::Window,
};
use poknite_client::{Api, ConnectionStatus, Profile, Store, Update};
use poknite_protocol::{Channel, Device, Permission, PublishRequest, User};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};

#[derive(Clone)]
struct Options {
    directory: PathBuf,
    ca: Option<PathBuf>,
    dev_http: bool,
    background: bool,
    headless: Option<String>,
    server: Option<String>,
    invitation: Option<String>,
    name: String,
    text_file: Option<PathBuf>,
    key_file: Option<PathBuf>,
    channel: i64,
    device: i64,
    seconds: u64,
    no_notify: bool,
}
impl Options {
    fn parse() -> Result<Self> {
        let mut o = Self {
            directory: poknite_client::store::default_directory(),
            ca: None,
            dev_http: false,
            background: false,
            headless: None,
            server: None,
            invitation: None,
            name: std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "Компьютер".into()),
            text_file: None,
            key_file: None,
            channel: 1,
            device: 0,
            seconds: 30,
            no_notify: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let next = |args: &mut std::iter::Skip<std::env::Args>| -> Result<String> {
                args.next().context("Не хватает значения аргумента")
            };
            match arg.as_str() {
                "--data-dir" => o.directory = next(&mut args)?.into(),
                "--ca" => o.ca = Some(next(&mut args)?.into()),
                "--dev-http" => o.dev_http = true,
                "--background" => o.background = true,
                "--headless" => o.headless = Some(next(&mut args)?),
                "--server" => o.server = Some(next(&mut args)?),
                "--invitation-file" => {
                    o.invitation = Some(std::fs::read_to_string(next(&mut args)?)?.trim().into())
                }
                "--name" => o.name = next(&mut args)?,
                "--text-file" => o.text_file = Some(next(&mut args)?.into()),
                "--key-file" => o.key_file = Some(next(&mut args)?.into()),
                "--channel" => o.channel = next(&mut args)?.parse()?,
                "--device" => o.device = next(&mut args)?.parse()?,
                "--seconds" => o.seconds = next(&mut args)?.parse()?,
                "--no-notify" => o.no_notify = true,
                "--help" => {
                    println!(
                        "Poknite\n  --background\n  --data-dir PATH\n  --ca PEM\n  --dev-http (только loopback)\n  --headless enroll --server URL --invitation-file PATH [--name NAME]\n  --headless run [--seconds 30] [--no-notify]\n  --headless send --channel ID --text-file PATH\n  --headless diagnose --server URL\n  --headless history|status|devices|clear\n  --headless revoke --device ID\n  --headless key-create|key-export|key-import --channel ID --key-file PATH"
                    );
                    std::process::exit(0)
                }
                _ => bail!("Неизвестный аргумент: {arg}"),
            }
        }
        Ok(o)
    }
}
#[derive(Clone)]
enum Ui {
    E2eeMembers(
        i64,
        std::result::Result<Vec<poknite_protocol::e2ee::Member>, String>,
    ),
    Core(Update),
    V2(v2_ui::Event),
    Enrolled(std::result::Result<Profile, String>),
    Published(
        i64,
        String,
        std::result::Result<poknite_protocol::Message, String>,
    ),
    Devices(std::result::Result<Vec<Device>, String>),
    Revoked(std::result::Result<(), String>),
    NotificationResult(std::result::Result<(), String>),
    Open,
    Quit,
    #[cfg(target_os = "linux")]
    TrayContext,
}
struct State {
    options: Options,
    store: Arc<Store>,
    profile: Option<Profile>,
    api: Option<Api>,
    runtime: tokio::runtime::Runtime,
    job: Option<tokio::task::JoinHandle<()>>,
    restart: watch::Sender<u64>,
    status: ConnectionStatus,
    channels: Vec<Channel>,
    devices: Vec<Device>,
    user: Option<User>,
    contacts: Vec<User>,
    editing: Option<serde_json::Value>,
    rule_role: i64,
    selected: i64,
    offset: usize,
    draft_generation: u64,
    follow_latest: bool,
}
impl State {
    fn start(&mut self, sender: app::Sender<Ui>) -> Result<()> {
        if let Some(job) = self.job.take() {
            job.abort()
        }
        let Some(profile) = self.profile.clone() else {
            return Ok(());
        };
        let api = Api::new(
            &profile.server,
            self.options.dev_http,
            self.options.ca.as_deref(),
        )?;
        self.api = Some(api.clone());
        let store = self.store.clone();
        let restart = self.restart.subscribe();
        let (tx, mut rx) = mpsc::channel(64);
        self.job = Some(self.runtime.spawn(async move {
            let bridge = tokio::spawn(async move {
                while let Some(update) = rx.recv().await {
                    sender.send(Ui::Core(update));
                }
            });
            api.run(profile, store, tx, restart).await;
            let _ = bridge.await;
        }));
        Ok(())
    }
}
fn error(text: impl ToString) {
    fltk::dialog::alert_default(&text.to_string())
}
fn refresh_history(state: &State, buffer: &mut TextBuffer, display: &mut TextDisplay) {
    match state.store.history(state.selected, state.offset) {
        Ok(messages) => {
            let mut text = String::new();
            let mut styles = String::new();
            let mut table = vec![StyleTableEntry {
                color: Color::Foreground,
                font: Font::Helvetica,
                size: 14,
            }];
            for m in messages.into_iter().rev() {
                let color = u32::from_str_radix(m.sender_color.trim_start_matches('#'), 16)
                    .unwrap_or(0x808080);
                let mark = (b'A' + table.len() as u8) as char;
                table.push(StyleTableEntry {
                    color: Color::from_rgb((color >> 16) as u8, (color >> 8) as u8, color as u8),
                    font: Font::HelveticaBold,
                    size: 14,
                });
                let title = format!("{}\n", m.sender_name);
                let body = format!("{}\n\n", m.text);
                styles.extend(std::iter::repeat_n(mark, title.len()));
                styles.extend(std::iter::repeat_n('A', body.len()));
                text.push_str(&title);
                text.push_str(&body);
            }
            let top = display.get_absolute_top_line_number();
            if text.is_empty() {
                text = if state.selected == 0 {
                    "Выберите канал или личный диалог слева".into()
                } else {
                    "Здесь пока нет сообщений. Начните разговор.".into()
                };
                styles = "A".repeat(text.len());
            }
            let mut style = TextBuffer::default();
            style.set_text(&styles);
            display.set_highlight_data(style, table);
            if buffer.text() != text {
                buffer.set_text(&text);
            }
            if state.follow_latest && state.offset == 0 {
                display.set_insert_position(buffer.length());
                display.show_insert_position();
            } else {
                display.scroll(top, 0);
            }
        }
        Err(e) => error(e),
    }
}
fn main() {
    if let Err(e) = run() {
        #[cfg(windows)]
        fltk::dialog::alert_default(&e.to_string());
        #[cfg(not(windows))]
        eprintln!("Poknite: {e}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let options = Options::parse()?;
    if options.headless.as_deref() == Some("diagnose") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let api = Api::new(
            options.server.as_deref().context("Нужен --server")?,
            options.dev_http,
            options.ca.as_deref(),
        )?;
        println!("{}", runtime.block_on(api.diagnostics()).join("\n"));
        return Ok(());
    }
    let store = Arc::new(Store::open(&options.directory)?);
    if options.headless.is_some() {
        return headless(options, store);
    }
    let _application = app::App::default().with_scheme(app::Scheme::Gtk);
    app::set_font_size(14);
    let (sender, receiver) = app::channel::<Ui>();
    let Some(_instance) = platform::instance(&options.directory, move || sender.send(Ui::Open))?
    else {
        return Ok(());
    };
    #[cfg(windows)]
    windows_identity::register_app()?;
    #[cfg(windows)]
    let _tray =
        tray::Tray::start(move || sender.send(Ui::Open), move || sender.send(Ui::Quit)).ok();
    let profile = Profile::load(&options.directory)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let (restart, _) = watch::channel(0);
    let _network_monitor = poknite_client::network::Monitor::start(restart.clone()).ok();
    let state = Rc::new(RefCell::new(State {
        options: options.clone(),
        store: store.clone(),
        profile,
        api: None,
        runtime,
        job: None,
        restart,
        status: ConnectionStatus::Offline,
        channels: store.channels()?,
        devices: Vec::new(),
        user: None,
        contacts: Vec::new(),
        editing: None,
        rule_role: 0,
        selected: 0,
        offset: 0,
        draft_generation: 0,
        follow_latest: true,
    }));
    #[cfg(target_os = "linux")]
    let _tray = linux_tray::Tray::start(
        state.borrow().runtime.handle().clone(),
        move || sender.send(Ui::Open),
        move || sender.send(Ui::TrayContext),
    )
    .ok()
    .flatten();
    let mut window = Window::new(160, 80, 980, 740, "Poknite");
    window.size_range(980, 740, 0, 0);
    let mut status = Frame::new(20, 8, 940, 28, "Подключение не настроено");
    status.set_align(Align::Left | Align::Inside);
    let mut tabs = Tabs::new(15, 45, 950, 640, "");
    let chat = Group::new(15, 75, 950, 610, "Общение");
    let sidebar = Group::new(25, 88, 220, 572, "");
    let mut sidebar_title = chat_ui::heading(35, 90, 200);
    sidebar_title.set_label("Разговоры");
    sidebar_title.set_label_size(17);
    let mut search_hint = chat_ui::hint(35, 123, 200);
    search_hint.set_label("Поиск по названию");
    let mut search = Input::new(35, 146, 200, 32, "");
    search.set_tooltip("Поиск канала или личного диалога");
    let mut channels = HoldBrowser::new(35, 189, 200, 410, "");
    channels.set_text_size(14);
    chat_ui::flat(&mut channels);
    let mut new_direct = Button::new(35, 615, 200, 35, "Новый диалог");
    sidebar.end();
    sidebar.resizable(&channels);
    let conversation = Group::new(260, 88, 690, 572, "");
    let title = chat_ui::heading(275, 91, 470);
    let hint = chat_ui::hint(275, 125, 660);
    let mut mute = CheckButton::new(790, 91, 150, 30, "Без звука");
    let mut previous = Button::new(275, 156, 155, 28, "Ранее");
    let mut latest = Button::new(440, 156, 180, 28, "К последним");
    let mut encryption = Button::new(630, 156, 180, 28, "Шифрование");
    encryption.set_callback({
        let state = state.clone();
        move |_| encryption_ui::open(&state, sender)
    });
    let mut history = TextDisplay::new(275, 195, 660, 327, "");
    let mut buffer = TextBuffer::default();
    history.set_buffer(buffer.clone());
    history.set_text_size(15);
    history.wrap_mode(fltk::text::WrapMode::AtBounds, 0);
    history.maintain_absolute_top_line_number(true);
    chat_ui::flat(&mut history);
    let mut draft = MultilineInput::new(275, 535, 660, 72, "");
    draft.set_text_size(15);
    draft.set_tooltip("Сообщение · Enter — новая строка, Ctrl+Enter — отправить");
    let mut mention = Button::new(275, 620, 45, 34, "@@");
    mention.set_tooltip("Упомянуть участника канала");
    let mut keyboard_hint = chat_ui::hint(330, 625, 420);
    keyboard_hint.set_label("Ctrl+Enter — отправить · Enter — новая строка");
    let mut send = Button::new(790, 620, 145, 34, "Отправить");
    conversation.end();
    conversation.resizable(&history);
    chat.end();
    chat.resizable(&conversation);
    let mut chat_controls = chat_ui::Controls {
        title,
        hint,
        send: send.clone(),
        mention: mention.clone(),
    };
    let setup = Group::new(15, 75, 950, 610, "Подключение");
    let mut server = Input::new(200, 120, 490, 35, "Адрес сервера:");
    server.set_value(
        state
            .borrow()
            .profile
            .as_ref()
            .map(|p| p.server.as_str())
            .unwrap_or("https://"),
    );
    let invitation = SecretInput::new(200, 170, 490, 35, "Приглашение:");
    let mut name = Input::new(200, 220, 490, 35, "Имя устройства:");
    name.set_value(&options.name);
    let mut enroll = Button::new(200, 280, 230, 40, "Подключить устройство");
    let mut reconnect = Button::new(445, 280, 245, 40, "Восстановить связь");
    let mut diagnose = Button::new(200, 325, 490, 35, "Проверить DNS, HTTPS и WebSocket");
    let mut info = Frame::new(
        50,
        385,
        630,
        150,
        "Нужно одноразовое приглашение от администратора.\nСообщения хранятся на устройстве после удаления на сервере.\nБез сети текст остаётся черновиком.\nКрестик скрывает окно; приём продолжается.\nСнова запустите Poknite, чтобы открыть окно.",
    );
    info.set_align(Align::Left | Align::Inside);
    setup.end();
    let settings = Group::new(15, 75, 950, 610, "Устройства и настройки");
    let mut devices = HoldBrowser::new(30, 95, 680, 255, "");
    devices.set_text_size(14);
    let mut reload = Button::new(30, 365, 220, 35, "Обновить устройства");
    let mut revoke = Button::new(265, 365, 220, 35, "Отключить выбранное");
    let mut clear = Button::new(30, 420, 240, 35, "Очистить историю");
    let mut auto = CheckButton::new(30, 475, 280, 35, "Запускать при входе");
    auto.set_value(state.borrow().profile.as_ref().is_some_and(|p| p.autostart));
    settings.end();
    let mut v2 = v2_ui::build(state.clone(), sender, draft.clone());
    tabs.end();
    let mut exit = Button::new(840, 700, 110, 30, "Выход");
    window.end();
    window.resizable(&tabs);
    {
        let mut s = state.borrow_mut();
        s.selected = s.store.restore_channel(&s.channels)?;
        chat_ui::fill_list(&s, &mut channels, "");
        refresh_history(&s, &mut buffer, &mut history);
        mute.set_value(s.store.muted(s.selected)?);
        draft.set_value(&s.store.draft(s.selected)?.0);
    }
    chat_ui::refresh_controls(&state.borrow(), &mut chat_controls, &draft.value());
    search.set_trigger(CallbackTrigger::Changed);
    search.set_callback({
        let state = state.clone();
        let mut channels = channels.clone();
        move |input| {
            chat_ui::fill_list(&state.borrow(), &mut channels, &input.value());
        }
    });
    new_direct.set_callback({
        let state = state.clone();
        let mut tabs = tabs.clone();
        let contact_group = v2.contacts.parent().unwrap();
        move |_| {
            let _ = tabs.set_value(&contact_group);
            let s = state.borrow();
            if let (Some(api), Some(profile)) = (s.api.clone(), s.profile.clone()) {
                s.runtime.spawn(async move {
                    sender.send(Ui::V2(v2_ui::Event::Contacts(
                        api.contacts(&profile).await.map_err(|e| e.to_string()),
                    )));
                });
            }
        }
    });
    mention.set_callback({
        let state = state.clone();
        move |_| v2_ui::mention_request(&state, sender)
    });
    history.handle({
        let state = state.clone();
        move |_, event| {
            if event == Event::MouseWheel && app::event_dy() == app::MouseWheel::Up {
                state.borrow_mut().follow_latest = false;
            }
            false
        }
    });
    // Consume Ctrl+Enter before FLTK's multiline handler can insert a newline.
    draft.super_handle_first(false);
    draft.handle({
        let mut send = send.clone();
        move |_, event| {
            if event == Event::KeyDown
                && app::event_key() == Key::Enter
                && app::event_state().contains(Shortcut::Ctrl)
            {
                if send.active() {
                    send.do_callback();
                }
                true
            } else {
                false
            }
        }
    });
    channels.set_callback({
        let state = state.clone();
        let mut draft = draft.clone();
        let mut mute = mute.clone();
        let mut buffer = buffer.clone();
        let mut history = history.clone();
        let search = search.clone();
        let mut controls = chat_controls.clone();
        move |choice| {
            let mut s = state.borrow_mut();
            let Some(channel) = (choice.value() > 0)
                .then(|| {
                    chat_ui::matching(&s.channels, &search.value())
                        .get((choice.value() - 1) as usize)
                        .map(|c| c.id)
                })
                .flatten()
            else {
                return;
            };
            if channel == s.selected {
                return;
            }
            if s.selected != 0
                && let Err(e) = s.store.save_draft(s.selected, &draft.value())
            {
                error(e)
            }
            s.selected = channel;
            s.store.select_channel(s.selected).unwrap_or_else(error);
            s.follow_latest = true;
            s.offset = 0;
            s.draft_generation += 1;
            refresh_history(&s, &mut buffer, &mut history);
            draft.set_value(&s.store.draft(s.selected).map(|x| x.0).unwrap_or_default());
            mute.set_value(s.store.muted(s.selected).unwrap_or(false));
            chat_ui::refresh_controls(&s, &mut controls, &draft.value());
        }
    });
    mute.set_callback({
        let state = state.clone();
        move |button| {
            let s = state.borrow();
            if let Err(e) = s.store.set_muted(s.selected, button.value()) {
                error(e)
            }
        }
    });
    draft.set_trigger(CallbackTrigger::Changed);
    draft.set_callback({
        let state = state.clone();
        let mut controls = chat_controls.clone();
        move |input| {
            let mut s = state.borrow_mut();
            s.draft_generation += 1;
            let generation = s.draft_generation;
            let state = state.clone();
            let value = input.value();
            chat_ui::refresh_controls(&s, &mut controls, &value);
            let caret = input.position().max(0) as usize;
            if value.get(..caret).is_some_and(|s| s.ends_with('@'))
                && value != s.store.draft(s.selected).map(|d| d.0).unwrap_or_default()
                && s.channels
                    .iter()
                    .find(|c| c.id == s.selected)
                    .is_some_and(|c| c.actions.contains(&Permission::Mention))
            {
                if let Err(e) = s.store.save_draft(s.selected, &value) {
                    error(e);
                    return;
                }
                drop(s);
                v2_ui::mention_request_at(&state, sender, caret);
                return;
            }
            let channel = s.selected;
            app::add_timeout3(0.5, move |_| {
                let s = state.borrow();
                if s.draft_generation == generation
                    && channel != 0
                    && let Err(e) = s.store.save_draft(channel, &value)
                {
                    error(e)
                }
            });
        }
    });
    previous.set_callback({
        let state = state.clone();
        let mut buffer = buffer.clone();
        let mut history = history.clone();
        move |_| {
            let mut s = state.borrow_mut();
            if !s
                .store
                .history(s.selected, s.offset + 50)
                .unwrap_or_default()
                .is_empty()
            {
                s.offset += 50;
                s.follow_latest = false;
            }
            refresh_history(&s, &mut buffer, &mut history)
        }
    });
    latest.set_callback({
        let state = state.clone();
        let mut buffer = buffer.clone();
        let mut history = history.clone();
        move |_| {
            let mut s = state.borrow_mut();
            s.offset = 0;
            s.follow_latest = true;
            refresh_history(&s, &mut buffer, &mut history)
        }
    });
    send.set_callback({
        let state = state.clone();
        let draft = draft.clone();
        move |_| {
            let s = state.borrow();
            let text = draft.value();
            let id = match s.store.save_draft(s.selected, &text) {
                Ok(id) => id,
                Err(e) => {
                    error(e);
                    return;
                }
            };
            if s.status != ConnectionStatus::Connected {
                error("Нет связи. Черновик сохранён; отправьте его после подключения.");
                return;
            }
            if !poknite_protocol::valid_text(&text) {
                error("Нужен непустой текст до 4096 байт.");
                return;
            }
            let (Some(api), Some(profile)) = (s.api.clone(), s.profile.clone()) else {
                return;
            };
            let channel = s.selected;
            let mentions = s.store.draft_mentions(channel).unwrap_or_default();
            let store = s.store.clone();
            s.runtime.spawn(async move {
                let result = api
                    .publish(
                        &profile,
                        &store,
                        channel,
                        &PublishRequest {
                            client_message_id: id.clone(),
                            text,
                            mentions,
                        },
                    )
                    .await
                    .map_err(|e| format!("{e}. Черновик сохранён."));
                sender.send(Ui::Published(channel, id, result));
            });
        }
    });
    enroll.set_callback({
        let state = state.clone();
        let server = server.clone();
        let mut invitation = invitation.clone();
        let name = name.clone();
        move |_| {
            let s = state.borrow();
            let address = server.value();
            let api = match Api::new(&address, s.options.dev_http, s.options.ca.as_deref()) {
                Ok(api) => api,
                Err(e) => {
                    error(e);
                    return;
                }
            };
            let code = invitation.value();
            invitation.set_value("");
            let name = name.value();
            s.runtime.spawn(async move {
                let result = api
                    .enroll(&code, &name)
                    .await
                    .map(|r| Profile {
                        server: address,
                        token: r.token,
                        device_id: r.device_id,
                        user_id: r.user_id,
                        user_name: r.user_name,
                        autostart: false,
                        management_server: String::new(),
                        management_ip: None,
                    })
                    .map_err(|e| e.to_string());
                sender.send(Ui::Enrolled(result));
            });
        }
    });
    diagnose.set_callback({
        let state = state.clone();
        let server = server.clone();
        move |_| {
            let s = state.borrow();
            match Api::new(&server.value(), s.options.dev_http, s.options.ca.as_deref()) {
                Ok(api) => {
                    s.runtime.spawn(async move {
                        sender.send(Ui::V2(v2_ui::Event::Diagnostics(api.diagnostics().await)));
                    });
                }
                Err(e) => error(e),
            }
        }
    });
    reconnect.set_callback({
        let state = state.clone();
        move |_| {
            let mut s = state.borrow_mut();
            if let Err(e) = s.start(sender) {
                error(e)
            }
        }
    });
    reload.set_callback({
        let state = state.clone();
        move |_| {
            let s = state.borrow();
            let (Some(api), Some(profile)) = (s.api.clone(), s.profile.clone()) else {
                error("Сначала подключите устройство");
                return;
            };
            s.runtime.spawn(async move {
                sender.send(Ui::Devices(
                    api.devices(&profile)
                        .await
                        .map_err(|_| "Не удалось загрузить устройства".into()),
                ));
            });
        }
    });
    revoke.set_callback({
        let state = state.clone();
        let devices = devices.clone();
        move |_| {
            let s = state.borrow();
            let (Some(api), Some(profile)) = (s.api.clone(), s.profile.clone()) else {
                return;
            };
            let Some(device) = s.devices.get((devices.value() - 1).max(0) as usize) else {
                return;
            };
            let id = device.id;
            if fltk::dialog::choice2_default("Отключить это устройство?", "Отмена", "Отключить", "")
                != Some(1)
            {
                return;
            }
            s.runtime.spawn(async move {
                sender.send(Ui::Revoked(
                    api.revoke(&profile, id)
                        .await
                        .map_err(|_| "Не удалось отключить устройство".into()),
                ));
            });
        }
    });
    clear.set_callback({
        let state = state.clone();
        let mut buffer = buffer.clone();
        move |_| {
            if fltk::dialog::choice2_default("Удалить локальную историю?", "Отмена", "Удалить", "")
                != Some(1)
            {
                return;
            }
            let s = state.borrow();
            match s.store.clear() {
                Ok(()) => buffer.set_text(""),
                Err(e) => error(e),
            }
        }
    });
    auto.set_callback({
        let state = state.clone();
        move |button| {
            let mut s = state.borrow_mut();
            let enabled = button.value();
            if let Err(e) = platform::autostart(
                enabled,
                &s.options.directory,
                s.options.ca.as_deref(),
                s.options.dev_http,
            ) {
                error(e);
                button.set_value(!enabled);
                return;
            }
            let directory = s.options.directory.clone();
            if let Some(profile) = s.profile.as_mut() {
                profile.autostart = enabled;
                if let Err(e) = profile.save(&directory) {
                    error(e)
                }
            }
        }
    });
    window.set_callback({
        let state = state.clone();
        let draft = draft.clone();
        move |w| {
            let s = state.borrow();
            let _ = s.store.save_draft(s.selected, &draft.value());
            w.hide();
        }
    });
    exit.set_callback({
        let state = state.clone();
        let draft = draft.clone();
        move |_| {
            let s = state.borrow();
            let _ = s.store.save_draft(s.selected, &draft.value());
            app::quit();
        }
    });
    if state.borrow().profile.is_none() {
        let _ = tabs.set_value(&setup);
    }
    if !options.background {
        window.show();
    }
    state.borrow_mut().start(sender)?;
    // wait() remains active after the main window is hidden. This explicit loop
    // lets a second invocation reopen the running instance without polling.
    while !app::should_program_quit() {
        app::wait_for(1e20)?;
        while let Some(message) = receiver.recv() {
            match message {
                Ui::E2eeMembers(channel, result) => {
                    encryption_ui::show(&state, channel, result);
                    refresh_history(&state.borrow(), &mut buffer, &mut history);
                }
                Ui::V2(event) => {
                    let is_direct = matches!(&event, v2_ui::Event::Direct(Ok(_)));
                    if is_direct {
                        let s = state.borrow();
                        if s.selected > 0 {
                            s.store.save_draft(s.selected, &draft.value())?;
                        }
                    }
                    v2_ui::handle(event, &state, sender, &mut v2, &mut draft);
                    if is_direct {
                        let s = state.borrow();
                        s.store.set_channels(&s.channels)?;
                        draft.set_value(&s.store.draft(s.selected)?.0);
                        mute.set_value(s.store.muted(s.selected)?);
                        chat_ui::fill_list(&s, &mut channels, &search.value());
                        refresh_history(&s, &mut buffer, &mut history);
                        let _ = tabs.set_value(&chat);
                        let _ = draft.take_focus();
                    }
                }
                Ui::Open => {
                    window.show();
                    let _ = window.take_focus();
                }
                #[cfg(target_os = "linux")]
                Ui::TrayContext => {
                    match fltk::dialog::choice2_default("Poknite", "Открыть", "Выход", "Отмена")
                    {
                        Some(0) => sender.send(Ui::Open),
                        Some(1) => sender.send(Ui::Quit),
                        _ => {}
                    }
                }
                Ui::Quit => {
                    let s = state.borrow();
                    let _ = s.store.save_draft(s.selected, &draft.value());
                    app::quit();
                }
                Ui::Enrolled(result) => match result {
                    Ok(profile) => {
                        let mut s = state.borrow_mut();
                        if let Some(job) = s.job.take() {
                            job.abort()
                        }
                        s.store.reset_account()?;
                        profile.save(&s.options.directory)?;
                        s.profile = Some(profile);
                        s.channels.clear();
                        s.selected = 0;
                        s.offset = 0;
                        s.follow_latest = true;
                        draft.set_value("");
                        channels.clear();
                        s.start(sender)?;
                        let _ = tabs.set_value(&chat);
                    }
                    Err(e) => error(e),
                },
                Ui::Published(channel, id, result) => match result {
                    Ok(message) => {
                        let mut s = state.borrow_mut();
                        s.store.receive(
                            &message,
                            false,
                            s.profile.as_ref().map(|p| p.user_id).unwrap_or(0),
                        )?;
                        if s.selected == channel && draft.value() != message.text {
                            s.store.save_draft(channel, &draft.value())?;
                        }
                        s.store.clear_draft_if(channel, &id)?;
                        if s.selected == channel
                            && draft.value() == message.text
                            && s.store.draft(channel)?.0.is_empty()
                        {
                            s.draft_generation += 1;
                            draft.set_value("")
                        }
                        refresh_history(&s, &mut buffer, &mut history)
                    }
                    Err(e) => error(e),
                },
                Ui::Devices(result) => match result {
                    Ok(list) => {
                        devices.clear();
                        for d in &list {
                            devices.add(&format!(
                                "{}{}",
                                d.name,
                                if d.current {
                                    " (это устройство)"
                                } else {
                                    ""
                                }
                            ));
                        }
                        state.borrow_mut().devices = list;
                    }
                    Err(e) => error(e),
                },
                Ui::Revoked(result) => match result {
                    Ok(()) => {
                        devices.clear();
                        state.borrow_mut().devices.clear();
                        status.set_label("Устройство отключено. Обновите список.");
                    }
                    Err(e) => error(e),
                },
                Ui::NotificationResult(result) => {
                    if let Err(e) = result {
                        status.set_label(&e)
                    }
                }
                Ui::Core(update) => match update {
                    Update::Diagnostic(reason) => status.set_label(&reason),
                    Update::Profile(user) => {
                        let mut s = state.borrow_mut();
                        let directory = s.options.directory.clone();
                        v2.nick.set_value(&user.name);
                        v2.color.set_value(&v2_ui::rgb_string(&user.color));
                        if user.actions.contains(&Permission::Profile) {
                            v2.save_nick.activate();
                        } else {
                            v2.save_nick.deactivate();
                        }
                        if user.actions.iter().any(|p| {
                            matches!(
                                p,
                                Permission::Users
                                    | Permission::Channels
                                    | Permission::Devices
                                    | Permission::Invitations
                                    | Permission::Roles
                                    | Permission::ManageChannel
                            )
                        }) {
                            v2.management.activate();
                        } else {
                            v2.management.deactivate();
                            v2.admin.clear();
                            v2.admin_data.borrow_mut().clear();
                        }
                        if let Some(p) = s.profile.as_mut() {
                            p.user_name = user.name.clone();
                            p.save(&directory)?;
                        }
                        s.user = Some(user);
                        refresh_history(&s, &mut buffer, &mut history);
                    }
                    Update::Contacts(users) => {
                        v2.contacts.clear();
                        for u in &users {
                            v2.contacts.add(&v2_ui::colored_name(u));
                        }
                        state.borrow_mut().contacts = users;
                    }
                    Update::Status(value) => {
                        if value == ConnectionStatus::Revoked {
                            let s = state.borrow();
                            s.store.set_channels(&[])?;
                            for (key, id) in s.store.drain_cancelled()? {
                                let _ = platform::cancel_notification(&key, id);
                            }
                            v2.management.deactivate();
                            send.deactivate();
                        }
                        state.borrow_mut().status = value.clone();
                        status.set_label(match value {
                            ConnectionStatus::Connecting => "Подключение…",
                            ConnectionStatus::Connected => "На связи",
                            ConnectionStatus::Offline => {
                                "Нет связи. Переподключение выполняется автоматически."
                            }
                            ConnectionStatus::Revoked => {
                                "Устройство отключено. Нужно новое приглашение."
                            }
                        });
                    }
                    Update::Channels(list) => {
                        let mut s = state.borrow_mut();
                        for (key, id) in s.store.drain_cancelled()? {
                            let _ = platform::cancel_notification(&key, id);
                        }
                        if s.selected != 0 {
                            s.store.save_draft(s.selected, &draft.value())?;
                        }
                        let selected = s.selected;
                        s.channels = list;
                        channels.clear();
                        if !s.channels.iter().any(|c| c.id == selected) {
                            s.selected = s.store.restore_channel(&s.channels)?;
                            s.follow_latest = true;
                            s.offset = 0;
                        } else {
                            s.selected = selected;
                            s.store.select_channel(selected)?;
                        }
                        chat_ui::fill_list(&s, &mut channels, &search.value());
                        draft.set_value(&s.store.draft(s.selected)?.0);
                        mute.set_value(s.store.muted(s.selected)?);
                        if s.channels
                            .iter()
                            .find(|c| c.id == s.selected)
                            .is_some_and(|c| c.actions.contains(&Permission::Send))
                        {
                            send.activate();
                        } else {
                            send.deactivate();
                        }
                        refresh_history(&s, &mut buffer, &mut history);
                    }
                    Update::Changed => {
                        let s = state.borrow();
                        if s.offset == 0 {
                            refresh_history(&s, &mut buffer, &mut history)
                        }
                    }
                    Update::Gap => {
                        status.set_label("На связи. Часть сообщений уже удалена по сроку хранения.")
                    }
                    Update::Notifications => {
                        let s = state.borrow();
                        let store = s.store.clone();
                        s.runtime.spawn_blocking(move||{let result=store.process_notifications(platform::notify).map_err(|_|"Системные уведомления недоступны. Сообщения сохранены в истории.".into());sender.send(Ui::NotificationResult(result));});
                    }
                },
            }
            chat_ui::refresh_controls(&state.borrow(), &mut chat_controls, &draft.value());
        }
    }
    Ok(())
}
fn headless(options: Options, store: Arc<Store>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let action=options.headless.as_deref().unwrap();
        if action=="diagnose"{let api=Api::new(options.server.as_deref().context("Нужен --server")?,options.dev_http,options.ca.as_deref())?;println!("{}",api.diagnostics().await.join("\n"));return Ok(())}
        if action=="enroll"{let server=options.server.as_deref().context("Нужен --server")?;let api=Api::new(server,options.dev_http,options.ca.as_deref())?;let r=api.enroll(options.invitation.as_deref().context("Нужен --invitation-file")?,&options.name).await?;store.reset_account()?;let profile=Profile{server:server.into(),token:r.token,device_id:r.device_id,user_id:r.user_id,user_name:r.user_name,autostart:false,
management_server: String::new(), management_ip: None,
};profile.save(&options.directory)?;println!("{}",serde_json::json!({"device_id":profile.device_id,"user_id":profile.user_id}));return Ok(())}
        if action=="clear"{store.clear()?;return Ok(())}
        if action=="history"{let messages=store.history(options.channel,0)?;println!("{}",serde_json::to_string(&messages)?);return Ok(())}
        let profile=Profile::load(&options.directory)?.context("Устройство не подключено")?;
        if action=="status"{println!("{}",serde_json::json!({"device_id":profile.device_id,"cursor":store.cursor()?,"pending":store.pending()?.len()}));return Ok(())}
        let api=Api::new(&profile.server,options.dev_http,options.ca.as_deref())?;
        match action{
            "key-create"|"key-export"=>{
                let path=options.key_file.as_ref().context("Нужен --key-file для сохранения секретного кода")?;
                use std::io::Write;
                let mut output=std::fs::OpenOptions::new();output.write(true).create_new(true);
                #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;output.mode(0o600);}
                let mut file=output.open(path)?;
                let code=if action=="key-create" {let members=api.e2ee_members(&profile,options.channel).await?;store.create_conversation_key(api.audience(),options.channel,&members)?} else {store.export_conversation_key(api.audience(),options.channel)?};
                file.write_all(code.as_bytes())?;file.sync_all()?;
                println!("{}",serde_json::json!({"key_saved":true}));
            },
            "key-import"=>{
                let path=options.key_file.as_ref().context("Нужен --key-file с кодом ключа")?;
                anyhow::ensure!(std::fs::metadata(path)?.len()<=2048,"Код ключа слишком длинный");
                let members=api.e2ee_members(&profile,options.channel).await?;
                store.import_conversation_key(api.audience(),options.channel,&members,&std::fs::read_to_string(path)?)?;
                println!("{}",serde_json::json!({"key_imported":true}));
            },
            "send"=>{let text=std::fs::read_to_string(options.text_file.context("Нужен --text-file")?)?;let id=store.save_draft(options.channel,&text)?;let message=api.publish(&profile,&store,options.channel,&PublishRequest{client_message_id:id.clone(),text,mentions:store.draft_mentions(options.channel)?,
}).await?;store.receive(&message,false,profile.user_id)?;store.clear_draft_if(options.channel,&id)?;println!("{}",serde_json::json!({"id":message.id,"seq":message.seq}));},
            "management-check"=>{let admin=api.management(&profile.management_server,profile.management_ip)?;let roles:serde_json::Value=admin.request(&profile,"v2/admin/roles","GET",None).await?;println!("{}",serde_json::json!({"available":true,"roles":roles.as_array().map(Vec::len)}));},
            "devices"=>println!("{}",serde_json::to_string(&api.devices(&profile).await?)?),
            "revoke"=>api.revoke(&profile,options.device).await?,
            "run"=>{let(tx,mut rx)=mpsc::channel(64);let(_restart,receiver)=watch::channel(0);let cloned=store.clone();let job=tokio::spawn(async move{api.run(profile,cloned,tx,receiver).await;});let end=tokio::time::sleep(Duration::from_secs(options.seconds));tokio::pin!(end);loop{tokio::select!{_= &mut end=>break,update=rx.recv()=>{let Some(update)=update else{break};match update{Update::Status(status)=>println!("{}",serde_json::json!({"status":format!("{status:?}").to_lowercase(),"cursor":store.cursor()?})),Update::Notifications=>{let messages=store.pending()?;println!("{}",serde_json::json!({"notifications":messages.len(),"ids":messages.iter().map(|(m,_)|&m.id).collect::<Vec<_>>() }));if !options.no_notify && platform::notify(&messages,0,messages.iter().all(|(m,_)|store.muted(m.channel_id).unwrap_or(false))).is_ok(){store.notified(&messages.iter().map(|(m,_)|m.id.clone()).collect::<Vec<_>>())?;}},Update::Gap=>println!("{}",serde_json::json!({"gap":true})),_=>{}}}}}job.abort();},
            _=>bail!("Неизвестная команда headless: {action}"),
        }Ok(())
    })
}
