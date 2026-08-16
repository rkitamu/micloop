//! 設定画面 (eframe/egui)。トレイから別プロセスとして起動される。
//!
//! 機能を増やすときはここにフォーム行を足し、Config (config.rs) に
//! フィールドを足す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::config::{self, Config, Mode, Modifier, Output};
use crate::listener::{capture_hotkey, parse_key};

/// バックグラウンドでホットキーの組み合わせを1つ録取する。
struct Capture {
    stop: Arc<AtomicBool>,
    rx: Receiver<(Vec<Modifier>, String)>,
}

impl Capture {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let stop_flag = stop.clone();
        std::thread::spawn(move || {
            if let Some(combo) = capture_hotkey(&stop_flag) {
                let _ = tx.send(combo);
            }
            // Escや中断ではtxがdropされ、UI側はDisconnectedで録取終了を知る
        });
        Self { stop, rx }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct SettingsApp {
    cfg: Config,
    capture: Option<Capture>,
    error: Option<&'static str>,
}

impl SettingsApp {
    fn new(cfg: Config) -> Self {
        Self {
            cfg,
            capture: None,
            error: None,
        }
    }

    fn poll_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.capture.take() else {
            return;
        };
        match capture.rx.try_recv() {
            Ok((mods, key)) => {
                if mods.is_empty() {
                    self.error = Some("修飾キーを含めて押してください (例: Ctrl+A)");
                    self.capture = Some(Capture::start());
                } else {
                    self.cfg.modifiers = mods;
                    self.cfg.key = key;
                    self.error = None;
                }
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                self.capture = Some(capture);
            }
            Err(TryRecvError::Disconnected) => {} // Escで取り消し
        }
    }

    fn validate(&self) -> Option<&'static str> {
        if self.cfg.modifiers.is_empty() {
            return Some("修飾キーを含むホットキーを登録してください");
        }
        if parse_key(&self.cfg.key).is_none() {
            return Some("ホットキーが未登録です");
        }
        None
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_capture(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("モード");
                    egui::ComboBox::from_id_salt("mode")
                        .selected_text(match self.cfg.mode {
                            Mode::Toggle => "押すたびON/OFF",
                            Mode::Hold => "押している間だけON",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cfg.mode, Mode::Toggle, "押すたびON/OFF");
                            ui.selectable_value(&mut self.cfg.mode, Mode::Hold, "押している間だけON");
                        });
                    ui.end_row();

                    ui.label("ホットキー");
                    let recording = self.capture.is_some();
                    let label = if recording {
                        "Recording... (Escで取り消し)".to_string()
                    } else {
                        self.cfg.hotkey_label()
                    };
                    if ui.button(label).clicked() {
                        if recording {
                            self.capture = None;
                        } else {
                            self.error = None;
                            self.capture = Some(Capture::start());
                        }
                    }
                    ui.end_row();

                    ui.label("出力");
                    egui::ComboBox::from_id_salt("output")
                        .selected_text(match self.cfg.output {
                            Output::Realtime => "リアルタイム再生",
                            Output::Delayed => "OFF時にまとめて再生",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.cfg.output,
                                Output::Realtime,
                                "リアルタイム再生",
                            );
                            ui.selectable_value(
                                &mut self.cfg.output,
                                Output::Delayed,
                                "OFF時にまとめて再生",
                            );
                        });
                    ui.end_row();

                    ui.label("レイテンシ");
                    ui.add_enabled(
                        self.cfg.output == Output::Realtime,
                        egui::Slider::new(&mut self.cfg.latency_msec, 1..=200).suffix(" ms"),
                    );
                    ui.end_row();

                    ui.label("最大録音時間");
                    ui.add_enabled(
                        self.cfg.output == Output::Delayed,
                        egui::Slider::new(&mut self.cfg.max_record_secs, 10..=3600).suffix(" 秒"),
                    );
                    ui.end_row();
                });

            if let Some(error) = self.error {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::RED, error);
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    self.error = self.validate();
                    if self.error.is_none() {
                        if let Err(err) = config::save(&self.cfg) {
                            eprintln!("設定の保存に失敗: {err}");
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                if ui.button("キャンセル").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}

/// eguiの既定フォントはCJKグリフを含まないため、システムの日本語フォントを足す。
fn install_jp_font(ctx: &egui::Context) {
    let Some(path) = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "sans:lang=ja"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("jp".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("jp".into());
    }
    ctx.set_fonts(fonts);
}

pub fn run() -> i32 {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_title("micloop 設定"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "micloop-settings",
        options,
        Box::new(|cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(SettingsApp::new(config::load())))
        }),
    );
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("設定画面の起動に失敗: {err}");
            1
        }
    }
}
