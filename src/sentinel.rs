use crate::adb::Adb;
use crate::app;
use crate::response;
use crate::system;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::time;

#[derive(Clone, Debug)]
pub struct WatchConfig {
    pub package_name: String,
    pub service_name: Option<String>,
    pub overlay: bool,
    pub permissions: Vec<String>,
    pub keep_awake: bool,
    pub pin: Option<String>,
}

static WATCH_LIST: LazyLock<Mutex<HashMap<String, WatchConfig>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn add_watch(
    package: String,
    service: Option<String>,
    overlay: bool,
    permissions: Vec<String>,
    keep_awake: bool,
    pin: Option<String>,
) -> Result<Value, Value> {
    let config = WatchConfig {
        package_name: package.clone(),
        service_name: service,
        overlay,
        permissions,
        keep_awake,
        pin,
    };

    match WATCH_LIST.lock() {
        Ok(mut list) => {
            list.insert(package.clone(), config);
            response::bounded_text_response(
                format!("Sentinel watching {package}"),
                response::DEFAULT_TEXT_BUDGET_BYTES,
                response::TruncationStrategy::Head,
            )
        }
        Err(_) => Err(json!({"code": -32603, "message": "Sentinel lock poisoned"})),
    }
}

pub fn remove_watch(package: String) -> Result<Value, Value> {
    match WATCH_LIST.lock() {
        Ok(mut list) => {
            list.remove(&package);
            response::bounded_text_response(
                format!("Sentinel stopped watching {package}"),
                response::DEFAULT_TEXT_BUDGET_BYTES,
                response::TruncationStrategy::Head,
            )
        }
        Err(_) => Err(json!({"code": -32603, "message": "Sentinel lock poisoned"})),
    }
}

pub fn list_watches() -> Result<Value, Value> {
    match WATCH_LIST.lock() {
        Ok(list) => {
            let keys: Vec<String> = list.keys().cloned().collect();
            match serde_json::to_string(&keys) {
                Ok(s) => response::bounded_text_response(
                    s,
                    response::DEFAULT_TEXT_BUDGET_BYTES,
                    response::TruncationStrategy::Head,
                ),
                Err(e) => {
                    Err(json!({"code": -32603, "message": format!("Serialization failed: {}", e)}))
                }
            }
        }
        Err(_) => Err(json!({"code": -32603, "message": "Sentinel lock poisoned"})),
    }
}

pub async fn start_loop() {
    let mut interval = time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;

        let watches: Vec<WatchConfig> = {
            if let Ok(lock) = WATCH_LIST.lock() {
                lock.values().cloned().collect()
            } else {
                tracing::error!("Sentinel loop lock poisoned");
                break;
            }
        };

        if watches.is_empty() {
            continue;
        }

        for config in watches {
            let _device_guard = crate::tools::DEVICE_OPERATION_LOCK.lock().await;
            if config.keep_awake && !system::is_screen_on().await {
                if let Err(error) = system::unlock_device(config.pin.clone()).await {
                    tracing::warn!(
                        "Sentinel failed to wake/unlock {}: {}",
                        config.package_name,
                        error
                    );
                }
            }

            if !is_app_installed(&config.package_name).await {
                continue;
            }

            if let Some(svc) = &config.service_name {
                if let Err(error) = system::set_accessibility(svc, true).await {
                    tracing::warn!(
                        "Sentinel accessibility enforcement failed for {}: {}",
                        config.package_name,
                        error
                    );
                }
            }

            if config.overlay {
                if let Err(error) = system::set_overlay(&config.package_name, true).await {
                    tracing::warn!(
                        "Sentinel overlay enforcement failed for {}: {}",
                        config.package_name,
                        error
                    );
                }
            }

            for perm in &config.permissions {
                if let Err(error) = app::set_permission(&config.package_name, perm, true).await {
                    tracing::warn!(
                        "Sentinel permission enforcement failed for {}: {}",
                        config.package_name,
                        error
                    );
                }
            }
        }
    }
}

async fn is_app_installed(pkg: &str) -> bool {
    match Adb::shell(&["shell", "pm", "list", "packages", pkg]).await {
        Ok(out) => out.contains(pkg),
        Err(_) => false,
    }
}
