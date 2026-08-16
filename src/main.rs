//! micloop: マイク入力をホットキーでループバック再生する。

mod config;
mod instance;
mod listener;
mod loopback;
mod settings;
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand};

use config::Mode;
use listener::Listener;
use loopback::{transition, Loopback};

#[derive(Parser)]
#[command(name = "micloop", about = "マイク入力をホットキーでループバック再生する")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// タスクトレイに常駐する (引数なしと同じ)
    Tray,
    /// ヘッドレスで前面実行する
    Run {
        /// 設定ファイルのモードを上書き
        #[arg(long)]
        mode: Option<Mode>,
        /// 設定ファイルのキーを上書き (例: KEY_F9)
        #[arg(long)]
        key: Option<String>,
        /// 設定ファイルの修飾キーを上書き (複数指定可)
        #[arg(long = "modifier")]
        modifiers: Vec<config::Modifier>,
        /// 設定ファイルの出力方式を上書き (realtime / delayed)
        #[arg(long)]
        output: Option<config::Output>,
    },
    /// 稼働中のインスタンスを停止する
    Stop,
    /// 設定画面を開く (トレイからも起動される)
    Settings,
    /// ランチャー用 .desktop を生成する
    Desktop {
        /// .desktop を削除する
        #[arg(long)]
        uninstall: bool,
    },
}

fn run_headless(
    mode: Option<Mode>,
    key: Option<String>,
    modifiers: Vec<config::Modifier>,
    output: Option<config::Output>,
) -> i32 {
    let mut cfg = config::load();
    if let Some(mode) = mode {
        cfg.mode = mode;
    }
    if let Some(key) = key {
        cfg.key = key;
    }
    if !modifiers.is_empty() {
        cfg.modifiers = modifiers;
    }
    if let Some(output) = output {
        cfg.output = output;
    }

    let loopback = Arc::new(Mutex::new(Loopback::new(&cfg)));
    let lb = loopback.clone();
    let mode = cfg.mode;
    let listener = match Listener::spawn(&cfg.key, cfg.modifiers.clone(), move |pressed| {
        let mut lb = lb.lock().expect("loopback lock");
        if let Some(next) = transition(mode, pressed, lb.active()) {
            lb.set_active(next);
        }
    }) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };

    instance::claim();
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let _ = signal_hook::flag::register(sig, term.clone());
    }

    println!("{} を監視中 ({} モード)", cfg.hotkey_label(), cfg.mode);
    while !term.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(300));
    }

    listener.stop();
    loopback.lock().expect("loopback lock").stop();
    instance::release();
    0
}

fn desktop(uninstall: bool) -> i32 {
    let apps = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::home_dir().join(".local/share"))
        .join("applications");
    let path = apps.join("micloop.desktop");

    if uninstall {
        let _ = std::fs::remove_file(&path);
        println!("削除しました: {}", path.display());
        return 0;
    }

    let exe = std::env::current_exe().expect("current_exe");
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Micloop\n\
         Comment=マイクループバック (ホットキー制御)\n\
         Exec={}\n\
         Icon=audio-input-microphone\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;\n\
         Keywords=micloop;loopback;mic;\n",
        exe.display()
    );
    if let Err(err) = std::fs::create_dir_all(&apps).and_then(|()| std::fs::write(&path, entry)) {
        eprintln!(".desktop の生成に失敗: {err}");
        return 1;
    }
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&apps)
        .output();
    println!("生成しました: {}", path.display());
    0
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        None | Some(Command::Tray) => tray::run(),
        Some(Command::Run {
            mode,
            key,
            modifiers,
            output,
        }) => run_headless(mode, key, modifiers, output),
        Some(Command::Stop) => {
            instance::stop_running(true);
            0
        }
        Some(Command::Settings) => settings::run(),
        Some(Command::Desktop { uninstall }) => desktop(uninstall),
    };
    std::process::ExitCode::from(code as u8)
}
