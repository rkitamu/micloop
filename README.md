# micloop

マイク入力をホットキーで即ループバック再生するLinux用ツール。
タスクトレイ常駐と設定GUIを持つ単一バイナリ。

- キー監視は evdev 直読み。フォーカスやX11/Waylandに依存しない (`input` グループ所属が必要)
- 音声は PulseAudio/PipeWire。`pactl` / `pw-loopback` を自動選択

## インストール

```bash
make install     # リリースビルド → ~/.local/bin/micloop → ランチャー登録
make uninstall
```

要 rustup。配置先は `PREFIX` で変更可 (既定: `~/.local`)。

## 使い方

ランチャーから Micloop を起動するとトレイに常駐する。設定はトレイメニューから:
ホットキーは録取式 (ボタンを押して実際の組み合わせを入力、修飾キー必須、Escで取り消し)。
既定は `Ctrl+ScrollLock`、モードは toggle (押すたびON/OFF) / hold (押している間だけON)。

```bash
micloop                # トレイ常駐 (引数なし)
micloop run [--mode toggle|hold] [--modifier ctrl]... [--key KEY_F9]
micloop stop
micloop settings
micloop desktop [--uninstall]
```

設定は `~/.config/micloop/config.json` に保存され、CLIとGUIで共有される。

## 開発

`cargo test` でユニットテスト、`cargo test -- --ignored` でuinput注入のE2E (要 /dev/uinput)。
機能追加は `src/settings.rs` のフォームと `src/config.rs` の `Config` に足す。
