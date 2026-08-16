//! マイク→出力ループバックの制御と、モード→状態遷移の対応付け。
//!
//! バックエンドは2系統を自動選択する:
//! - pactl: PulseAudio、または pipewire-pulse + pulseaudio-utils 構成
//! - pw-loopback: PipeWire (pactlが無い構成)。プロセスの生死がON/OFF

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Child, Command};

use crate::config::{Config, Mode, Output};

enum State {
    Off,
    /// pactl load-module が返したモジュールID
    PactlModule(String),
    /// 稼働中の pw-loopback プロセス
    PwProcess(Child),
    /// 稼働中の録音プロセス (timeoutでラップ済み、delayedモード)
    Recording(Child),
}

/// (録音コマンド, 再生コマンド)。pactl系優先は既存バックエンド選択と同じ順。
fn record_tools() -> Option<(&'static str, &'static str)> {
    let in_path = |bin: &str| {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
        })
    };
    if in_path("parecord") {
        Some(("parecord", "paplay"))
    } else if in_path("pw-record") {
        Some(("pw-record", "pw-play"))
    } else {
        None
    }
}

/// 録音先。XDG_RUNTIME_DIRはtmpfsなのでディスクに書かず、ログアウトで消える。
fn wav_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("micloop.wav")
}

fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub struct Loopback {
    latency_msec: u32,
    output: Output,
    max_record_secs: u32,
    state: State,
    /// delayedモードで再生中のプロセス。再ON時と終了時に片付ける
    playing: Option<Child>,
}

impl Loopback {
    pub fn new(cfg: &Config) -> Self {
        Self {
            latency_msec: cfg.latency_msec,
            output: cfg.output,
            max_record_secs: cfg.max_record_secs,
            state: State::Off,
            playing: None,
        }
    }

    pub fn active(&self) -> bool {
        !matches!(self.state, State::Off)
    }

    pub fn start(&mut self) {
        if self.active() {
            return;
        }
        if let Some(child) = self.playing.take() {
            kill_and_reap(child);
        }
        if self.output == Output::Delayed {
            match self.start_record() {
                Ok(state) => self.state = state,
                Err(err) if err.kind() == ErrorKind::NotFound => eprintln!(
                    "parecord も pw-record も見つかりません。\
                     pulseaudio-utils か pipewire を導入してください"
                ),
                Err(err) => eprintln!("録音の開始に失敗: {err}"),
            }
            return;
        }
        match self.start_pactl() {
            Ok(state) => self.state = state,
            Err(err) if err.kind() == ErrorKind::NotFound => match self.start_pw_loopback() {
                Ok(state) => self.state = state,
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    eprintln!(
                        "pactl も pw-loopback も見つかりません。\
                         pulseaudio-utils か pipewire を導入してください"
                    );
                }
                Err(err) => eprintln!("pw-loopback の起動に失敗: {err}"),
            },
            Err(err) => eprintln!("pactl の実行に失敗: {err}"),
        }
    }

    fn start_pactl(&self) -> std::io::Result<State> {
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-loopback",
                &format!("latency_msec={}", self.latency_msec),
            ])
            .output()?;
        if out.status.success() {
            Ok(State::PactlModule(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ))
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }

    fn start_record(&self) -> std::io::Result<State> {
        let (recorder, _) = record_tools().ok_or(ErrorKind::NotFound)?;
        // timeoutで録音時間に上限を張り、tmpfsを食い潰さないようにする。
        // 時間切れでもSIGTERMなのでWAVヘッダは正常に閉じられる
        let child = Command::new("timeout")
            .arg(self.max_record_secs.to_string())
            .arg(recorder)
            .arg(wav_path())
            .spawn()?;
        Ok(State::Recording(child))
    }

    fn start_pw_loopback(&self) -> std::io::Result<State> {
        // ponytail: 起動直後のPipeWire接続失敗は検知しない。必要になったらtry_waitで監視
        let child = Command::new("pw-loopback")
            .args(["-n", "micloop", "-l", &self.latency_msec.to_string()])
            .spawn()?;
        Ok(State::PwProcess(child))
    }

    pub fn stop(&mut self) {
        match std::mem::replace(&mut self.state, State::Off) {
            State::Off => {}
            State::PactlModule(id) => {
                let _ = Command::new("pactl").args(["unload-module", &id]).status();
            }
            State::PwProcess(mut child) => {
                let _ = child.kill();
                let _ = child.wait(); // ゾンビ回収
            }
            State::Recording(mut child) => {
                // Child::kill()はSIGKILLでWAVヘッダが壊れるため、SIGTERMで閉じさせる。
                // timeoutはSIGTERMを録音プロセスへ転送してから終了する
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
                if let Some((_, player)) = record_tools() {
                    match Command::new(player).arg(wav_path()).spawn() {
                        Ok(child) => self.playing = Some(child),
                        Err(err) => eprintln!("再生の開始に失敗: {err}"),
                    }
                }
            }
        }
    }

    pub fn set_active(&mut self, active: bool) {
        if active {
            self.start();
        } else {
            self.stop();
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop();
        if let Some(child) = self.playing.take() {
            kill_and_reap(child);
        }
        let _ = std::fs::remove_file(wav_path());
    }
}

/// キーイベントから次のループバック状態を決める。Noneは「変化なし」。
pub fn transition(mode: Mode, pressed: bool, active: bool) -> Option<bool> {
    match (mode, pressed) {
        (Mode::Hold, pressed) => Some(pressed),
        (Mode::Toggle, true) => Some(!active),
        (Mode::Toggle, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_follows_key_state() {
        assert_eq!(transition(Mode::Hold, true, false), Some(true));
        assert_eq!(transition(Mode::Hold, false, true), Some(false));
    }

    #[test]
    fn toggle_flips_on_press_only() {
        assert_eq!(transition(Mode::Toggle, true, false), Some(true));
        assert_eq!(transition(Mode::Toggle, false, true), None);
        assert_eq!(transition(Mode::Toggle, true, true), Some(false));
    }
}
