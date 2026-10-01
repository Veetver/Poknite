#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(target_os = "linux")]
mod dbus;
#[cfg(target_os = "linux")]
mod linux_tray;
mod platform;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod windows_identity;
use anyhow::{Context, Result, bail};
use fltk::{
    app,
    browser::HoldBrowser,
    button::{Button, CheckButton},
    enums::{Align, CallbackTrigger},
    frame::Frame,
    group::{Group, Tabs},
    input::{Input, MultilineInput, SecretInput},
    menu::Choice,
    prelude::*,
    text::{TextBuffer, TextDisplay},
    window::Window,
};
use poknite_client::{Api, ConnectionStatus, Profile, Store, Update};
use poknite_protocol::{Channel, Device, PublishRequest};
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
                "--channel" => o.channel = next(&mut args)?.parse()?,
                "--device" => o.device = next(&mut args)?.parse()?,
                "--seconds" => o.seconds = next(&mut args)?.parse()?,
                "--no-notify" => o.no_notify = true,
                "--help" => {
                    println!(
                        "Poknite\n  --background\n  --data-dir PATH\n  --ca PEM\n  --dev-http (только loopback)\n  --headless enroll --server URL --invitation-file PATH [--name NAME]\n  --headless run [--seconds 30] [--no-notify]\n  --headless send --channel ID --text-file PATH\n  --headless history|status|devices|clear\n  --headless revoke --device ID"
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
    Core(Update),
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
    selected: i64,
    offset: usize,
    draft_generation: u64,
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
fn refresh_history(state: &State, buffer: &mut TextBuffer) {
    match state.store.history(state.selected, state.offset) {
        Ok(messages) => {
            let text = messages
                .into_iter()
                .rev()
                .map(|m| format!("{}\n{}\n\n", m.sender_name, m.text))
                .collect::<String>();
            buffer.set_text(&text)
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
        selected: 0,
        offset: 0,
        draft_generation: 0,
    }));
    #[cfg(target_os = "linux")]
    let _tray = linux_tray::Tray::start(
        state.borrow().runtime.handle().clone(),
        move || sender.send(Ui::Open),
        move || sender.send(Ui::TrayContext),
    )
    .ok()
    .flatten();
    let mut window = Window::new(160, 120, 740, 630, "Poknite");
    window.size_range(740, 630, 0, 0);
    let mut status = Frame::new(20, 8, 700, 28, "Подключение не настроено");
    status.set_align(Align::Left | Align::Inside);
    let mut tabs = Tabs::new(15, 45, 710, 525, "");
    let chat = Group::new(15, 75, 710, 495, "Сообщения");
    let mut channels = Choice::new(95, 90, 430, 30, "Канал:");
    let mut mute = CheckButton::new(540, 90, 170, 30, "Без звука");
    let mut history = TextDisplay::new(30, 135, 680, 260, "");
    let mut buffer = TextBuffer::default();
    history.set_buffer(buffer.clone());
    history.set_text_size(14);
    history.wrap_mode(fltk::text::WrapMode::AtBounds, 0);
    let mut previous = Button::new(30, 405, 190, 30, "Предыдущие 50");
    let mut latest = Button::new(230, 405, 170, 30, "Новые сообщения");
    let mut draft = MultilineInput::new(30, 445, 540, 90, "");
    draft.set_text_size(14);
    let mut send = Button::new(580, 470, 130, 40, "Отправить");
    chat.end();
    let setup = Group::new(15, 75, 710, 495, "Подключение");
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
    let mut info = Frame::new(
        50,
        340,
        630,
        170,
        "Нужно одноразовое приглашение от администратора.\nСообщения хранятся на устройстве после удаления на сервере.\nБез сети текст остаётся черновиком.\nКрестик скрывает окно; приём продолжается.\nСнова запустите Poknite, чтобы открыть окно.",
    );
    info.set_align(Align::Left | Align::Inside);
    setup.end();
    let settings = Group::new(15, 75, 710, 495, "Устройства и настройки");
    let mut devices = HoldBrowser::new(30, 95, 680, 255, "");
    devices.set_text_size(14);
    let mut reload = Button::new(30, 365, 220, 35, "Обновить устройства");
    let mut revoke = Button::new(265, 365, 220, 35, "Отключить выбранное");
    let mut clear = Button::new(30, 420, 240, 35, "Очистить историю");
    let mut auto = CheckButton::new(30, 475, 280, 35, "Запускать при входе");
    auto.set_value(state.borrow().profile.as_ref().is_some_and(|p| p.autostart));
    settings.end();
    tabs.end();
    let mut exit = Button::new(600, 585, 110, 30, "Выход");
    window.end();
    window.resizable(&tabs);
    {
        let mut s = state.borrow_mut();
        if let Some(first) = s.channels.first() {
            s.selected = first.id;
        }
        for channel in &s.channels {
            channels.add_choice(&channel.name.replace(['|', '/', '\\'], " "));
        }
        channels.set_value(0);
        refresh_history(&s, &mut buffer);
        mute.set_value(s.store.muted(s.selected)?);
        draft.set_value(&s.store.draft(s.selected)?.0);
    }
    channels.set_callback({
        let state = state.clone();
        let mut draft = draft.clone();
        let mut mute = mute.clone();
        let mut buffer = buffer.clone();
        move |choice| {
            let mut s = state.borrow_mut();
            if s.selected != 0
                && let Err(e) = s.store.save_draft(s.selected, &draft.value())
            {
                error(e)
            }
            if let Some(channel) = s.channels.get(choice.value().max(0) as usize) {
                s.selected = channel.id;
            }
            s.offset = 0;
            s.draft_generation += 1;
            refresh_history(&s, &mut buffer);
            draft.set_value(&s.store.draft(s.selected).map(|x| x.0).unwrap_or_default());
            mute.set_value(s.store.muted(s.selected).unwrap_or(false));
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
        move |input| {
            let mut s = state.borrow_mut();
            s.draft_generation += 1;
            let generation = s.draft_generation;
            let state = state.clone();
            let value = input.value();
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
        move |_| {
            let mut s = state.borrow_mut();
            if !s
                .store
                .history(s.selected, s.offset + 50)
                .unwrap_or_default()
                .is_empty()
            {
                s.offset += 50;
            }
            refresh_history(&s, &mut buffer)
        }
    });
    latest.set_callback({
        let state = state.clone();
        let mut buffer = buffer.clone();
        move |_| {
            let mut s = state.borrow_mut();
            s.offset = 0;
            refresh_history(&s, &mut buffer)
        }
    });
    send.set_callback({let state=state.clone();let draft=draft.clone();move|_|{
        let s=state.borrow();let text=draft.value();let id=match s.store.save_draft(s.selected,&text){Ok(id)=>id,Err(e)=>{error(e);return}};
        if s.status!=ConnectionStatus::Connected{error("Нет связи. Черновик сохранён; отправьте его после подключения.");return}
        if !poknite_protocol::valid_text(&text){error("Нужен непустой текст до 4096 байт.");return}
        let(Some(api),Some(profile))=(s.api.clone(),s.profile.clone())else{return};let channel=s.selected;
        s.runtime.spawn(async move{let result=api.publish(&profile,channel,&PublishRequest{client_message_id:id.clone(),text}).await.map_err(|_|"Не удалось отправить. Черновик сохранён. Повторите отправку после восстановления связи.".into());sender.send(Ui::Published(channel,id,result));});
    }});
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
                    })
                    .map_err(|e| e.to_string());
                sender.send(Ui::Enrolled(result));
            });
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
                        refresh_history(&s, &mut buffer)
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
                Ui::Core(update) => {
                    match update {
                        Update::Status(value) => {
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
                            if s.selected != 0 {
                                s.store.save_draft(s.selected, &draft.value())?;
                            }
                            let selected = s.selected;
                            s.channels = list;
                            channels.clear();
                            for c in &s.channels {
                                channels.add_choice(&c.name.replace(['|', '/', '\\'], " "));
                            }
                            let position = s
                                .channels
                                .iter()
                                .position(|c| c.id == selected)
                                .unwrap_or(0);
                            channels.set_value(position as i32);
                            if let Some(c) = s.channels.get(position) {
                                s.selected = c.id;
                            } else {
                                s.selected = 0
                            }
                            draft.set_value(&s.store.draft(s.selected)?.0);
                            mute.set_value(s.store.muted(s.selected)?);
                            refresh_history(&s, &mut buffer);
                        }
                        Update::Changed => {
                            let s = state.borrow();
                            if s.offset == 0 {
                                refresh_history(&s, &mut buffer)
                            }
                        }
                        Update::Gap => status
                            .set_label("На связи. Часть сообщений уже удалена по сроку хранения."),
                        Update::Notifications => {
                            let s = state.borrow();
                            let store = s.store.clone();
                            s.runtime.spawn_blocking(move||{let result=(||->Result<()> {let messages=store.take_pending()?;let ids=messages.iter().map(|(m,_)|m.id.clone()).collect::<Vec<_>>();let summary=messages.len()>1 || messages.iter().any(|(_,r)|*r);let key=messages.first().map(|(m,_)|m.id.as_str()).unwrap_or("");let replace=store.popup_id(summary,key)?;let quiet=messages.iter().all(|(m,_)|store.muted(m.channel_id).unwrap_or(false));match platform::notify(&messages,replace,quiet){Ok(id)=>{store.set_popup_id(summary,key,id)?;},Err(e)=>{store.notification_failed(&ids)?;return Err(e)}}store.notified(&ids)})().map_err(|_|"Системные уведомления недоступны. Сообщения сохранены в истории.".into());sender.send(Ui::NotificationResult(result));});
                        }
                    }
                }
            }
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
        if action=="enroll"{let server=options.server.as_deref().context("Нужен --server")?;let api=Api::new(server,options.dev_http,options.ca.as_deref())?;let r=api.enroll(options.invitation.as_deref().context("Нужен --invitation-file")?,&options.name).await?;store.reset_account()?;let profile=Profile{server:server.into(),token:r.token,device_id:r.device_id,user_id:r.user_id,user_name:r.user_name,autostart:false};profile.save(&options.directory)?;println!("{}",serde_json::json!({"device_id":profile.device_id,"user_id":profile.user_id}));return Ok(())}
        if action=="clear"{store.clear()?;return Ok(())}
        if action=="history"{let messages=store.history(options.channel,0)?;println!("{}",serde_json::to_string(&messages)?);return Ok(())}
        let profile=Profile::load(&options.directory)?.context("Устройство не подключено")?;
        if action=="status"{println!("{}",serde_json::json!({"device_id":profile.device_id,"cursor":store.cursor()?,"pending":store.pending()?.len()}));return Ok(())}
        let api=Api::new(&profile.server,options.dev_http,options.ca.as_deref())?;
        match action{
            "send"=>{let text=std::fs::read_to_string(options.text_file.context("Нужен --text-file")?)?;let id=store.save_draft(options.channel,&text)?;let message=api.publish(&profile,options.channel,&PublishRequest{client_message_id:id.clone(),text}).await?;store.receive(&message,false,profile.user_id)?;store.clear_draft_if(options.channel,&id)?;println!("{}",serde_json::json!({"id":message.id,"seq":message.seq}));},
            "devices"=>println!("{}",serde_json::to_string(&api.devices(&profile).await?)?),
            "revoke"=>api.revoke(&profile,options.device).await?,
            "run"=>{let(tx,mut rx)=mpsc::channel(64);let(_restart,receiver)=watch::channel(0);let cloned=store.clone();let job=tokio::spawn(async move{api.run(profile,cloned,tx,receiver).await;});let end=tokio::time::sleep(Duration::from_secs(options.seconds));tokio::pin!(end);loop{tokio::select!{_= &mut end=>break,update=rx.recv()=>{let Some(update)=update else{break};match update{Update::Status(status)=>println!("{}",serde_json::json!({"status":format!("{status:?}").to_lowercase(),"cursor":store.cursor()?})),Update::Notifications=>{let messages=store.pending()?;println!("{}",serde_json::json!({"notifications":messages.len(),"ids":messages.iter().map(|(m,_)|&m.id).collect::<Vec<_>>() }));if !options.no_notify && platform::notify(&messages,0,messages.iter().all(|(m,_)|store.muted(m.channel_id).unwrap_or(false))).is_ok(){store.notified(&messages.iter().map(|(m,_)|m.id.clone()).collect::<Vec<_>>())?;}},Update::Gap=>println!("{}",serde_json::json!({"gap":true})),_=>{}}}}}job.abort();},
            _=>bail!("Неизвестная команда headless: {action}"),
        }Ok(())
    })
}
