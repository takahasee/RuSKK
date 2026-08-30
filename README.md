# skk-proxy

macSKK 向けの skkserv プロキシです。  
[azoo-key-skkserv](https://github.com/gitusp/azoo-key-skkserv) を優先し、見つからない場合のみ [yaskkserv2](https://github.com/wachikun/yaskkserv2) にフォールバックします。

```
macSKK  →  skk-proxy(:1178)  →  azoo-key-skkserv(:1180)   # primary
                             →  yaskkserv2(:1179)         # fallback
```

## 特徴

- 順次フォールバック（azookey → yaskkserv2）— opcode `1`（変換）
- opcode `4`（補完）は両バックエンドを並列照会し、候補をマージ・重複除去
- 補完候補は過去の選択頻度で並べ替え（`~/.skk-proxy-frequency.json`）
- クライアントへの応答は **UTF-8**
- azoo-key-skkserv の UTF-8 応答はそのまま転送
- yaskkserv2 の EUC-JP 応答は UTF-8 に変換
- skkserv プロトコル `0` / `1` / `2` / `3` / `4` に対応

## ビルド

```sh
cargo build --release
```

## 起動例

各 skkserv を先に起動したうえで:

```sh
# azoo-key-skkserv（例）
azoo-key-skkserv --port 1180 --incoming-charset EUC-JP

# yaskkserv2（例）
yaskkserv2 --port 1179 --google-suggest ~/Documents/SKK/dictionary.yaskkserv2

# プロキシ
./target/release/skk-proxy
```

オプション:

```text
--listen 127.0.0.1:1178          macSKK が接続するアドレス
--azookey 127.0.0.1:1180         azoo-key-skkserv
--yaskkserv2 127.0.0.1:1179      yaskkserv2
--azookey-timeout-ms 1500        azookey のタイムアウト
--yaskkserv2-timeout-ms 500      yaskkserv2 のタイムアウト
```

ログレベルは `RUST_LOG` で変更できます（例: `RUST_LOG=debug`）。

## macOS ログイン時の自動起動

LaunchAgent で yaskkserv2 / skk-proxy を登録できます。  
azooKey SKKServ は GUI アプリで自動起動する想定のため、LaunchAgent には含めません。

```sh
# 1. 設定ファイルを作成
cp launchd/config.env.example launchd/config.env
# config.env を編集（yaskkserv2 のパスなどを指定）

# 2. リリースビルド
cargo build --release

# 3. 登録・起動
chmod +x scripts/install-launchd.sh scripts/wait-and-run-skk-proxy.sh
./scripts/install-launchd.sh install
```

yaskkserv2 は次のオプションで起動します:

```text
yaskkserv2 --port 1179 --google-suggest ~/Documents/SKK/dictionary.yaskkserv2
```

ログは `~/Library/Logs/skk-proxy/` に出力されます。

```sh
# 状態確認
./scripts/install-launchd.sh status

# 停止・削除
./scripts/install-launchd.sh uninstall
```

skk-proxy は azooKey SKKServ (:1180) と yaskkserv2 (:1179) の起動を待ってから立ち上がります。

## macSKK 設定

1. 辞書メニューから **SKKServ** を有効化
2. アドレス: `127.0.0.1`
3. ポート: `1178`
4. **応答エンコーディング: UTF-8**

## プロトコル動作

| opcode | 動作 |
|--------|------|
| `0` | 切断 |
| `1` | 変換候補検索（順次フォールバック: azookey → yaskkserv2） |
| `2` | `skk-proxy/0.1.0 ` を返却 |
| `3` | ホスト情報を返却 |
| `4` | サーバー補完（両バックエンドを並列照会 → マージ → 頻度で並べ替え） |

opcode `1` では primary がタイムアウト・接続失敗・`4`（未検出）の場合に fallback を照会します。  
opcode `4` では両方の結果を統合し、過去に候補が1件だけ返った変換（暗黙の選択）を学習して補完候補を並べ替えます。  
履歴は `~/.skk-proxy-frequency.json` に保存されます（旧 `~/.skk-proxy-bayesian.json` から自動移行）。
