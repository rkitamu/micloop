//! evdevによるグローバルホットキー監視。
//!
//! /dev/input を直接読むため、ウィンドウフォーカスやX11/Waylandに依存しない。
//! 読み取りには input グループ所属が必要。

use std::collections::HashSet;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use evdev::{Device, InputEventKind, Key};

use crate::config::Modifier;

/// evdevキー名 (例: "KEY_F9") をKeyに変換する。
pub fn parse_key(name: &str) -> Option<Key> {
    // Keyの一覧APIがないため、既知コード範囲をDebug表記で総当たりする
    (0..0x2ff).map(Key::new).find(|k| format!("{k:?}") == name)
}

fn modifier_keys(m: Modifier) -> [Key; 2] {
    match m {
        Modifier::Ctrl => [Key::KEY_LEFTCTRL, Key::KEY_RIGHTCTRL],
        Modifier::Shift => [Key::KEY_LEFTSHIFT, Key::KEY_RIGHTSHIFT],
        Modifier::Alt => [Key::KEY_LEFTALT, Key::KEY_RIGHTALT],
        Modifier::Super => [Key::KEY_LEFTMETA, Key::KEY_RIGHTMETA],
    }
}

pub const ALL_MODIFIER_KEYS: [Key; 8] = [
    Key::KEY_LEFTCTRL,
    Key::KEY_RIGHTCTRL,
    Key::KEY_LEFTSHIFT,
    Key::KEY_RIGHTSHIFT,
    Key::KEY_LEFTALT,
    Key::KEY_RIGHTALT,
    Key::KEY_LEFTMETA,
    Key::KEY_RIGHTMETA,
];

/// 要求された修飾キーが全て押されているか (左右どちらでも可)。
fn mods_satisfied(required: &[Modifier], pressed: &HashSet<Key>) -> bool {
    required
        .iter()
        .all(|m| modifier_keys(*m).iter().any(|k| pressed.contains(k)))
}

fn poll_readable(pollfds: &mut [libc::pollfd], timeout_ms: i32) -> bool {
    let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout_ms) };
    n > 0
}

fn pollfds_for(devices: &[Device]) -> Vec<libc::pollfd> {
    devices
        .iter()
        .map(|dev| libc::pollfd {
            fd: dev.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect()
}

/// ホットキー (修飾キー+メインキー) を監視するスレッド。
///
/// コールバックは監視スレッド上で呼ばれる。キーリピート(value==2)は無視。
/// 押下は修飾キーが揃っているときのみ通知、解放は常に通知する
/// (holdモードで修飾キーを先に離してもOFFにできるように)。
pub struct Listener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    pub fn spawn(
        key_name: &str,
        modifiers: Vec<Modifier>,
        mut on_key: impl FnMut(bool) + Send + 'static,
    ) -> Result<Self, String> {
        let key = parse_key(key_name).ok_or_else(|| format!("不明なキー名: {key_name}"))?;
        let mod_keys: Vec<Key> = modifiers
            .iter()
            .flat_map(|m| modifier_keys(*m))
            .collect();
        // メインキーを持つデバイスに加え、修飾キーだけのデバイスも監視する
        // (メインキーがマクロパッド側にある構成のため)
        let devices: Vec<Device> = evdev::enumerate()
            .map(|(_, dev)| dev)
            .filter(|dev| {
                dev.supported_keys().is_some_and(|keys| {
                    keys.contains(key) || mod_keys.iter().any(|k| keys.contains(*k))
                })
            })
            .collect();
        if devices.is_empty() {
            return Err(format!(
                "{key_name} を持つ入力デバイスが見つかりません (inputグループに所属していますか?)"
            ));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("micloop-listener".into())
            .spawn(move || watch(devices, key, &modifiers, &stop_flag, &mut on_key))
            .map_err(|err| err.to_string())?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// 監視を止めてスレッドの終了を待つ。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(
    mut devices: Vec<Device>,
    key: Key,
    required: &[Modifier],
    stop: &AtomicBool,
    on_key: &mut impl FnMut(bool),
) {
    let mut pollfds = pollfds_for(&devices);
    let mut pressed: HashSet<Key> = HashSet::new();

    while !stop.load(Ordering::Relaxed) {
        // 停止フラグを見るため200msでタイムアウトさせる
        if !poll_readable(&mut pollfds, 200) {
            continue;
        }
        for i in 0..pollfds.len() {
            if pollfds[i].revents & libc::POLLIN == 0 {
                continue;
            }
            let Ok(events) = devices[i].fetch_events() else {
                continue;
            };
            for event in events {
                let InputEventKind::Key(ev_key) = event.kind() else {
                    continue;
                };
                if ev_key == key {
                    match event.value() {
                        1 if mods_satisfied(required, &pressed) => on_key(true),
                        0 => on_key(false),
                        _ => {} // 修飾キー不足の押下、キーリピート
                    }
                } else if ALL_MODIFIER_KEYS.contains(&ev_key) {
                    match event.value() {
                        1 => {
                            pressed.insert(ev_key);
                        }
                        0 => {
                            pressed.remove(&ev_key);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// ホットキーの組み合わせを1つ録取する (設定画面用)。
/// 非修飾キーが押された時点の修飾キー状態と合わせて返す。
/// Escは取り消し、stopが立ったら中断で、いずれもNone。
pub fn capture_hotkey(stop: &AtomicBool) -> Option<(Vec<Modifier>, String)> {
    let mut devices: Vec<Device> = evdev::enumerate()
        .map(|(_, dev)| dev)
        .filter(|dev| dev.supported_keys().is_some())
        .collect();
    let mut pollfds = pollfds_for(&devices);
    let mut pressed: HashSet<Key> = HashSet::new();

    while !stop.load(Ordering::Relaxed) {
        if !poll_readable(&mut pollfds, 100) {
            continue;
        }
        for i in 0..pollfds.len() {
            if pollfds[i].revents & libc::POLLIN == 0 {
                continue;
            }
            let Ok(events) = devices[i].fetch_events() else {
                continue;
            };
            for event in events {
                let InputEventKind::Key(ev_key) = event.kind() else {
                    continue;
                };
                if ALL_MODIFIER_KEYS.contains(&ev_key) {
                    match event.value() {
                        1 => {
                            pressed.insert(ev_key);
                        }
                        0 => {
                            pressed.remove(&ev_key);
                        }
                        _ => {}
                    }
                    continue;
                }
                if event.value() != 1 {
                    continue;
                }
                if ev_key == Key::KEY_ESC {
                    return None;
                }
                let name = format!("{ev_key:?}");
                // BTN_* (マウスボタン等) は対象外
                if !name.starts_with("KEY_") {
                    continue;
                }
                let mods = crate::config::ALL_MODIFIERS
                    .into_iter()
                    .filter(|m| modifier_keys(*m).iter().any(|k| pressed.contains(k)))
                    .collect();
                return Some((mods, name));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_resolves_known_names() {
        assert_eq!(parse_key("KEY_A"), Some(Key::KEY_A));
        assert_eq!(parse_key("KEY_SCROLLLOCK"), Some(Key::KEY_SCROLLLOCK));
        assert_eq!(parse_key("KEY_BOGUS"), None);
    }

    #[test]
    fn mods_satisfied_accepts_either_side() {
        let required = vec![Modifier::Ctrl, Modifier::Super];
        let pressed: HashSet<Key> = [Key::KEY_RIGHTCTRL, Key::KEY_LEFTMETA].into();
        assert!(mods_satisfied(&required, &pressed));
    }

    #[test]
    fn mods_satisfied_rejects_missing_modifier() {
        let required = vec![Modifier::Ctrl, Modifier::Alt];
        let pressed: HashSet<Key> = [Key::KEY_LEFTCTRL].into();
        assert!(!mods_satisfied(&required, &pressed));
    }

    #[test]
    fn empty_requirement_is_always_satisfied() {
        assert!(mods_satisfied(&[], &HashSet::new()));
    }

    /// uinputの仮想キーボードでAlt+Kを注入するE2Eテスト。
    /// 要 /dev/uinput への書き込み権限: cargo test -- --ignored
    #[test]
    #[ignore = "requires /dev/uinput access"]
    fn end_to_end_alt_k() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        use evdev::uinput::VirtualDeviceBuilder;
        use evdev::{AttributeSet, EventType, InputEvent};

        let mut keys = AttributeSet::<Key>::new();
        keys.insert(Key::KEY_K);
        keys.insert(Key::KEY_LEFTALT);
        let mut vdev = VirtualDeviceBuilder::new()
            .unwrap()
            .name("micloop-test-kbd")
            .with_keys(&keys)
            .unwrap()
            .build()
            .unwrap();
        std::thread::sleep(Duration::from_millis(500)); // udev settle待ち

        let presses = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let (p, r) = (presses.clone(), releases.clone());
        let listener = Listener::spawn("KEY_K", vec![Modifier::Alt], move |pressed| {
            if pressed {
                p.fetch_add(1, Ordering::Relaxed);
            } else {
                r.fetch_add(1, Ordering::Relaxed);
            }
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let key_event = |k: Key, v: i32| InputEvent::new(EventType::KEY, k.code(), v);
        // Alt+K → 発動するはず
        vdev.emit(&[key_event(Key::KEY_LEFTALT, 1)]).unwrap();
        vdev.emit(&[key_event(Key::KEY_K, 1)]).unwrap();
        vdev.emit(&[key_event(Key::KEY_K, 0)]).unwrap();
        vdev.emit(&[key_event(Key::KEY_LEFTALT, 0)]).unwrap();
        // K単独 → 押下は発動しないはず (解放は通知される)
        vdev.emit(&[key_event(Key::KEY_K, 1)]).unwrap();
        vdev.emit(&[key_event(Key::KEY_K, 0)]).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        listener.stop();

        assert_eq!(presses.load(Ordering::Relaxed), 1, "Alt+K press");
        assert_eq!(releases.load(Ordering::Relaxed), 2, "K releases");
    }
}
