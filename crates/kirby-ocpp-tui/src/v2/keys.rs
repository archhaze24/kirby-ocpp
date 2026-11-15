//! Key handling of the OCPP 2.x model, per mode. Each [`Mode2`] owns its keys; nothing falls
//! through to another mode.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use kirby_ocpp_core::ocpp::v2::messages::Mutability;
use kirby_ocpp_core::ocpp::v2::types::WireEnum;
use kirby_ocpp_core::station2::VariableView;

use super::actions::{ActionId2, Tab2, palette_rows};
use super::app::{App2, ConfirmAction2, Effect2, Mode2};
use super::browser::{BrowserEvent, DeviceBrowser};
use super::forms::{apply_form, choice_effect, is_secret_variable, set_variable_form};
use crate::app::{Detail, Focus, NoticeLevel};
use crate::forms::{Choice, Form, FormEvent};
use crate::palette::Palette;

/// Applies one key press to `app` and appends the effects it causes to `fx`.
pub(super) fn handle_key(app: &mut App2, key: KeyEvent, fx: &mut Vec<Effect2>) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        match &app.mode {
            // A second Ctrl-C while the quit question is open means yes; during another
            // question Ctrl-C keeps its plain meaning and quits.
            Mode2::Confirm(_) => app.quit = true,
            Mode2::Normal => app.request_quit(),
            // A form or another view may hold work: ask, and bring the view back if the
            // answer is no.
            _ if app.quit_reason().is_some() => {
                let previous = std::mem::replace(&mut app.mode, Mode2::Normal);
                app.request_quit();
                app.quit_prev = Some(previous);
            }
            _ => app.quit = true,
        }
        return;
    }
    match std::mem::replace(&mut app.mode, Mode2::Normal) {
        Mode2::Normal => normal(app, key, fx),
        Mode2::Palette(p) => palette(app, p, key, fx),
        Mode2::Form(f) => form(app, f, key, fx),
        Mode2::Choice(c) => choice(app, c, key, fx),
        Mode2::Help => {
            if !matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::Enter
            ) {
                app.mode = Mode2::Help;
            }
        }
        Mode2::Detail(d) => detail(app, d, key, fx),
        Mode2::Confirm(c) => match key.code {
            KeyCode::Char('y' | 'Y') => confirmed(app, &c.action),
            KeyCode::Char('q' | 'Q') if c.action == ConfirmAction2::Quit => app.quit = true,
            _ => {
                if let Some(previous) = app.quit_prev.take() {
                    app.mode = previous;
                }
                app.notify(NoticeLevel::Info, "cancelled");
            }
        },
        Mode2::DeviceModel(b) => device_model(app, b, key),
        Mode2::Pending(sel) => pending(app, sel, key),
    }
    // A form or detail opened from the device model gives the screen back to it.
    if matches!(app.mode, Mode2::Normal)
        && let Some(b) = app.suspended.take()
    {
        app.mode = Mode2::DeviceModel(b);
    }
}

/// Keys of the pending-commands list: move; `x` and `Delete` explain that there is no cancel.
fn pending(app: &mut App2, sel: usize, key: KeyEvent) {
    let last = app.pending.len().saturating_sub(1);
    let sel = match key.code {
        KeyCode::Esc | KeyCode::Char('q' | 'W') | KeyCode::Enter => return,
        KeyCode::Up | KeyCode::Char('k') => sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => (sel + 1).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        KeyCode::Char('x') | KeyCode::Delete => {
            app.notify(
                NoticeLevel::Info,
                "cancel is not available for OCPP 2.0.1 yet",
            );
            sel
        }
        _ => sel,
    };
    app.mode = Mode2::Pending(sel.min(last));
}

fn confirmed(app: &mut App2, action: &ConfirmAction2) {
    match action {
        ConfirmAction2::Quit => app.quit = true,
        ConfirmAction2::ClearLog => {
            app.log.clear();
            app.notify(NoticeLevel::Info, "log cleared");
        }
    }
}

/// Keys of the device-model browser. `b` is taken out of `app.mode` by the caller; whatever it
/// opens (a form, a detail) goes into `app.mode` and the browser into `app.suspended`.
fn device_model(app: &mut App2, mut b: DeviceBrowser, key: KeyEvent) {
    let event = b.handle_key(key, &app.snapshot.variables);
    browser_event(app, b, event);
}

/// Acts on what the browser asked for after a key press.
pub(super) fn browser_event(app: &mut App2, b: DeviceBrowser, event: BrowserEvent) {
    match event {
        BrowserEvent::None => app.mode = Mode2::DeviceModel(b),
        BrowserEvent::Close => {}
        BrowserEvent::Edit(i) => match app.snapshot.variables.get(i).cloned() {
            Some(view) if view.mutability != Mutability::ReadOnly => {
                app.suspended = Some(b);
                app.mode = Mode2::Form(set_variable_form(&view));
            }
            Some(view) => {
                let text = format!(
                    "{}.{} is read-only",
                    crate::text::sanitize(&view.component),
                    crate::text::sanitize(&view.variable)
                );
                app.notify(NoticeLevel::Error, text);
                app.mode = Mode2::DeviceModel(b);
            }
            None => app.mode = Mode2::DeviceModel(b),
        },
        BrowserEvent::Show(i) => match app.snapshot.variables.get(i) {
            Some(view) => {
                let shown = variable_detail(view);
                app.suspended = Some(b);
                app.mode = Mode2::Detail(shown);
            }
            None => app.mode = Mode2::DeviceModel(b),
        },
    }
}

/// The detail view of one variable. A write-only or password value is never shown.
fn variable_detail(view: &VariableView) -> Detail {
    let value = if is_secret_variable(view) {
        "(hidden)".to_owned()
    } else {
        view.value
            .as_deref()
            .map_or_else(|| "(none)".to_owned(), crate::text::sanitize)
    };
    let lines = vec![
        format!("component:   {}", crate::text::sanitize(&view.component)),
        format!(
            "instance:    {}",
            view.instance
                .as_deref()
                .map_or_else(|| "-".to_owned(), crate::text::sanitize)
        ),
        format!("variable:    {}", crate::text::sanitize(&view.variable)),
        format!("mutability:  {}", view.mutability.as_str()),
        format!("value:       {value}"),
    ];
    Detail {
        title: format!(
            "{}.{}",
            crate::text::sanitize(&view.component),
            crate::text::sanitize(&view.variable)
        ),
        copy: lines.join("\n"),
        lines,
        scroll: 0,
    }
}

fn open_palette(app: &mut App2) {
    app.mode = Mode2::Palette(Palette::from_rows(&palette_rows(&app.catalog)));
}

fn normal(app: &mut App2, key: KeyEvent, fx: &mut Vec<Effect2>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('p') if ctrl => open_palette(app),
        _ if ctrl || alt => {}
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('?') => app.mode = Mode2::Help,
        KeyCode::Char('W') => app.mode = Mode2::Pending(0),
        KeyCode::Char(':') => open_palette(app),
        KeyCode::Char('/') => {
            if app.tab == Tab2::Logs && app.focus == Focus::Log {
                app.run_action2(ActionId2::LogSearch, fx);
            } else {
                open_palette(app);
            }
        }
        KeyCode::Char(d @ '1'..='6') => {
            app.set_tab(Tab2::from_index(usize::from(d as u8 - b'1')));
        }
        KeyCode::Tab => app.set_tab(app.tab.next()),
        KeyCode::BackTab => app.set_tab(app.tab.prev()),
        KeyCode::Char('[') => app.move_evse(-1),
        KeyCode::Char(']') => app.move_evse(1),
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
                    app.run_action2(id, fx);
                }
            }
            Focus::Log => app.open_detail2(),
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
                app.run_action2(id, fx);
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

fn step(app: &mut App2, d: isize) {
    match app.focus {
        Focus::Connectors => app.move_evse(d),
        Focus::Actions => app.move_action(d.signum()),
        Focus::Log => app.log.move_selection(d),
    }
}

fn palette(app: &mut App2, mut p: Palette, key: KeyEvent, fx: &mut Vec<Effect2>) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let refresh = |p: &mut Palette, app: &App2| p.refresh_rows(&palette_rows(&app.catalog));
    match key.code {
        KeyCode::Esc => return,
        KeyCode::Enter => {
            if let Some(id) = p.chosen().and_then(|i| app.catalog.get(i)).map(|a| a.id) {
                app.run_action2(id, fx);
            }
            return;
        }
        KeyCode::Up => p.move_by(-1),
        KeyCode::Down => p.move_by(1),
        KeyCode::Char('p') if ctrl => p.move_by(-1),
        KeyCode::Char('n') if ctrl => p.move_by(1),
        KeyCode::Char('u') if ctrl => {
            p.query = crate::text::TextInput::default();
            refresh(&mut p, app);
        }
        KeyCode::Left => p.query.left(),
        KeyCode::Right => p.query.right(),
        KeyCode::Home => p.query.home(),
        KeyCode::End => p.query.end(),
        KeyCode::Delete => {
            p.query.delete();
            refresh(&mut p, app);
        }
        KeyCode::Backspace => {
            p.query.backspace();
            refresh(&mut p, app);
        }
        KeyCode::Char(c) if !ctrl => {
            p.query.insert_char(c);
            refresh(&mut p, app);
        }
        _ => {}
    }
    app.mode = Mode2::Palette(p);
}

fn form(app: &mut App2, mut f: Form, key: KeyEvent, fx: &mut Vec<Effect2>) {
    match f.handle_key(key) {
        FormEvent::Cancel => {}
        FormEvent::Submit => match f.submit() {
            Some(done) => apply_form(app, f, done, fx),
            None => app.mode = Mode2::Form(f),
        },
        FormEvent::None => app.mode = Mode2::Form(f),
    }
}

fn choice(app: &mut App2, mut c: Choice, key: KeyEvent, fx: &mut Vec<Effect2>) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => return,
        KeyCode::Up | KeyCode::Char('k') => c.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => c.move_by(1),
        KeyCode::Home => c.selected = 0,
        KeyCode::End => c.selected = c.items.len().saturating_sub(1),
        KeyCode::Enter => {
            fx.extend(choice_effect(&c));
            return;
        }
        _ => {}
    }
    app.mode = Mode2::Choice(c);
}

fn detail(app: &mut App2, mut d: Detail, key: KeyEvent, fx: &mut Vec<Effect2>) {
    let max = d.lines.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => return,
        KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => d.scroll = (d.scroll + 1).min(max),
        KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(10),
        KeyCode::PageDown => d.scroll = (d.scroll + 10).min(max),
        KeyCode::Home | KeyCode::Char('g') => d.scroll = 0,
        KeyCode::End | KeyCode::Char('G') => d.scroll = max,
        KeyCode::Char('c' | 'y') => fx.push(Effect2::Copy {
            text: d.copy.clone(),
            what: "detail".to_owned(),
        }),
        _ => {}
    }
    app.mode = Mode2::Detail(d);
}
