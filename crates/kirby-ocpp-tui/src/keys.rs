//! Key handling. Each [`Mode`] owns its keys; nothing falls through to another mode.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::ids::IdTag;
use kirby_ocpp_core::ocpp::types::{
    ChargePointErrorCode, ConnectorStatus, DiagnosticsStatus, FirmwareStatus, StopReason,
};
use kirby_ocpp_core::station::{Command, DiagnosticsOutcome, FirmwareOutcome, ProfileSummary};

use crate::actions::{ActionId, Tab};
use crate::app::{App, Confirm, ConfirmAction, Detail, Effect, Focus, Mode, NoticeLevel};
use crate::browser::{Browser, Page};
use crate::forms::{Choice, ChoiceKind, Form, FormEvent};
use crate::palette::Palette;

pub fn handle(app: &mut App, key: KeyEvent, fx: &mut Vec<Effect>) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    match std::mem::replace(&mut app.mode, Mode::Normal) {
        Mode::Normal => normal(app, key, fx),
        Mode::Palette(p) => palette(app, p, key, fx),
        Mode::Form(f) => form(app, f, key, fx),
        Mode::Choice(c) => choice(app, c, key, fx),
        Mode::Help => {
            if !matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::Enter
            ) {
                app.mode = Mode::Help;
            }
        }
        Mode::Detail(d) => detail(app, d, key, fx),
        Mode::Confirm(c) => match key.code {
            KeyCode::Char('y' | 'Y') => confirmed(app, c.action, fx),
            _ => app.notify(NoticeLevel::Info, "cancelled"),
        },
        Mode::Browser(b) => browser(app, b, key, fx),
    }
    // A form, confirmation or detail opened from the browser gives the screen back to it.
    if matches!(app.mode, Mode::Normal)
        && let Some(b) = app.suspended.take()
    {
        app.mode = Mode::Browser(b);
    }
}

fn confirmed(app: &mut App, action: ConfirmAction, fx: &mut Vec<Effect>) {
    let send = |fx: &mut Vec<Effect>, label: &str, command: Command| {
        fx.push(Effect::Send {
            label: label.to_owned(),
            command,
            show_response: false,
        });
    };
    match action {
        ConfirmAction::ClearLog => {
            app.log.clear();
            app.notify(NoticeLevel::Info, "log cleared");
        }
        ConfirmAction::ClearLocalList => send(fx, "clear local list", Command::ClearLocalList),
        ConfirmAction::RemoveLocalEntry(id_tag) => {
            let label = format!("remove {id_tag}");
            send(fx, &label, Command::RemoveLocalListEntry { id_tag });
        }
        ConfirmAction::ClearProfiles => send(
            fx,
            "clear charging profiles",
            Command::ClearChargingProfiles { id: None },
        ),
        ConfirmAction::RemoveProfile(id) => {
            let label = format!("remove profile {id}");
            send(fx, &label, Command::ClearChargingProfiles { id: Some(id) });
        }
    }
}

/// Keys of the data browser. `b` is taken out of `app.mode` by the caller; whatever it opens
/// (a form, a confirmation, a detail) goes into `app.mode` and the browser into `app.suspended`.
fn browser(app: &mut App, mut b: Browser, key: KeyEvent, fx: &mut Vec<Effect>) {
    let _ = fx;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if b.filtering {
        match key.code {
            KeyCode::Esc => {
                b.filter = crate::text::TextInput::default();
                b.filtering = false;
                b.reset_selection();
            }
            KeyCode::Enter => b.filtering = false,
            KeyCode::Up => b.move_by(-1, &app.snapshot),
            KeyCode::Down => b.move_by(1, &app.snapshot),
            KeyCode::Left => b.filter.left(),
            KeyCode::Right => b.filter.right(),
            KeyCode::Home => b.filter.home(),
            KeyCode::End => b.filter.end(),
            KeyCode::Backspace => {
                b.filter.backspace();
                b.reset_selection();
            }
            KeyCode::Delete => {
                b.filter.delete();
                b.reset_selection();
            }
            KeyCode::Char('u') if ctrl => {
                b.filter = crate::text::TextInput::default();
                b.reset_selection();
            }
            KeyCode::Char(c) if !ctrl => {
                b.filter.insert_char(c);
                b.reset_selection();
            }
            _ => {}
        }
        app.mode = Mode::Browser(b);
        return;
    }
    let snap = &app.snapshot;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            if b.filter.is_empty() {
                return;
            }
            b.filter = crate::text::TextInput::default();
            b.reset_selection();
        }
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => b.switch_page(1),
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => b.switch_page(-1),
        KeyCode::Char(d @ '1'..='4') => {
            while b.page.index() != usize::from(d as u8 - b'1') {
                b.switch_page(1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => b.move_by(-1, snap),
        KeyCode::Down | KeyCode::Char('j') => b.move_by(1, snap),
        KeyCode::PageUp => b.move_by(-10, snap),
        KeyCode::PageDown => b.move_by(10, snap),
        KeyCode::Home | KeyCode::Char('g') => b.select_edge(false, snap),
        KeyCode::End | KeyCode::Char('G') => b.select_edge(true, snap),
        KeyCode::Char('/') => b.filtering = true,
        KeyCode::Char(c) if !ctrl => {
            if let Some(next) = page_action(app, &b, key.code, c) {
                app.suspended = Some(b);
                app.mode = next;
                return;
            }
        }
        KeyCode::Enter | KeyCode::Delete => {
            if let Some(next) = page_action(app, &b, key.code, '\n') {
                app.suspended = Some(b);
                app.mode = next;
                return;
            }
        }
        _ => {}
    }
    app.mode = Mode::Browser(b);
}

/// What the page-specific key opens, if anything (an explanation goes to the notice line).
fn page_action(app: &mut App, b: &Browser, code: KeyCode, c: char) -> Option<Mode> {
    let snap = &app.snapshot;
    let row = b.selected_row(snap);
    let enter = code == KeyCode::Enter;
    let delete = code == KeyCode::Delete || c == 'd';
    let max_connector = snap.connectors.last().map_or(1, |c| c.id.get());
    match b.page {
        Page::Config if enter || c == 'e' => {
            let item = row.and_then(|r| snap.config.get(r.index))?;
            if item.readonly {
                let text = format!("{} is read-only", item.key);
                app.notify(NoticeLevel::Error, text);
                return None;
            }
            Some(Mode::Form(Form::config_value(item, snap)))
        }
        Page::LocalList if c == 'a' => Some(Mode::Form(Form::local_entry(None))),
        Page::LocalList if enter || c == 'e' => {
            let entry = row.and_then(|r| snap.local_list.entries.get(r.index))?;
            Some(Mode::Form(Form::local_entry(Some(entry))))
        }
        Page::LocalList if delete => {
            let entry = row.and_then(|r| snap.local_list.entries.get(r.index))?;
            let id_tag = IdTag::new(entry.id_tag.clone()).ok()?;
            Some(Mode::Confirm(Confirm {
                message: format!("Remove {id_tag} from the local list?"),
                action: ConfirmAction::RemoveLocalEntry(id_tag),
            }))
        }
        Page::LocalList if c == 'c' => {
            if snap.local_list.total == 0 {
                app.notify(NoticeLevel::Error, "the local list is empty");
                return None;
            }
            Some(Mode::Confirm(Confirm {
                message: format!("Clear the local list ({} entries)?", snap.local_list.total),
                action: ConfirmAction::ClearLocalList,
            }))
        }
        Page::Profiles if c == 'i' => Some(Mode::Form(Form::profile(None, max_connector))),
        Page::Profiles if c == 'e' => {
            let p = row.and_then(|r| snap.charging_profiles.get(r.index))?;
            Some(Mode::Form(Form::profile(Some(p), max_connector)))
        }
        Page::Profiles if enter => {
            let p = row.and_then(|r| snap.charging_profiles.get(r.index))?;
            Some(Mode::Detail(profile_detail(p)))
        }
        Page::Profiles if delete => {
            let p = row.and_then(|r| snap.charging_profiles.get(r.index))?;
            Some(Mode::Confirm(Confirm {
                message: format!("Remove charging profile {}?", p.id),
                action: ConfirmAction::RemoveProfile(p.id),
            }))
        }
        Page::Profiles if c == 'c' => {
            if snap.charging_profiles.is_empty() {
                app.notify(NoticeLevel::Error, "no charging profiles are installed");
                return None;
            }
            Some(Mode::Confirm(Confirm {
                message: format!(
                    "Clear all {} charging profiles?",
                    snap.charging_profiles.len()
                ),
                action: ConfirmAction::ClearProfiles,
            }))
        }
        _ => None,
    }
}

fn profile_detail(p: &ProfileSummary) -> Detail {
    let pretty = serde_json::from_str::<serde_json::Value>(&p.json)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| p.json.clone());
    Detail {
        title: format!("Charging profile {} (connector {})", p.id, p.connector),
        lines: pretty.lines().map(crate::text::sanitize).collect(),
        scroll: 0,
        copy: pretty,
    }
}

fn open_palette(app: &mut App) {
    app.mode = Mode::Palette(Palette::new(&app.catalog));
}

fn normal(app: &mut App, key: KeyEvent, fx: &mut Vec<Effect>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('p') if ctrl => open_palette(app),
        _ if ctrl || alt => {}
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(':') => open_palette(app),
        KeyCode::Char('/') => {
            if app.tab == Tab::Logs && app.focus == Focus::Log {
                app.run_action(ActionId::LogSearch, fx);
            } else {
                open_palette(app);
            }
        }
        KeyCode::Char(d @ '1'..='7') => {
            app.set_tab(Tab::from_index(usize::from(d as u8 - b'1')));
        }
        KeyCode::Tab => app.set_tab(app.tab.next()),
        KeyCode::BackTab => app.set_tab(app.tab.prev()),
        KeyCode::Char('[') => app.move_connector(-1),
        KeyCode::Char(']') => app.move_connector(1),
        KeyCode::Left => app.focus = shift_focus(app.focus, -1),
        KeyCode::Right => app.focus = shift_focus(app.focus, 1),
        KeyCode::Up | KeyCode::Char('k') => step(app, -1),
        KeyCode::Down | KeyCode::Char('j') => step(app, 1),
        KeyCode::PageUp => step(app, -10),
        KeyCode::PageDown => step(app, 10),
        KeyCode::Home if app.focus == Focus::Log => app.log.select_first(),
        KeyCode::End if app.focus == Focus::Log => app.log.follow_tail(),
        KeyCode::Esc => {
            if !app.log.search.is_empty() {
                app.log.search.clear();
                app.notify(NoticeLevel::Info, "search cleared");
            }
        }
        KeyCode::Enter => match app.focus {
            Focus::Actions => {
                if let Some(id) = app.selected_action().map(|a| a.id) {
                    app.run_action(id, fx);
                }
            }
            Focus::Log => app.open_detail(),
            Focus::Connectors => {}
        },
        KeyCode::Char(c) => {
            // Accelerators exist only for the active tab's own actions.
            let id = app
                .catalog
                .iter()
                .find(|a| a.tab == app.tab && a.key == Some(c))
                .map(|a| a.id);
            if let Some(id) = id {
                app.run_action(id, fx);
            }
        }
        _ => {}
    }
}

fn shift_focus(f: Focus, d: isize) -> Focus {
    const ORDER: [Focus; 3] = [Focus::Connectors, Focus::Actions, Focus::Log];
    let i = ORDER.iter().position(|x| *x == f).unwrap_or(1);
    ORDER[i.saturating_add_signed(d).min(2)]
}

fn step(app: &mut App, d: isize) {
    match app.focus {
        Focus::Connectors => app.move_connector(d),
        Focus::Actions => app.move_action(d.signum()),
        Focus::Log => app.log.move_selection(d),
    }
}

fn palette(app: &mut App, mut p: Palette, key: KeyEvent, fx: &mut Vec<Effect>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => return,
        KeyCode::Enter => {
            if let Some(id) = p.chosen().map(|i| app.catalog[i].id) {
                app.run_action(id, fx);
            }
            return;
        }
        KeyCode::Up => p.move_by(-1),
        KeyCode::Down => p.move_by(1),
        KeyCode::Char('p') if ctrl => p.move_by(-1),
        KeyCode::Char('n') if ctrl => p.move_by(1),
        KeyCode::Char('u') if ctrl => {
            p.query = crate::text::TextInput::default();
            p.refresh(&app.catalog);
        }
        KeyCode::Left => p.query.left(),
        KeyCode::Right => p.query.right(),
        KeyCode::Home => p.query.home(),
        KeyCode::End => p.query.end(),
        KeyCode::Delete => {
            p.query.delete();
            p.refresh(&app.catalog);
        }
        KeyCode::Backspace => {
            p.query.backspace();
            p.refresh(&app.catalog);
        }
        KeyCode::Char(c) if !ctrl => {
            p.query.insert_char(c);
            p.refresh(&app.catalog);
        }
        _ => {}
    }
    app.mode = Mode::Palette(p);
}

fn form(app: &mut App, mut f: crate::forms::Form, key: KeyEvent, fx: &mut Vec<Effect>) {
    match f.handle_key(key) {
        FormEvent::Cancel => {}
        FormEvent::Submit => match f.submit() {
            Some(done) => app.apply_form(done, fx),
            None => app.mode = Mode::Form(f),
        },
        FormEvent::None => app.mode = Mode::Form(f),
    }
}

fn choice(app: &mut App, mut c: Choice, key: KeyEvent, fx: &mut Vec<Effect>) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return,
        KeyCode::Up | KeyCode::Char('k') => c.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => c.move_by(1),
        KeyCode::Home => c.selected = 0,
        KeyCode::End => c.selected = c.items.len().saturating_sub(1),
        KeyCode::Enter => {
            if let Some(command) = choice_command(&c) {
                let label = format!("{}: {}", command.name(), c.items[c.selected]);
                fx.push(Effect::Send {
                    label,
                    command,
                    show_response: false,
                });
            }
            return;
        }
        _ => {}
    }
    app.mode = Mode::Choice(c);
}

fn choice_command(c: &Choice) -> Option<Command> {
    let i = c.selected;
    let connector = c.connector;
    Some(match c.kind {
        ChoiceKind::SetStatus => Command::SetConnectorStatus {
            connector,
            status: *ConnectorStatus::ALL.get(i)?,
        },
        ChoiceKind::Fault => Command::Fault {
            connector,
            error_code: *ChargePointErrorCode::ALL
                .iter()
                .filter(|e| **e != ChargePointErrorCode::NoError)
                .nth(i)?,
            info: None,
            vendor_error_code: None,
        },
        ChoiceKind::StopReason => Command::StopTransaction {
            connector,
            reason: *StopReason::ALL.get(i)?,
            id_tag: None,
        },
        ChoiceKind::DiagOutcome => Command::SetDiagnosticsOutcome {
            outcome: *DiagnosticsOutcome::ALL.get(i)?,
        },
        ChoiceKind::FwOutcome => Command::SetFirmwareOutcome {
            outcome: *FirmwareOutcome::ALL.get(i)?,
        },
        ChoiceKind::DiagStatus => Command::DiagnosticsStatusNotification {
            status: *DiagnosticsStatus::ALL.get(i)?,
        },
        ChoiceKind::FwStatus => Command::FirmwareStatusNotification {
            status: *FirmwareStatus::ALL.get(i)?,
        },
    })
}

fn detail(app: &mut App, mut d: Detail, key: KeyEvent, fx: &mut Vec<Effect>) {
    let max = d.lines.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return,
        KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => d.scroll = (d.scroll + 1).min(max),
        KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(10),
        KeyCode::PageDown => d.scroll = (d.scroll + 10).min(max),
        KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
        KeyCode::End | KeyCode::Char('G') => d.scroll = max,
        KeyCode::Char('c' | 'y') => fx.push(Effect::Copy {
            text: d.copy.clone(),
            what: "detail".to_owned(),
        }),
        _ => {}
    }
    app.mode = Mode::Detail(d);
}
