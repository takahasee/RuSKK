# RuSKK 開発引き継ぎコンテキスト (CONTEXT.md)

本ドキュメントは、新規セッション開始時や開発者が直近の設計・実装状態を即座に把握できるようにするためのまとめです。詳細な運用ルールや知見は `AGENTS.md` も併せて参照してください。

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

### ① バックエンド仕様とエンコーディング
- **Primary (`azoo-key-skkserv`, :1180)**:
  - macOS の `azooKey skkserv.app` は **リクエストに EUC-JP** を要求し、**レスポンスに UTF-8** を返す（`UpstreamEncoding::EucJpRequestUtf8Response`）。
- **Fallback (`yaskkserv2`, :1179)**:
  - 見出し語を EUC-JP にエンコードして送信、返却結果（EUC-JP）を UTF-8 にデコード（`UpstreamEncoding::EucJp`）。

### ② 候補の並び替えとセッション文脈追跡（`rank_candidates`）
- skkserv プロトコルの仕様上、クライアント（macSKK）側でユーザーが実際に確定した候補を知る手段はありません。
- 過去のリアルタイム自動学習機能は、azooKey の第1候補を誤学習する自己強化ループを引き起こしたため **完全廃止** されました。
- 代わりに、**手動およびプリセットに基づく Seed ファイル (`~/.ruskk-frequency.json`)** を読み取り専用として使用します。
- 通常変換（Lookup）で返却された直前の漢字単語をオンメモリのセッション文脈（`session_context`）として安全に追跡し、Seed ファイル内の文脈共起データ（`context_frequencies`: 「服」→「着る」、「肉」→「切る」など）に基づいて候補を第1候補へ昇格・並び替えます。
- **60秒タイムアウト**: 最後の漢字変換から60秒経過した古い文脈は自動クリアされ、長時間放置後の誤った引きずりを防ぎます。

### ③ 補完機能と文脈汚染防止、および全体最適設定（プラン A）
- **opcode `4`（補完）**: 両バックエンドを並列照会し、候補をマージ・重複排除した上で返却。
- **汚染防止（`is_completion_refer`）**: macSKK が裏で自動送信する補完バースト Lookup やひらがな1文字プレビュー Lookup は文脈追跡から確実に除外されます。また、助詞や平仮名単語では文脈は上書きされません。
- **macSKK 側の補完最適設定（プラン A: 誤爆ゼロ構成）**:
  - macSKK 内部のホームポジション確定（`ASDFGHJKL`）は AZIK/ローマ字打鍵キーと 100% 衝突するため、`completionConfirmationTimeLimit -int 86400000`（24 Hours）で実質無効化。
  - `fixedCompletionByPeriod -int 0` により、AZIK の句点打鍵（`.`）との衝突を根絶。
  - macSKK 内部で補完選択中の Space は未実装（`TODO`）で何もしない仕様のため、補完選択・巡回は `Tab` キーで行い、通常の変換は SKK 王道の「見出し語 ＋ Space」に集約。

### ④ Seed データ・プリセットとインポート機能
- **保存先**: `~/.ruskk-frequency.json`
- **組み込みプリセット**: `data/default-seed.json`（日常・ビジネス頻出の同音異義語コロケーションを約 420 エントリ網羅）。
- **`ruskk init-seed [--force]`**: 既存の個人頻度を壊さずに文脈プリセットを安全にマージ・初期化。
- **`ruskk import-user-dict <path> [--send-reload]`**: macSKK ユーザー辞書（`skk-jisyo.utf8`）から選択頻度をインポート。`--send-reload` 指定時はインポート完了後に `pkill -HUP -x ruskk` を実行してホットリロードをトリガーする（文脈共起が空の場合はプリセットも自動注入）。
- **SIGHUP ホットリロード**: `ruskk` プロセスが SIGHUP を受信すると、`~/.ruskk-frequency.json` を再起動なしで即座に再読み込みする。LaunchAgent による毎時自動インポートもホットリロードで反映される。

### ⑤ 送りあり見出し・複合語の活用形自動復元と語幹抽出
- **背景**: `かk` や `といあわs`, `交ぜGak` などの送りあり見出しを azooKey に直接照会すると、同音名詞の単漢字が大量に返って目的の動詞・複合語が埋没する。
- **メカニズム**:
  - 見出しを全活用形（終止形・連用形・下一段形など）に展開し、azooKey / yaskkserv2 に照会。
  - 各活用形から抽出された語幹候補（「書」「交ぜ書」「書き起こ」「書起こ」「問い合わ」「問合」「割当」「引出」）を重複排除マージして macSKK に返却。
  - 送りあり復元は `RUSKK_OKURI_EXPANSION=0` または `--okuri-expansion=false` で即座にロールバック可能。

### ⑥ ゼロコピー・O(1)・SIMD 最適化
- **柔軟照合の O(1) 化**: seed ロード時に双方向エイリアス（「切る」↔「切」等）を展開し、ランタイムでの線形探索 $O(N)$ を完全排除。
- **EUC-JP 一括 SIMD デコード**: スラッシュ分割・逐次変換を撤廃し、`EUC_JP.decode(response)` の SIMD 最適化一括変換へ移行。
- **ゼロアロケーション整列**: 候補ソートに `sort_unstable_by`（PDQsort）を採用し、通常文脈（要素数 0/1）のインライン化と静的スライス参照により、定常時の不要ヒープ確保をゼロ化。

---

## 3. LaunchAgent 運用仕様

- **Plist**:
  - `launchd/com.ruskk.skkserv.plist`（ラベル: `com.ruskk.skkserv`）
  - `launchd/com.ruskk.yaskkserv2.plist`（ラベル: `com.ruskk.yaskkserv2`）
  - `launchd/com.ruskk.import-user-dict.plist`（ラベル: `com.ruskk.import-user-dict`）
- **起動スクリプト**:
  - `scripts/wait-and-run-ruskk.sh`: azooKey (:1180) と yaskkserv2 (:1179) の待機後に `ruskk` を起動。
- **管理スクリプト**:
  - `scripts/install-launchd.sh [install|uninstall|status]`
- **設定ファイル**:
  - `launchd/config.env`
- **ログ**: `~/Library/Logs/ruskk/`

---

## 4. ビルド & テストコマンド

```sh
cargo build --release  # ./target/release/ruskk
cargo test             # 全41テスト合格（単体・モック・統合テスト）
cargo clippy           # 警告 0 件確認済み
```
