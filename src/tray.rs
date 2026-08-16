//! タスクトレイ常駐 (ksni / StatusNotifierItem)。
//!
//! 設定画面は別プロセス (`micloop settings`) として起動し、
//! 終了後に設定を読み直してリスナーを再起動する。

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{self, Config};
use crate::instance;
use crate::listener::Listener;
use crate::loopback::{transition, Loopback};

enum Msg {
    OpenSettings,
    SettingsClosed,
    Quit,
}

struct MicloopTray {
    status: String,
    active: bool,
    tx: Sender<Msg>,
}

impl ksni::Tray for MicloopTray {
    fn id(&self) -> String {
        "micloop".into()
    }

    fn title(&self) -> String {
        "micloop".into()
    }

    fn icon_name(&self) -> String {
        if self.active {
            "microphone-sensitivity-high".into()
        } else {
            "microphone-sensitivity-muted".into()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: format!("micloop: {}", self.status),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: self.status.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "設定...".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Msg::OpenSettings);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "終了".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(Msg::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// リスナーとループバックひと組の起動。設定変更時に作り直す。
fn start_engine(
    cfg: &Config,
    handle: &ksni::Handle<MicloopTray>,
) -> Result<(Listener, Arc<Mutex<Loopback>>), String> {
    let loopback = Arc::new(Mutex::new(Loopback::new(cfg.latency_msec)));
    let lb = loopback.clone();
    let tray_handle = handle.clone();
    let mode = cfg.mode;
    let status_base = format!("{} ({})", cfg.hotkey_label(), cfg.mode);

    let listener = Listener::spawn(&cfg.key, cfg.modifiers.clone(), move |pressed| {
        let mut lb = lb.lock().expect("loopback lock");
        if let Some(next) = transition(mode, pressed, lb.active()) {
            lb.set_active(next);
            let active = lb.active();
            let status = format!("{} — {}", if active { "ON" } else { "OFF" }, status_base);
            tray_handle.update(|tray| {
                tray.active = active;
                tray.status = status.clone();
            });
        }
    })?;
    Ok((listener, loopback))
}

fn set_status(handle: &ksni::Handle<MicloopTray>, active: bool, status: String) {
    handle.update(move |tray| {
        tray.active = active;
        tray.status = status.clone();
    });
}

pub fn run() -> i32 {
    instance::claim();

    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = std::sync::mpsc::channel();
    let service = ksni::TrayService::new(MicloopTray {
        status: "起動中...".into(),
        active: false,
        tx: tx.clone(),
    });
    let handle = service.handle();
    service.spawn();

    let term = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let _ = signal_hook::flag::register(sig, term.clone());
    }

    let mut cfg = config::load();
    let mut engine = match start_engine(&cfg, &handle) {
        Ok(engine) => {
            set_status(
                &handle,
                false,
                format!("OFF — {} ({})", cfg.hotkey_label(), cfg.mode),
            );
            Some(engine)
        }
        Err(err) => {
            eprintln!("{err}");
            set_status(&handle, false, format!("停止中: {err}"));
            None
        }
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(Msg::Quit) => break,
            Ok(Msg::OpenSettings) => {
                // キー取得中の誤発動を防ぐため、設定中はホットキーを止める
                if let Some((listener, loopback)) = engine.take() {
                    listener.stop();
                    loopback.lock().expect("loopback lock").stop();
                }
                set_status(&handle, false, "設定中 (ホットキー停止)".into());
                let tx = tx.clone();
                let exe = std::env::current_exe().expect("current_exe");
                std::thread::spawn(move || {
                    let _ = std::process::Command::new(exe).arg("settings").status();
                    let _ = tx.send(Msg::SettingsClosed);
                });
            }
            Ok(Msg::SettingsClosed) => {
                cfg = config::load();
                match start_engine(&cfg, &handle) {
                    Ok(new_engine) => {
                        set_status(
                            &handle,
                            false,
                            format!("OFF — {} ({})", cfg.hotkey_label(), cfg.mode),
                        );
                        engine = Some(new_engine);
                    }
                    Err(err) => {
                        eprintln!("{err}");
                        set_status(&handle, false, format!("停止中: {err}"));
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if term.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Some((listener, loopback)) = engine.take() {
        listener.stop();
        loopback.lock().expect("loopback lock").stop();
    }
    instance::release();
    0
}
