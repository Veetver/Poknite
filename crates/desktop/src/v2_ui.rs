use crate::{State, Ui, error};
use fltk::{
    app,
    browser::HoldBrowser,
    button::{Button, CheckButton},
    enums::Align,
    frame::Frame,
    group::{Group, Scroll},
    input::Input,
    menu::Choice,
    prelude::*,
    window::Window,
};
use poknite_client::Api;
use poknite_protocol::*;
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub enum Event {
    Contacts(Result<Vec<User>, String>),
    Direct(Result<Channel, String>),
    Mention(i64, usize, Result<Vec<User>, String>),
    Admin(String, Result<Value, String>),
    Done(Result<Value, String>),
    Renamed(Result<User, String>),
    Diagnostics(Vec<String>),
}
#[derive(Clone)]
pub struct View {
    pub contacts: HoldBrowser,
    pub admin: HoldBrowser,
    pub nick: Input,
    pub color: Input,
    pub save_nick: Button,
    pub management: Group,
    pub admin_data: Rc<RefCell<Vec<Value>>>,
}
fn api(state: &State) -> Result<(Api, poknite_client::Profile), String> {
    Ok((
        state.api.clone().ok_or("Сначала подключите устройство")?,
        state
            .profile
            .clone()
            .ok_or("Сначала подключите устройство")?,
    ))
}
fn request(
    state: &Rc<RefCell<State>>,
    sender: app::Sender<Ui>,
    path: String,
    method: &str,
    body: Option<Value>,
    entity: Option<String>,
) {
    let s = state.borrow();
    let (api, profile) = match api(&s) {
        Ok(x) => x,
        Err(e) => {
            error(e);
            return;
        }
    };
    let admin = match api.management(&profile.management_server, profile.management_ip) {
        Ok(x) => x,
        Err(e) => {
            error(e);
            return;
        }
    };
    let method = method.to_string();
    s.runtime.spawn(async move {
        let result = admin
            .request::<Value>(&profile, &path, &method, body)
            .await
            .map_err(|e| e.to_string());
        sender.send(Ui::V2(if let Some(entity) = entity {
            Event::Admin(entity, result)
        } else {
            Event::Done(result)
        }));
    });
}
pub fn build(
    state: Rc<RefCell<State>>,
    sender: app::Sender<Ui>,
    mut draft: fltk::input::MultilineInput,
) -> View {
    let contact_group = Group::new(15, 75, 950, 610, "Контакты");
    let contacts = HoldBrowser::new(30, 100, 680, 340, "");
    let mut reload = Button::new(30, 455, 200, 35, "Обновить контакты");
    let mut direct = Button::new(245, 455, 200, 35, "Личный диалог");
    contact_group.end();
    reload.set_callback({
        let state = state.clone();
        move |_| {
            let s = state.borrow();
            if let Ok((a, p)) = api(&s) {
                s.runtime.spawn(async move {
                    sender.send(Ui::V2(Event::Contacts(
                        a.contacts(&p).await.map_err(|e| e.to_string()),
                    )));
                });
            }
        }
    });
    direct.set_callback({
        let state = state.clone();
        let contacts = contacts.clone();
        move |_| {
            let s = state.borrow();
            let Some(u) = s.contacts.get((contacts.value() - 1).max(0) as usize) else {
                return;
            };
            let id = u.id;
            if let Ok((a, p)) = api(&s) {
                s.runtime.spawn(async move {
                    sender.send(Ui::V2(Event::Direct(
                        a.direct(&p, id).await.map_err(|e| e.to_string()),
                    )));
                });
            }
        }
    });
    let mut management = Group::new(15, 75, 950, 610, "Управление");
    let mut address = Input::new(205, 95, 490, 30, "HTTPS управления:");
    let mut ip = Input::new(205, 135, 490, 30, "IP подключения:");
    if let Some(p) = state.borrow().profile.as_ref() {
        address.set_value(&p.management_server);
        ip.set_value(&p.management_ip.map(|ip| ip.to_string()).unwrap_or_default());
    }
    let mut save = Button::new(30, 178, 180, 32, "Сохранить адрес");
    let mut entity = Choice::new(300, 178, 200, 32, "");
    entity.add_choice("Пользователи|Каналы|Роли");
    entity.set_value(0);
    let mut refresh = Button::new(520, 178, 175, 32, "Загрузить");
    let admin = HoldBrowser::new(30, 225, 665, 160, "");
    let data = Rc::new(RefCell::new(Vec::<Value>::new()));
    let mut add = Button::new(30, 400, 155, 32, "Создать");
    let mut edit = Button::new(195, 400, 155, 32, "Изменить");
    let mut disable = Button::new(360, 400, 155, 32, "Отключить / закрыть");
    let mut rules = Button::new(525, 400, 170, 32, "Правила канала");
    let invite = Button::new(30, 445, 155, 32, "Приглашение");
    let devices = Button::new(195, 445, 155, 32, "Устройства");
    let revoke_invite = Button::new(360, 445, 200, 32, "Отозвать приглашения");
    let mut hint = Frame::new(
        30,
        490,
        665,
        60,
        "Управление требует доверенной сети и разрешения роли.\nIP подключения необязателен; имя HTTPS проверяется сертификатом.",
    );
    hint.set_align(Align::Left | Align::Inside);
    management.end();
    management.deactivate();
    save.set_callback({
        let state = state.clone();
        move |_| {
            let mut s = state.borrow_mut();
            let directory = s.options.directory.clone();
            let Some(p) = s.profile.as_mut() else {
                return;
            };
            let raw = ip.value();
            let parsed = if raw.trim().is_empty() {
                None
            } else {
                match raw.trim().parse() {
                    Ok(v) => Some(v),
                    Err(_) => {
                        error("Проверьте IP подключения");
                        return;
                    }
                }
            };
            if Api::new(&address.value(), false, None).is_err() {
                error("Нужен HTTPS-адрес управления без пути");
                return;
            }
            p.management_server = address.value();
            p.management_ip = parsed;
            if let Err(e) = p.save(&directory) {
                error(e);
            }
        }
    });
    refresh.set_callback({
        let state = state.clone();
        let entity = entity.clone();
        move |_| {
            let e = entity_name(&entity);
            request(
                &state,
                sender,
                format!("v2/admin/{e}"),
                "GET",
                None,
                Some(e.into()),
            );
        }
    });
    add.set_callback({
        let state = state.clone();
        let entity = entity.clone();
        move |_| {
            let e = entity_name(&entity);
            if e == "users" {
                request(
                    &state,
                    sender,
                    "v2/admin/roles".into(),
                    "GET",
                    None,
                    Some("new_user".into()),
                );
            } else if let Some(body) = edit_form(e, None, &[]) {
                request(
                    &state,
                    sender,
                    format!("v2/admin/{e}"),
                    "POST",
                    Some(body),
                    None,
                );
            }
        }
    });
    edit.set_callback({
        let state = state.clone();
        let entity = entity.clone();
        let admin = admin.clone();
        let data = data.clone();
        move |_| {
            let e = entity_name(&entity);
            let rows = data.borrow();
            let Some(v) = rows.get((admin.value() - 1).max(0) as usize) else {
                return;
            };
            let id = v["id"].as_i64().unwrap_or(0);
            if e == "users" {
                state.borrow_mut().editing = Some(v.clone());
                request(
                    &state,
                    sender,
                    "v2/admin/roles".into(),
                    "GET",
                    None,
                    Some("edit_user".into()),
                );
            } else if let Some(body) = edit_form(e, Some(v), &[]) {
                request(
                    &state,
                    sender,
                    format!("v2/admin/{e}/{id}"),
                    "PUT",
                    Some(body),
                    None,
                );
            }
        }
    });
    disable.set_callback({
        let state = state.clone();
        let entity = entity.clone();
        let admin = admin.clone();
        let data = data.clone();
        move |_| {
            let e = entity_name(&entity);
            let rows = data.borrow();
            let Some(v) = rows.get((admin.value() - 1).max(0) as usize) else {
                return;
            };
            let id = v["id"].as_i64().unwrap_or(0);
            if fltk::dialog::choice2_default(
                "Отключить пользователя, закрыть канал или удалить роль? История сохраняется.",
                "Отмена",
                "Продолжить",
                "",
            ) == Some(1)
            {
                request(
                    &state,
                    sender,
                    format!("v2/admin/{e}/{id}"),
                    "DELETE",
                    None,
                    None,
                );
            }
        }
    });
    for (mut button, action) in [
        (invite, "invitations"),
        (devices, "devices"),
        (revoke_invite, "revoke_invitations"),
    ] {
        let state = state.clone();
        let entity = entity.clone();
        let admin = admin.clone();
        let data = data.clone();
        button.set_callback(move |_| {
            if entity_name(&entity) != "users" {
                error("Выберите пользователя");
                return;
            }
            let rows = data.borrow();
            let Some(v) = rows.get((admin.value() - 1).max(0) as usize) else {
                return;
            };
            let id = v["id"].as_i64().unwrap_or(0);
            let (path, method, event) = match action {
                "devices" => (
                    format!("v2/admin/users/{id}/devices"),
                    "GET",
                    Some("devices".into()),
                ),
                "invitations" => (format!("v2/admin/users/{id}/invitations"), "POST", None),
                _ => (format!("v2/admin/users/{id}/invitations"), "DELETE", None),
            };
            request(&state, sender, path, method, None, event);
        });
    }
    rules.set_callback({
        let state = state.clone();
        let entity = entity.clone();
        let admin = admin.clone();
        let data = data.clone();
        move |_| {
            if entity_name(&entity) != "channels" {
                error("Выберите канал");
                return;
            }
            let rows = data.borrow();
            let Some(v) = rows.get((admin.value() - 1).max(0) as usize) else {
                return;
            };
            state.borrow_mut().editing = Some(v.clone());
            request(
                &state,
                sender,
                "v2/admin/roles".into(),
                "GET",
                None,
                Some("rule_roles".into()),
            );
        }
    });
    // Profile controls are placed in their own compact tab so every setting is reachable.
    let profile_group = Group::new(15, 75, 950, 610, "Профиль");
    let mut nick = Input::new(200, 120, 490, 35, "Мой ник:");
    let mut save_nick = Button::new(200, 180, 230, 35, "Сохранить профиль");
    if let Some(p) = state.borrow().profile.as_ref() {
        nick.set_value(&p.user_name);
    }
    let mut color = Input::new(200, 235, 300, 35, "RGB ника:");
    color.set_value("128,128,128");
    let mut choose_color = Button::new(515, 235, 175, 35, "Выбрать цвет");
    choose_color.set_callback({
        let mut color = color.clone();
        move |_| {
            if let Some((r, g, b)) =
                fltk::dialog::color_chooser("Цвет ника", fltk::dialog::ColorMode::Rgb)
            {
                color.set_value(&format!("{r},{g},{b}"));
            }
        }
    });
    save_nick.set_callback({
        let state = state.clone();
        let nick = nick.clone();
        let color = color.clone();
        move |_| {
            let s = state.borrow();
            if let Ok((a, p)) = api(&s) {
                let name = nick.value();
                let color = match rgb_color(&color.value()) {
                    Some(c) => c,
                    None => {
                        error("RGB: три числа от 0 до 255 через запятую");
                        return;
                    }
                };
                s.runtime.spawn(async move {
                    sender.send(Ui::V2(Event::Renamed(
                        a.profile_color(&p, name, color)
                            .await
                            .map_err(|e| e.to_string()),
                    )));
                });
            }
        }
    });
    profile_group.end();
    // Keep ownership of the composer clone; insertion is performed by handle().
    draft.set_trigger(fltk::enums::CallbackTrigger::Changed);
    View {
        contacts,
        admin,
        nick,
        color,
        save_nick,
        management,
        admin_data: data,
    }
}
fn entity_name(c: &Choice) -> &'static str {
    match c.value() {
        1 => "channels",
        2 => "roles",
        _ => "users",
    }
}
pub fn mention_request(state: &Rc<RefCell<State>>, sender: app::Sender<Ui>) {
    mention_request_at(state, sender, usize::MAX);
}
pub fn mention_request_at(state: &Rc<RefCell<State>>, sender: app::Sender<Ui>, position: usize) {
    let s = state.borrow();
    let id = s.selected;
    if let Ok((a, p)) = api(&s) {
        s.runtime.spawn(async move {
            sender.send(Ui::V2(Event::Mention(
                id,
                position,
                a.participants(&p, id).await.map_err(|e| e.to_string()),
            )));
        });
    }
}
fn pick(title: &str, names: &[String]) -> Option<usize> {
    let mut w = Window::new(220, 160, 520, 430, title);
    w.make_modal(true);
    let mut list = HoldBrowser::new(20, 20, 480, 330, "");
    for n in names {
        list.add(n);
    }
    let mut yes = Button::new(20, 370, 200, 35, "Выбрать");
    let mut no = Button::new(260, 370, 240, 35, "Отмена");
    w.end();
    let selected = Rc::new(RefCell::new(None));
    yes.set_callback({
        let selected = selected.clone();
        let mut w = w.clone();
        move |_| {
            if list.value() > 0 {
                *selected.borrow_mut() = Some((list.value() - 1) as usize);
                w.hide();
            }
        }
    });
    no.set_callback({
        let mut w = w.clone();
        move |_| w.hide()
    });
    w.show();
    while w.shown() {
        app::wait();
    }
    *selected.borrow()
}
fn permission_label(p: Permission) -> &'static str {
    match p {
        Permission::Profile => "Изменение профиля",
        Permission::Contacts => "Контакты",
        Permission::Direct => "Личная переписка",
        Permission::Read => "Чтение",
        Permission::Send => "Отправка",
        Permission::Mention => "Упоминания",
        Permission::ManageChannel => "Управление каналом",
        Permission::Users => "Пользователи",
        Permission::Channels => "Каналы",
        Permission::Devices => "Устройства",
        Permission::Invitations => "Приглашения",
        Permission::Roles => "Роли",
    }
}
fn edit_form(entity: &'static str, old: Option<&Value>, roles: &[Role]) -> Option<Value> {
    let mut w = Window::new(
        220,
        120,
        600,
        610,
        if entity == "users" {
            "Пользователь и роли"
        } else if entity == "roles" {
            "Роль и разрешения"
        } else {
            "Канал"
        },
    );
    w.make_modal(true);
    let mut name = Input::new(155, 20, 410, 35, "Название / ник:");
    name.set_value(old.and_then(|v| v["name"].as_str()).unwrap_or(""));
    let mut flag = CheckButton::new(
        25,
        65,
        500,
        30,
        if entity == "users" {
            "Пользователь отключён"
        } else {
            "Канал закрыт"
        },
    );
    flag.set_value(
        old.and_then(|v| {
            v[if entity == "users" {
                "disabled"
            } else {
                "closed"
            }]
            .as_bool()
        })
        .unwrap_or(false),
    );
    if entity == "roles" {
        flag.hide();
    }
    let mut user_color = Input::new(155, 65, 410, 30, "RGB:");
    user_color.set_tooltip(
        "Пустое поле при создании — случайный цвет. Иначе три числа 0–255 через запятую.",
    );
    user_color.set_value(
        &old.and_then(|v| v["color"].as_str())
            .map(rgb_string)
            .unwrap_or_default(),
    );
    if entity != "users" {
        user_color.hide();
    } else {
        flag.resize(25, 100, 500, 30);
    }
    let mut role_checks = Vec::new();
    let mut perms = Vec::new();
    if entity == "users" {
        let scroll = Scroll::new(20, 140, 560, 395, "");
        for (i, r) in roles.iter().enumerate() {
            let mut b = CheckButton::new(35, 145 + i as i32 * 34, 510, 30, r.name.as_str());
            b.set_value(
                old.map(|v| {
                    v["roles"]
                        .as_array()
                        .is_some_and(|ids| ids.iter().any(|id| id.as_i64() == Some(r.id)))
                })
                .unwrap_or(r.id == 2),
            );
            role_checks.push((r.id, b));
        }
        scroll.end();
    }
    if entity == "roles" {
        for (i, p) in ALL_PERMISSIONS.iter().enumerate() {
            let mut c = Choice::new(300, 105 + i as i32 * 33, 265, 29, permission_label(*p));
            c.add_choice("Не задано|Разрешить|Запретить");
            let key = serde_json::to_value(p).unwrap();
            c.set_value(
                if old.is_some_and(|v| v["deny"].as_array().is_some_and(|a| a.contains(&key))) {
                    2
                } else if old
                    .is_some_and(|v| v["allow"].as_array().is_some_and(|a| a.contains(&key)))
                {
                    1
                } else {
                    0
                },
            );
            perms.push((*p, c));
        }
    }
    let mut save = Button::new(25, 560, 230, 35, "Сохранить");
    let mut cancel = Button::new(300, 560, 265, 35, "Отмена");
    w.end();
    let result = Rc::new(RefCell::new(None));
    save.set_callback({let result=result.clone();let mut w=w.clone();move |_|{let mut body=match entity{"users"=>json!({"name":name.value().trim(),"disabled":flag.value(),"roles":role_checks.iter().filter(|(_,b)|b.value()).map(|(id,_)|id).collect::<Vec<_>>()}),"roles"=>json!({"name":name.value().trim(),"allow":perms.iter().filter(|(_,c)|c.value()==1).map(|(p,_)|p).collect::<Vec<_>>(),"deny":perms.iter().filter(|(_,c)|c.value()==2).map(|(p,_)|p).collect::<Vec<_>>()}),_=>json!({"name":name.value().trim(),"closed":flag.value()})};if entity=="users" && !user_color.value().trim().is_empty(){match rgb_color(&user_color.value()){Some(color)=>body["color"]=json!(color),None=>{error("RGB: три числа от 0 до 255");return;}}}*result.borrow_mut()=Some(body);w.hide();}});
    cancel.set_callback({
        let mut w = w.clone();
        move |_| w.hide()
    });
    w.show();
    while w.shown() {
        app::wait();
    }
    result.borrow_mut().take()
}
fn rule_form(old: &[ChannelRule], role: i64) -> Option<ChannelRule> {
    let mut w = Window::new(220, 180, 600, 300, "Правила роли в канале");
    w.make_modal(true);
    let mut choices = Vec::new();
    for (i, p) in [
        Permission::Read,
        Permission::Send,
        Permission::Mention,
        Permission::ManageChannel,
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = Choice::new(290, 20 + i as i32 * 45, 280, 32, permission_label(p));
        c.add_choice("Не задано|Разрешить|Запретить");
        c.set_value(
            if old.iter().any(|r| r.role_id == role && r.deny.contains(&p)) {
                2
            } else if old
                .iter()
                .any(|r| r.role_id == role && r.allow.contains(&p))
            {
                1
            } else {
                0
            },
        );
        choices.push((p, c));
    }
    let mut save = Button::new(30, 230, 240, 35, "Сохранить");
    let mut cancel = Button::new(300, 230, 270, 35, "Отмена");
    w.end();
    let result = Rc::new(RefCell::new(None));
    save.set_callback({
        let result = result.clone();
        let mut w = w.clone();
        move |_| {
            *result.borrow_mut() = Some(ChannelRule {
                role_id: role,
                allow: choices
                    .iter()
                    .filter(|(_, c)| c.value() == 1)
                    .map(|(p, _)| *p)
                    .collect(),
                deny: choices
                    .iter()
                    .filter(|(_, c)| c.value() == 2)
                    .map(|(p, _)| *p)
                    .collect(),
            });
            w.hide();
        }
    });
    cancel.set_callback({
        let mut w = w.clone();
        move |_| w.hide()
    });
    w.show();
    while w.shown() {
        app::wait();
    }
    result.borrow_mut().take()
}
pub fn handle(
    event: Event,
    state: &Rc<RefCell<State>>,
    sender: app::Sender<Ui>,
    view: &mut View,
    draft: &mut fltk::input::MultilineInput,
) {
    match event {
        Event::Contacts(r) => match r {
            Ok(users) => {
                view.contacts.clear();
                for u in &users {
                    view.contacts.add(&colored_name(u));
                }
                state.borrow_mut().contacts = users;
            }
            Err(e) => error(e),
        },
        Event::Direct(r) => match r {
            Ok(c) => {
                let mut s = state.borrow_mut();
                s.selected = c.id;
                s.follow_latest = true;
                s.offset = 0;
                if let Err(e) = s.store.select_channel(c.id) {
                    error(e);
                }
                if !s.channels.iter().any(|ch| ch.id == c.id) {
                    s.channels.push(c);
                }
                s.restart.send_modify(|v| *v += 1);
            }
            Err(e) => error(e),
        },
        Event::Renamed(r) => match r {
            Ok(user) => {
                view.nick.set_value(&user.name);
                let mut s = state.borrow_mut();
                let directory = s.options.directory.clone();
                if let Some(p) = s.profile.as_mut() {
                    p.user_name = user.name;
                    if let Err(e) = p.save(&directory) {
                        error(e);
                    }
                }
            }
            Err(e) => error(e),
        },
        Event::Diagnostics(report) => fltk::dialog::message_default(&report.join("\n\n")),
        Event::Mention(id, position, r) => match r {
            Ok(users) => {
                if id != state.borrow().selected {
                    return;
                }
                if let Some(index) = pick(
                    "Упомянуть участника",
                    &users.iter().map(colored_name).collect::<Vec<_>>(),
                ) {
                    let u = &users[index];
                    let s = state.borrow();
                    let old = draft.value();
                    if let Err(e) = s.store.save_draft(id, &old) {
                        error(e);
                        return;
                    }
                    let caret = if position == usize::MAX {
                        draft.position().max(0) as usize
                    } else {
                        position
                    };
                    if caret > old.len() || !old.is_char_boundary(caret) {
                        return;
                    }
                    let byte_start = if old[..caret].ends_with('@') {
                        caret - 1
                    } else {
                        caret
                    };
                    let start = old[..byte_start].chars().count();
                    let insertion = format!("@{} ", u.name);
                    let text = format!("{}{}{}", &old[..byte_start], insertion, &old[caret..]);
                    let end = start + u.name.chars().count() + 1;
                    let mut mentions = poknite_client::store::adjust_mentions(
                        &old,
                        &text,
                        &s.store.draft_mentions(id).unwrap_or_default(),
                    );
                    mentions.push(Mention {
                        user_id: u.id,
                        start,
                        end,
                    });
                    mentions.sort_by_key(|m| m.start);
                    match s.store.save_draft_with_mentions(id, &text, &mentions) {
                        Ok(_) => {
                            draft.set_value(&text);
                            draft
                                .set_position((byte_start + insertion.len()) as i32)
                                .ok();
                        }
                        Err(e) => error(e),
                    }
                }
            }
            Err(e) => error(e),
        },
        Event::Done(r) => match r {
            Ok(v) => {
                if let Some(code) = v["invitation"].as_str() {
                    app::copy(code);
                    fltk::dialog::message_default(
                        "Приглашение скопировано. Оно действует 15 минут и используется один раз.",
                    );
                } else {
                    fltk::dialog::message_default("Изменения сохранены. Обновите список.");
                }
            }
            Err(e) => error(e),
        },
        Event::Admin(kind, r) => match r {
            Err(e) => error(e),
            Ok(v) => {
                if kind == "new_user" || kind == "edit_user" {
                    let roles = serde_json::from_value::<Vec<Role>>(v).unwrap_or_default();
                    let old = if kind == "edit_user" {
                        state.borrow_mut().editing.take()
                    } else {
                        None
                    };
                    if let Some(body) = edit_form("users", old.as_ref(), &roles) {
                        let path = if let Some(old) = old {
                            format!("v2/admin/users/{}", old["id"])
                        } else {
                            "v2/admin/users".into()
                        };
                        request(
                            state,
                            sender,
                            path,
                            if kind == "edit_user" { "PUT" } else { "POST" },
                            Some(body),
                            None,
                        );
                    }
                } else if kind == "devices" {
                    let rows = v.as_array().cloned().unwrap_or_default();
                    if let Some(index) = pick(
                        "Выберите устройство для отключения",
                        &rows
                            .iter()
                            .map(|d| d["name"].as_str().unwrap_or("").to_string())
                            .collect::<Vec<_>>(),
                    ) {
                        request(
                            state,
                            sender,
                            format!("v2/admin/devices/{}", rows[index]["id"]),
                            "DELETE",
                            None,
                            None,
                        );
                    }
                } else if kind == "rule_roles" {
                    let roles = serde_json::from_value::<Vec<Role>>(v).unwrap_or_default();
                    if let Some(index) = pick(
                        "Выберите роль",
                        &roles.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
                    ) {
                        state.borrow_mut().rule_role = roles[index].id;
                        let id = state
                            .borrow()
                            .editing
                            .as_ref()
                            .and_then(|v| v["id"].as_i64())
                            .unwrap_or(0);
                        request(
                            state,
                            sender,
                            format!("v2/admin/channels/{id}/rules"),
                            "GET",
                            None,
                            Some("rules".into()),
                        );
                    }
                } else if kind == "rules" {
                    let mut rules =
                        serde_json::from_value::<Vec<ChannelRule>>(v).unwrap_or_default();
                    let role = state.borrow().rule_role;
                    if let Some(rule) = rule_form(&rules, role) {
                        rules.retain(|r| r.role_id != role);
                        rules.push(rule);
                        let id = state
                            .borrow()
                            .editing
                            .as_ref()
                            .and_then(|v| v["id"].as_i64())
                            .unwrap_or(0);
                        request(
                            state,
                            sender,
                            format!("v2/admin/channels/{id}/rules"),
                            "PUT",
                            Some(json!(rules)),
                            None,
                        );
                    }
                } else {
                    let rows = v.as_array().cloned().unwrap_or_default();
                    view.admin.clear();
                    for row in &rows {
                        view.admin.add(&format!(
                            "{}{}",
                            row["name"].as_str().unwrap_or(""),
                            if row["disabled"].as_bool() == Some(true)
                                || row["closed"].as_bool() == Some(true)
                            {
                                " (отключён / закрыт)"
                            } else {
                                ""
                            }
                        ));
                    }
                    *view.admin_data.borrow_mut() = rows;
                }
            }
        },
    }
}

pub fn rgb_color(raw: &str) -> Option<String> {
    if poknite_protocol::valid_color(raw) {
        return Some(raw.to_lowercase());
    }
    let parts = raw
        .split(',')
        .map(|p| p.trim().parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if parts.len() != 3 {
        return None;
    }
    Some(format!("#{:02x}{:02x}{:02x}", parts[0], parts[1], parts[2]))
}
pub fn rgb_string(color: &str) -> String {
    let color = u32::from_str_radix(color.trim_start_matches('#'), 16).unwrap_or(0x808080);
    format!(
        "{},{},{}",
        (color >> 16) & 255,
        (color >> 8) & 255,
        color & 255
    )
}
pub fn colored_name(user: &User) -> String {
    let c = u32::from_str_radix(user.color.trim_start_matches('#'), 16).unwrap_or(0x808080);
    format!(
        "@C{}@.{}",
        fltk::enums::Color::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8).bits(),
        user.name
    )
}
