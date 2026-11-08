//! Tray icon: closing the window hides it, and only the tray menu's Quit exits.

use std::sync::mpsc::{Receiver, channel};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub enum Action {
    Show,
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    actions: Receiver<Action>,
}

impl Tray {
    /// Must run on the thread that owns the window's event loop. Returns None if Windows refuses
    /// the icon, so the caller can let the window close normally instead of trapping the user.
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Show middlemic", true, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[&show, &quit]).ok()?;

        let image = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok()?;
        let icon = tray_icon::Icon::from_rgba(image.rgba, image.width, image.height).ok()?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("middlemic")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;

        let (tx, actions) = channel();
        let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let action = if e.id == show_id {
                Action::Show
            } else if e.id == quit_id {
                Action::Quit
            } else {
                return;
            };
            let _ = menu_tx.send(action);
            menu_ctx.request_repaint();
        }));
        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
            | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = e
            {
                let _ = tx.send(Action::Show);
                click_ctx.request_repaint();
            }
        }));
        Some(Self { _icon: icon, actions })
    }

    pub fn poll(&self) -> Vec<Action> {
        self.actions.try_iter().collect()
    }
}
