//! マイク→出力ループバックの制御と、モード→状態遷移の対応付け。
//!
//! バックエンドは2系統を自動選択する:
//! - pactl: PulseAudio、または pipewire-pulse + pulseaudio-utils 構成
//! - pw-loopback: PipeWire (pactlが無い構成)。プロセスの生死がON/OFF

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::config::{Config, Mode, Output};

enum State {
    Off,
    /// pactl load-module が返したモジュールID
    PactlModule(String),
    PwProcess(Child),
    /// 稼働中の録音プロセス (timeoutでラップ済み、delayedモード) と録音先
    Recording(Child, PathBuf),
}

/// 履歴として残す録音数の上限。tmpfs (RAM) を食い潰さないため
// ponytail: 固定値。変えたい要望が出たら設定に昇格
const MAX_RECORDINGS: usize = 20;

/// (録音コマンド, 再生コマンド)。pactl系優先は既存バックエンド選択と同じ順。
pub fn record_tools() -> Option<(&'static str, &'static str)> {
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

/// 録音先ディレクトリ。XDG_RUNTIME_DIRはtmpfsなのでディスクに書かず、ログアウトで消える。
fn recordings_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 新しい録音先。ファイル名のunixミリ秒が履歴の時系列キーになる。
fn new_wav_path() -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    recordings_dir().join(format!("micloop-{ms}.wav"))
}

/// 録音履歴を (unixミリ秒, パス) で新しい順に返す。
pub fn recordings() -> Vec<(u64, PathBuf)> {
    recordings_in(&recordings_dir())
}

fn recordings_in(dir: &Path) -> Vec<(u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut list: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let ms = name.strip_prefix("micloop-")?.strip_suffix(".wav")?.parse().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    list.sort_unstable_by(|a, b| b.cmp(a));
    list
}

/// 新規録音1件分の空きを作る (古い順に削除)。
fn prune_recordings() {
    for (_, path) in recordings().into_iter().skip(MAX_RECORDINGS - 1) {
        let _ = std::fs::remove_file(path);
    }
}

/// WAVヘッダのバイトレートから長さ(秒)を概算する。
/// parecord/pw-record が書く標準ヘッダ (fmtチャンクがオフセット12固定) 前提。
pub fn wav_secs(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 32];
    file.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let byte_rate = u32::from_le_bytes(head[28..32].try_into().unwrap()) as u64;
    let data_len = file.metadata().ok()?.len().saturating_sub(44);
    (byte_rate > 0).then_some(data_len / byte_rate)
}

pub fn kill_and_reap(mut child: Child) {
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
        prune_recordings();
        let path = new_wav_path();
        // timeoutで録音時間に上限を張り、tmpfsを食い潰さないようにする。
        // 時間切れでもSIGTERMなのでWAVヘッダは正常に閉じられる
        let child = Command::new("timeout")
            .arg(self.max_record_secs.to_string())
            .arg(recorder)
            .arg(&path)
            .spawn()?;
        Ok(State::Recording(child, path))
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
            State::Recording(mut child, path) => {
                // Child::kill()はSIGKILLでWAVヘッダが壊れるため、SIGTERMで閉じさせる。
                // timeoutはSIGTERMを録音プロセスへ転送してから終了する
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
                if let Some((_, player)) = record_tools() {
                    match Command::new(player).arg(&path).spawn() {
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

// 録音ファイルは履歴として意図的に残す (tmpfsなのでログアウトで消える)
impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop();
        if let Some(child) = self.playing.take() {
            kill_and_reap(child);
        }
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
    fn recordings_sorted_newest_first_ignoring_others() {
        let dir = std::env::temp_dir().join("micloop-test-recordings");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["micloop-200.wav", "micloop-100.wav", "other.wav", "micloop-x.wav"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let ms: Vec<u64> = recordings_in(&dir).into_iter().map(|(ms, _)| ms).collect();
        assert_eq!(ms, vec![200, 100]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wav_secs_reads_byte_rate() {
        let path = std::env::temp_dir().join("micloop-test.wav");
        let mut head = Vec::new();
        head.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0");
        head.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        head.extend_from_slice(&176400u32.to_le_bytes()); // byte rate (offset 28)
        head.resize(44, 0);
        head.resize(44 + 176400 * 3, 0); // 3秒ぶんのデータ
        std::fs::write(&path, &head).unwrap();
        assert_eq!(wav_secs(&path), Some(3));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn toggle_flips_on_press_only() {
        assert_eq!(transition(Mode::Toggle, true, false), Some(true));
        assert_eq!(transition(Mode::Toggle, false, true), None);
        assert_eq!(transition(Mode::Toggle, true, true), Some(false));
    }
}
