//! 録音履歴ウィンドウ (eframe/egui)。トレイまたはホットキーから別プロセスとして起動される。
//!
//! 履歴を時系列 (新しい順) で表示し、クリックで再生する。

use std::path::Path;
use std::process::{Child, Command};

use eframe::egui;

use crate::loopback::{kill_and_reap, record_tools, recordings, wav_secs};
use crate::settings::install_jp_font;

fn time_label(epoch_ms: u64) -> String {
    let t = (epoch_ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:02}/{:02} {:02}:{:02}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[derive(Default)]
struct HistoryApp {
    playing: Option<Child>,
}

impl HistoryApp {
    fn play(&mut self, path: &Path) {
        if let Some(child) = self.playing.take() {
            kill_and_reap(child);
        }
        let Some((_, player)) = record_tools() else {
            eprintln!("paplay も pw-play も見つかりません");
            return;
        };
        match Command::new(player).arg(path).spawn() {
            Ok(child) => self.playing = Some(child),
            Err(err) => eprintln!("再生の開始に失敗: {err}"),
        }
    }
}

impl eframe::App for HistoryApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            // 20件上限なので毎フレーム読み直しても軽い。常に最新が出る
            let recs = recordings();
            if recs.is_empty() {
                ui.label("録音はまだありません (delayed出力でONにすると録音されます)");
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (ms, path) in recs {
                    ui.horizontal(|ui| {
                        if ui.button("▶").clicked() {
                            self.play(&path);
                        }
                        let secs = wav_secs(&path)
                            .map(|s| format!("{s}秒"))
                            .unwrap_or_else(|| "?".into());
                        ui.label(format!("{}　{}", time_label(ms), secs));
                    });
                }
            });
        });
    }
}

impl Drop for HistoryApp {
    fn drop(&mut self) {
        if let Some(child) = self.playing.take() {
            kill_and_reap(child);
        }
    }
}

pub fn run() -> i32 {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([320.0, 400.0])
            // WaylandのCSDタイトルはCJKが豆腐になるためASCIIにする
            .with_title("micloop - History")
            // micloop.desktop と一致させ、dockに歯車ではなくアプリアイコンを出す
            .with_app_id("micloop"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "micloop-history",
        options,
        Box::new(|cc| {
            install_jp_font(&cc.egui_ctx);
            Ok(Box::new(HistoryApp::default()))
        }),
    );
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("履歴画面の起動に失敗: {err}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_label_formats() {
        // タイムゾーン依存を避けて桁と区切りだけ確かめる
        let label = time_label(1_755_300_000_000);
        assert_eq!(label.len(), "MM/DD HH:MM:SS".len());
        assert_eq!(&label[2..3], "/");
        assert_eq!(&label[5..6], " ");
    }
}
