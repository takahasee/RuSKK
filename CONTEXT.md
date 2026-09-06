# RuSKK 開発引き継ぎコンテキスト

本ファイルは、ディレクトリ名変更や新規セッション開始時に AI アシスタントおよび開発者が直近の設計・実装状態を即座に把握できるようにするためのまとめです。

---

## 1. プロジェクト概要

- **名称**: RuSKK（バイナリ・クレート名: `ruskk`）
- **リポジトリ**: `https://github.com/takahasee/RuSKK`
- **目的**: macSKK 向けの高速・インテリジェントな skkserv プロキシ。
  - **Primary**: `azoo-key-skkserv` (`127.0.0.1:1180`, UTF-8)
  - **Fallback**: `yaskkserv2` (`127.0.0.1:1179`, EUC-JP, Google Suggest 連携)
  - **Listen**: `127.0.0.1:1178` (UTF-8)

---

## 2. 実装済みの重要機能・仕様

### ① バックエンド判定と候補順序（`BackendHit`）
- **azooKey（Primary）がヒットした場合**:
  - azooKey の言語モデルによる候補順序を 100% 保持してそのままクライアントへ返却。
  - 日常単語による学習履歴の汚染を防ぐため、**`observe`（履歴学習）は呼び出さない**。
- **yaskkserv2（Fallback）がヒットした場合**:
  - 見出し語を EUC-JP にエンコードして yaskkserv2 へ送信。
  - yaskkserv2 の結果は UTF-8 にデコード。
  - フォールバックで候補が確定したものは頻度学習（`observe`）の対象とする。
  - 過去の選択頻度に基づき候補を再ソート（`rank_candidates`）。

### ② 補完機能（opcode `4`）
- 両バックエンドを並列照会し、候補をマージ・重複排除した上で、過去の選択頻度に基づいて候補を並べ替えて返却。

### ③ 頻度学習ファイル
- 保存先: `~/.ruskk-frequency.json`
- レガシー互換: `~/.skk-proxy-frequency.json` や `~/.skk-proxy-bayesian.json` が存在する場合は自動移行。

---

## 3. LaunchAgent 運用仕様

- **Plist**:
  - `launchd/com.ruskk.skkserv.plist`（ラベル: `com.ruskk.skkserv`）
  - `launchd/com.ruskk.yaskkserv2.plist`（ラベル: `com.ruskk.yaskkserv2`）
- **起動待機スクリプト**:
  - `scripts/wait-and-run-ruskk.sh`: azooKey (:1180) と yaskkserv2 (:1179) のポート導通を確認した後に `ruskk` を起動。
- **管理スクリプト**:
  - `scripts/install-launchd.sh [install|uninstall|status]`
  - ログ出力先: `~/Library/Logs/ruskk/`
- **設定ファイル**:
  - `launchd/config.env`

---

## 4. ビルド & テストコマンド

```sh
cargo build --release  # ./target/release/ruskk
cargo test             # 18 テスト全て合格確認済み
```
