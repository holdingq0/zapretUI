use rkn_core::ipc::{Phase, SiteState, SiteStatus, State, Status, Update, UpdateState};
use rkn_core::sites::SiteMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
    Mute,
}

pub fn plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let (d, h) = (n % 10, n % 100);
    if d == 1 && h != 11 {
        one
    } else if (2..=4).contains(&d) && !(12..=14).contains(&h) {
        few
    } else {
        many
    }
}

pub fn phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Probing => "Смотрю, что режет провайдер",
        Phase::Round1 => "Пробую способы обхода",
        Phase::Round2 => "Проверяю лучшие на всех сервисах",
        Phase::Done => "Готово, запомнил для этой сети",
        Phase::Failed => "Настройка остановлена",
    }
}

pub struct View {
    pub title: String,
    pub sub: String,
    pub action: Option<&'static str>,
}

pub fn view(connected: bool, missing: bool, stopped: bool, status: Option<&Status>) -> View {
    let v = |title: &str, sub: String, action: Option<&'static str>| View { title: title.into(), sub, action };
    if missing && rkn_core::paths::portable() {
        return v("Не запущено", "Нужны права администратора: подтвердите запрос Windows".into(), Some("Запустить"));
    }
    if missing {
        return v("Не установлено", "Служба zapretUI работает в фоне и обходит блокировки. Права администратора нужны один раз".into(), Some("Установить"));
    }
    let Some(status) = status.filter(|_| connected) else {
        return match stopped {
            true => v("Нет связи", "Служба zapretUI остановлена. Запустите её снова".into(), Some("Запустить")),
            false => v("Нет связи", "Жду ответа службы zapretUI".into(), None),
        };
    };
    let message = || status.message.clone();
    match status.state {
        State::Working => {
            // Games without their own domains share their launcher's row and are not counted twice.
            let own = |s: &&SiteStatus| !s.id.starts_with("game:");
            let n = status.sites.iter().filter(own).filter(|s| s.state == SiteState::Bypassed).count();
            let sub = if n > 0 {
                format!("Через обход {} {n} {}", plural(n, "идёт", "идут", "идут"), plural(n, "сервис", "сервиса", "сервисов"))
            } else if status.sites.iter().any(|s| s.state == SiteState::Checking) {
                "Проверяю, что режет провайдер".into()
            } else {
                "Провайдер пока ничего не режет".into()
            };
            v("Включено", sub, None)
        }
        State::Paused => v("Выключено", String::new(), None),
        State::Starting => v("Запуск", "Включаю обход".into(), None),
        State::Learning => {
            let sub = status.learn.as_ref().map(|l| phase(l.phase).to_string()).unwrap_or_else(|| "Подбираю способ под вашего провайдера".into());
            v("Настраиваю", sub, Some("Остановить"))
        }
        State::Tuning => v("Нужна настройка", message().unwrap_or_else(|| "Провайдер что-то поменял".into()), Some("Настроить")),
        State::Whitelist => v("Белый список", message().unwrap_or_else(|| "Оператор открывает только разрешённые сайты".into()), None),
        State::EngineMissing | State::Failed => {
            v("Ошибка", message().unwrap_or_else(|| "Не удалось запустить обход".into()), Some("Настроить заново"))
        }
    }
}

/// The settings line about the version and updates, with its colour.
pub fn update(update: &Update) -> (String, Tone) {
    let new = update.version.clone().unwrap_or_default();
    match update.state {
        UpdateState::Off => ("Обновления выключены в этой сборке".into(), Tone::Mute),
        UpdateState::Idle => ("Обновления проверяются сами".into(), Tone::Mute),
        UpdateState::Checking => ("Проверяю обновления".into(), Tone::Mute),
        UpdateState::Latest => ("Это последняя версия".into(), Tone::Mute),
        UpdateState::Available => (format!("Вышла версия {new}"), Tone::Ok),
        UpdateState::Downloading => (format!("Скачиваю версию {new}"), Tone::Mute),
        UpdateState::Installing => (format!("Устанавливаю версию {new}, перезапущусь сам"), Tone::Mute),
        UpdateState::Failed => (update.error.clone().unwrap_or_else(|| "Обновление не удалось".into()), Tone::Warn),
    }
}

pub fn site_short(site: &SiteStatus, learning: bool) -> (&'static str, Tone) {
    match site.state {
        SiteState::Bypassed => ("через обход", Tone::Ok),
        SiteState::Direct if site.mode == SiteMode::Never => ("всегда напрямую", Tone::Mute),
        SiteState::Direct => ("напрямую", Tone::Mute),
        SiteState::Checking => ("проверка", Tone::Mute),
        SiteState::Tuning if learning => ("подбираю", Tone::Warn),
        SiteState::Tuning => ("не подобран", Tone::Warn),
        SiteState::Partial => ("режется", Tone::Warn),
        SiteState::Blocked => ("не работает", Tone::Bad),
        SiteState::IpBlocked => ("блок по IP", Tone::Bad),
        SiteState::DnsSpoofed => ("подмена DNS", Tone::Bad),
        SiteState::Geoblocked => ("закрыт для РФ", Tone::Bad),
        SiteState::Unknown => ("не проверялся", Tone::Mute),
    }
}
