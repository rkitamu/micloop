//! マイク→出力ループバックの制御と、モード→状態遷移の対応付け。
//!
//! バックエンドは2系統を自動選択する:
//! - pactl: PulseAudio、または pipewire-pulse + pulseaudio-utils 構成
//! - pw-loopback: PipeWire (pactlが無い構成)。プロセスの生死がON/OFF

use std::io::ErrorKind;
use std::process::{Child, Command};

use crate::config::Mode;

enum State {
    Off,
    /// pactl load-module が返したモジュールID
    PactlModule(String),
    /// 稼働中の pw-loopback プロセス
    PwProcess(Child),
}

pub struct Loopback {
    latency_msec: u32,
    state: State,
}

impl Loopback {
    pub fn new(latency_msec: u32) -> Self {
        Self {
            latency_msec,
            state: State::Off,
        }
    }

    pub fn active(&self) -> bool {
        !matches!(self.state, State::Off)
    }

    pub fn start(&mut self) {
        if self.active() {
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
