//! pidfileによる単一インスタンス管理。

use std::path::PathBuf;
use std::time::Duration;

pub fn pidfile() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("micloop.pid")
}

/// 稼働中のインスタンスをSIGTERMで止め、終了を待つ。
pub fn stop_running(verbose: bool) {
    let path = pidfile();
    let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        if verbose {
            println!("micloop は稼働していません");
        }
        return;
    };
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let _ = std::fs::remove_file(&path);
        if verbose {
            println!("micloop は稼働していません (残っていたpidfileを削除)");
        }
        return;
    }
    for _ in 0..20 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if verbose {
        println!("micloop (pid {pid}) を停止しました");
    }
}

/// 既存インスタンスを止めてから自分のpidを記録する。
pub fn claim() {
    stop_running(false);
    let _ = std::fs::write(pidfile(), std::process::id().to_string());
}

pub fn release() {
    let _ = std::fs::remove_file(pidfile());
}
