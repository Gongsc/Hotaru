//! Window geometry is kept separate from preferences so saving a settings form
//! cannot overwrite a window move that happened while the form was open.
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, Window};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Geometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Default)]
pub struct GeometryStore(Mutex<BTreeMap<String, Geometry>>);

fn path(app: &AppHandle) -> Option<std::path::PathBuf> {
    Some(app.path().app_config_dir().ok()?.join("windows.json"))
}

pub fn init(app: &AppHandle) -> GeometryStore {
    let values = path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    GeometryStore(Mutex::new(values))
}

pub fn remember(window: &Window) {
    if !matches!(window.label(), "main" | "settings" | "chart")
        || !window.is_visible().unwrap_or(false)
        || window.is_minimized().unwrap_or(true)
        || window.is_maximized().unwrap_or(true)
    {
        return;
    }
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    if size.width == 0 || size.height == 0 {
        return;
    }
    let app = window.app_handle();
    let store = app.state::<GeometryStore>();
    let mut values = store.0.lock();
    values.insert(
        window.label().to_owned(),
        Geometry {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
        },
    );
    if let Some(path) = path(app) {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, serde_json::to_vec(&*values)?)?;
            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("保存窗口位置失败: {error}");
        }
    }
}

pub fn restore(window: &WebviewWindow) -> bool {
    let saved = window
        .app_handle()
        .state::<GeometryStore>()
        .0
        .lock()
        .get(window.label())
        .copied();
    let Some(mut g) = saved else { return false };
    if g.width == 0 || g.height == 0 {
        return false;
    }
    // The chart restores width and position; content determines its height.
    if window.label() == "chart" {
        if let Ok(size) = window.inner_size() {
            g.height = size.height;
        }
    }
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    let monitor = monitors
        .iter()
        .find(|m| {
            let a = m.work_area();
            g.x >= a.position.x
                && g.y >= a.position.y
                && (g.x as i64) < a.position.x as i64 + a.size.width as i64
                && (g.y as i64) < a.position.y as i64 + a.size.height as i64
        })
        .or_else(|| monitors.first());
    let Some(monitor) = monitor else { return false };
    let area = monitor.work_area();
    g.width = g.width.min(area.size.width);
    g.height = g.height.min(area.size.height);
    g.x = g.x.clamp(
        area.position.x,
        area.position
            .x
            .saturating_add((area.size.width - g.width) as i32),
    );
    g.y = g.y.clamp(
        area.position.y,
        area.position
            .y
            .saturating_add((area.size.height - g.height) as i32),
    );
    let _ = window.set_size(PhysicalSize::new(g.width, g.height));
    let _ = window.set_position(PhysicalPosition::new(g.x, g.y));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_round_trip_preserves_negative_monitor_coordinates() {
        let g = Geometry {
            x: -1200,
            y: 40,
            width: 800,
            height: 600,
        };
        let saved = serde_json::to_string(&g).unwrap();
        let restored: Geometry = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            (restored.x, restored.y, restored.width, restored.height),
            (-1200, 40, 800, 600)
        );
    }

    #[test]
    fn existing_preferences_keep_auto_hide_enabled() {
        let settings: crate::models::Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.chart_keep_open);
    }
}
