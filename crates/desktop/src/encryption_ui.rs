use crate::{State, Ui, error};
use fltk::{
    app, button::Button, enums::Align, frame::Frame, input::MultilineInput, prelude::*,
    window::Window,
};
use poknite_protocol::e2ee::Member;
use std::{cell::RefCell, rc::Rc};

pub fn open(state: &Rc<RefCell<State>>, sender: app::Sender<Ui>) {
    let s = state.borrow();
    let (Some(api), Some(profile)) = (s.api.clone(), s.profile.clone()) else {
        error("Сначала подключите устройство");
        return;
    };
    let channel = s.selected;
    if channel <= 0 {
        error("Выберите разговор");
        return;
    }
    s.runtime.spawn(async move {
        sender.send(Ui::E2eeMembers(
            channel,
            api.e2ee_members(&profile, channel)
                .await
                .map_err(|_| "Не удалось получить состав устройств разговора".into()),
        ));
    });
}
pub fn show(state: &Rc<RefCell<State>>, channel: i64, result: Result<Vec<Member>, String>) {
    let members = match result {
        Ok(m) => m,
        Err(e) => {
            error(e);
            return;
        }
    };
    let s = state.borrow();
    let Some(api) = s.api.clone() else {
        return;
    };
    let store = s.store.clone();
    let server = api.audience().to_owned();
    let name = s
        .channels
        .iter()
        .find(|c| c.id == channel)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    drop(s);
    let mut window = Window::new(0, 0, 700, 445, "Шифрование разговора");
    let mut description = Frame::new(20, 15, 660, 100, "");
    description.set_align(Align::Left | Align::Inside | Align::Wrap);
    description.set_label(&format!("{name}\nКлюч создаётся на устройстве. Передайте код только доверенным участникам через личную встречу или другой защищённый канал. Сервер код не получает.\nПри изменении состава устройств создайте новый ключ и перенесите его заново."));
    let mut fingerprint = Frame::new(20, 120, 660, 45, "");
    fingerprint.set_align(Align::Left | Align::Inside | Align::Wrap);
    fingerprint.set_label(
        &store
            .key_fingerprint(&server, channel)
            .map(|id| format!("Отпечаток ключа: {id}"))
            .unwrap_or_else(|_| "Ключ ещё не подключён".into()),
    );
    let code = MultilineInput::new(20, 175, 660, 165, "");
    let mut create = Button::new(20, 355, 205, 35, "Создать / сменить ключ");
    let mut export = Button::new(245, 355, 205, 35, "Показать код ключа");
    let mut import = Button::new(475, 355, 205, 35, "Подключить код");
    let mut close = Button::new(475, 405, 205, 30, "Закрыть");
    create.set_callback({
        let store=store.clone();let server=server.clone();let members=members.clone();let mut code=code.clone();let mut fingerprint=fingerprint.clone();
        move |_| {
            if fltk::dialog::choice2_default("Новый ключ нужно перенести на все доверенные устройства. Уже сохранённая история останется доступна.","Отмена","Создать","") != Some(1) {return;}
            match store.create_conversation_key(&server,channel,&members) {
                Ok(token) => {code.set_value(&token);fingerprint.set_label(&format!("Отпечаток ключа: {}",store.key_fingerprint(&server,channel).unwrap_or_default()));},
                Err(_) => error("Не удалось сохранить ключ разговора"),
            }
        }
    });
    export.set_callback({
        let store = store.clone();
        let server = server.clone();
        let mut code = code.clone();
        move |_| match store.export_conversation_key(&server, channel) {
            Ok(token) => code.set_value(&token),
            Err(_) => error("Нет ключа разговора"),
        }
    });
    import.set_callback({
        let store = store.clone();
        let server = server.clone();
        let members = members.clone();
        let code = code.clone();
        let mut fingerprint = fingerprint.clone();
        move |_| match store.import_conversation_key(&server, channel, &members, &code.value()) {
            Ok(()) => fingerprint.set_label(&format!(
                "Ключ подключён: {}",
                store.key_fingerprint(&server, channel).unwrap_or_default()
            )),
            Err(e) => error(e),
        }
    });
    close.set_callback({
        let mut window = window.clone();
        move |_| window.hide()
    });
    window.end();
    window.make_modal(true);
    window.show();
    while window.shown() {
        app::wait();
    }
}
