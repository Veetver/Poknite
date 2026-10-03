use crate::State;
use fltk::{
    browser::HoldBrowser,
    button::Button,
    enums::{Align, Color, Font, FrameType},
    frame::Frame,
    prelude::*,
};
use poknite_protocol::{Channel, ConversationKind, Permission};

#[derive(Clone)]
pub struct Controls {
    pub title: Frame,
    pub hint: Frame,
    pub send: Button,
    pub mention: Button,
}
pub fn matching<'a>(channels: &'a [Channel], query: &str) -> Vec<&'a Channel> {
    let query = query.trim().to_lowercase();
    channels
        .iter()
        .filter(|c| c.name.to_lowercase().contains(&query))
        .collect()
}
pub fn fill_list(state: &State, browser: &mut HoldBrowser, query: &str) {
    browser.clear();
    for (index, c) in matching(&state.channels, query).into_iter().enumerate() {
        browser.add(&format!(
            "@.{}{}{}",
            if c.kind == ConversationKind::Direct {
                "Лично · "
            } else {
                "# "
            },
            c.name,
            if c.closed { " · закрыт" } else { "" }
        ));
        if c.id == state.selected {
            browser.select(index as i32 + 1);
        }
    }
}
pub fn refresh_controls(state: &State, controls: &mut Controls, text: &str) {
    let current = state.channels.iter().find(|c| c.id == state.selected);
    let title = current
        .map(|c| c.name.as_str())
        .unwrap_or("Выберите разговор")
        .replace('@', "@@");
    if controls.title.label() != title {
        controls.title.set_label(&title);
    }
    let can_send = current.is_some_and(|c| c.actions.contains(&Permission::Send));
    let online = state.status == poknite_client::ConnectionStatus::Connected;
    let hint = match current {
        None => "Каналы и личные диалоги появятся после подключения",
        Some(c) if c.closed => "Канал закрыт · доступна история",
        Some(_) if !can_send => "Только чтение · отправка недоступна",
        Some(_) if !online => "Нет связи · черновик сохраняется на устройстве",
        Some(c) if c.kind == ConversationKind::Direct => "Личный диалог · только вы и собеседник",
        Some(_) => "Канал · введите @@, чтобы выбрать адресата уведомления",
    };
    if controls.hint.label() != hint {
        controls.hint.set_label(hint);
    }
    if can_send && online && poknite_protocol::valid_text(text) {
        controls.send.activate();
    } else {
        controls.send.deactivate();
    }
    if online && current.is_some_and(|c| c.actions.contains(&Permission::Mention)) {
        controls.mention.activate();
    } else {
        controls.mention.deactivate();
    }
}
pub fn heading(x: i32, y: i32, w: i32) -> Frame {
    let mut frame = Frame::new(x, y, w, 30, "");
    frame.set_align(Align::Left | Align::Inside);
    frame.set_label_size(21);
    frame.set_label_font(Font::HelveticaBold);
    frame
}
pub fn hint(x: i32, y: i32, w: i32) -> Frame {
    let mut frame = Frame::new(x, y, w, 24, "");
    frame.set_align(Align::Left | Align::Inside);
    frame.set_label_size(12);
    frame.set_label_color(Color::from_rgb(92, 105, 104));
    frame
}
pub fn flat<W: WidgetExt>(widget: &mut W) {
    widget.set_frame(FrameType::FlatBox);
    widget.set_color(Color::White);
}
