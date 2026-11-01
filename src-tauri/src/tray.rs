use crate::db::get_settings;
use crate::i18n::TrayI18n;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn setup_tray(app: &AppHandle, lang: &str) -> Result<(), Box<dyn std::error::Error>> {
    let i18n = TrayI18n::new(lang);

    let quit_i = MenuItem::with_id(app, "quit", i18n.get("quit"), true, None::<&str>)?;
    let settings_i = MenuItem::with_id(app, "settings", i18n.get("settings"), true, None::<&str>)?;
    let clean_i = MenuItem::with_id(app, "clean", i18n.get("clean_now"), true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&clean_i, &settings_i, &separator, &quit_i])?;

    let mut builder = TrayIconBuilder::with_id("tray")
        .tooltip(i18n.get("tooltip"))
        .show_menu_on_left_click(false)
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "quit" => {
                app.exit(0);
            }
            "settings" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { show_settings_window(&app); });
            }
            "clean" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { crate::commands::show_review_cmd(app, Vec::new()).await; });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                ..
            } = event
            {
                if button == MouseButton::Left && button_state == tauri::tray::MouseButtonState::Up
                {
                    let app = tray.app_handle().clone();
                    tauri::async_runtime::spawn(async move { show_popup_window(&app); });
                }
            }
        });

    #[cfg(target_os = "macos")]
    {
        let mut pixels = vec![0u8; 22 * 22 * 4];
        for y in 0..22i32 {
            for x in 0..22i32 {
                let body = (x - 11) * (x - 11) + (y - 12) * (y - 12) <= 49;
                let ears = (x - 5) * (x - 5) + (y - 5) * (y - 5) <= 12
                    || (x - 17) * (x - 17) + (y - 5) * (y - 5) <= 12;
                if body || ears {
                    pixels[((y * 22 + x) * 4 + 3) as usize] = 255;
                }
            }
        }
        builder = builder
            .icon(tauri::image::Image::new_owned(pixels, 22, 22))
            .icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let _tray = builder.build(app)?;

    Ok(())
}

fn tray_lang(_app: &AppHandle) -> String {
    get_settings()
        .map(|s| s.language)
        .unwrap_or_else(|_| "en".to_string())
}

pub fn show_popup_window(app: &AppHandle) {
    let i18n = TrayI18n::new(&tray_lang(app));
    if let Some(window) = app.get_webview_window("popup") {
        #[cfg(target_os = "macos")]
        position_popup(app, &window);
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        #[cfg(target_os = "macos")]
        let window = tauri::WebviewWindowBuilder::new(
            app,
            "popup",
            tauri::WebviewUrl::App("/#/popup".into()),
        )
        .title(i18n.get("popup_title"))
        .inner_size(300.0, 420.0)
        .decorations(false)
        .always_on_top(true)
        .shadow(false)
        .build();

        #[cfg(not(target_os = "macos"))]
        let window = tauri::WebviewWindowBuilder::new(
            app,
            "popup",
            tauri::WebviewUrl::App("/#/popup".into()),
        )
        .title(i18n.get("popup_title"))
        .inner_size(300.0, 420.0)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .build();

        if let Ok(win) = window {
            #[cfg(target_os = "macos")]
            {
                position_popup(app, &win);
                let popup = win.clone();
                win.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Focused(false)) {
                        let _ = popup.hide();
                    }
                });
            }
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

#[cfg(target_os = "macos")]
fn position_popup(app: &AppHandle, window: &tauri::WebviewWindow) {
    if let Some(tray) = app.tray_by_id("tray") {
        if let Ok(Some(rect)) = tray.rect() {
            let scale = window.scale_factor().unwrap_or(1.0);
            let physical = rect.position.to_physical::<f64>(scale);
            let monitor = window
                .monitor_from_point(physical.x, physical.y)
                .ok()
                .flatten();
            let scale = monitor.as_ref().map(|m| m.scale_factor()).unwrap_or(scale);
            let position = rect.position.to_logical::<f64>(scale);
            let size = rect.size.to_logical::<f64>(scale);
            let mut x = position.x + size.width / 2.0 - 150.0;
            if let Some(monitor) = monitor {
                let left = monitor.position().to_logical::<f64>(scale).x;
                let width = monitor.size().to_logical::<f64>(scale).width;
                x = x.clamp(left + 4.0, (left + width - 304.0).max(left + 4.0));
            }
            let _ = window.set_position(tauri::LogicalPosition::new(
                x,
                position.y + size.height + 4.0,
            ));
        }
    }
}

pub fn update_tray_tooltip(app: &AppHandle, count: usize) {
    let i18n = TrayI18n::new(&tray_lang(app));
    let tooltip = if count == 0 {
        i18n.get("tooltip").to_string()
    } else if count == 1 {
        i18n.get("tooltip_one_pending").replace("{}", "1")
    } else {
        i18n.get("tooltip_many_pending")
            .replace("{}", &count.to_string())
    };
    if let Some(tray) = app.tray_by_id("tray") {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}

pub fn show_settings_window(app: &AppHandle) {
    let i18n = TrayI18n::new(&tray_lang(app));
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        let window = tauri::WebviewWindowBuilder::new(
            app,
            "settings",
            tauri::WebviewUrl::App("/#/settings".into()),
        )
        .title(i18n.get("settings_title"))
        .inner_size(900.0, 650.0)
        .min_inner_size(700.0, 500.0)
        .build();

        if let Ok(win) = window {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}
