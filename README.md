# RuSKK

macSKK 向けの高速・インテリジェントな skkserv プロキシサーバーです。  
[azooKey SKKServ](https://github.com/gitusp/azoo-key-skkserv) を最優先し、見つからない場合のみ [yaskkserv2](https://github.com/wachikun/yaskkserv2)（Google Suggest 連携・統合辞書）にフォールバックします。

```text
macSKK  →  RuSKK (:1178, UTF-8)  →  azoo-key-skkserv (:1180, Primary)
                                 →  yaskkserv2 (:1179, Fallback)
```

---

## 主な特徴

- **送りあり見出し・複合語の活用形自動復元と語幹抽出**:
  - 通常動詞（`かk` → `かく` → 「書」、`きr` → `きる` → 「切」「着」）
  - 大文字接尾辞・複合語（`交ぜGak` / `交ぜGk` → `交ぜがき` → 「交ぜ書」、`まぜがk` → 「混ぜ書」）
- **直前確定単語に基づく文脈連動候補昇格**:
  - 「肉」確定後の `きr` → 「切る（切）」が第1候補
  - 「服」確定後の `きr` → 「着る（着）」が第1候補
  - 「木」確定後の `きr` → 「伐る（伐）」が第1候補
  - 同一接続単位での補完連射ガード（50ms）により、タイピング途中の過渡的照会による文脈汚染を確実に防止
- **高速な順次フォールバック（azooKey → yaskkserv2）**:
  - azooKey（AI推論・日常語・送りあり）にない固有名詞・専門用語・Wikipedia 語は yaskkserv2 巨大辞書から自動補完
- **見出し語リアルタイム補完（opcode 4）フォワード対応**:
  - 入力途中の推測補完リストを Primary / Fallback からシームレスに macSKK へ提供
- **seed ファイル（`~/.ruskk-frequency.json`）による安全な並び替え**:
  - 手動編集可能な単語頻度（`frequencies`）と文脈共起（`context_frequencies`）に基づき候補を整列
  - 組み込みプリセット（`ruskk init-seed`）や macSKK ユーザー辞書自動インポート機能（LaunchAgent）を完備
- **ゼロコピー・超低遅延設計**:
  - 並び替え不要な単語は不要なパース・アロケーションを完全スキップ
  - azooKey SKKServ（EUC-JP リクエスト / UTF-8 レスポンス）および yaskkserv2（EUC-JP）のエンコーディング差分を自動吸収
- **即時切り替え・ロールバック安全機能**:
  - 送りあり復元や文脈連動などの各機能は、環境変数または CLI 引数で再ビルドなしに即座に無効化可能

---

## 送りあり見出しの活用形復元・語幹抽出

### 課題と解決
SKK では動詞・形容詞の送り仮名の最初の子音/母音をローマ字で入力します（例: `かk` で「書く」）。  
これをそのまま azooKey SKKServ に問い合わせると、「化/家/下/科...」など大量の同音名詞の単漢字が返ってしまい、目的の動詞（「書」）が数十番目に埋没してしまいます。また、`交ぜGak` や `交ぜGk` のような大文字接尾辞複合語では目的の動詞・名詞候補がヒットしません。

RuSKK は形態素解析器（MeCab / ChaSen 活用表）の規則に基づき、見出し語を自動判定・復元します：

1. **活用形の自動合成**:
   - 通常動詞: `かk` → `かく`、`きr` → `きる`、`よm` → `よむ`、`あかi` → `あかい`
   - 複合語・大文字接尾辞: `交ぜGak` / `交ぜGk` → `交ぜがき` / `交ぜがく`
   - 平仮名複合語: `まぜがk` → `まぜがき` / `まぜがく`
2. **アップストリーム照会**:
   - 合成した平仮名で azooKey / yaskkserv2 に照会し、精度の高い活用形候補（「書く」「交ぜ書き」「混ぜ書き」等）を取得
3. **語幹の厳密な抽出**:
   - 送り仮名を取り除き、語幹「書」「交ぜ書」「混ぜ書」を抽出
   - 終止形（`く`）照会時に「交ぜ学」のような同音名詞が誤って混入しないよう、連用形（`き`）と文字数・漢字判定を組み合わせて厳密に分離
4. **macSKK での表示**:
   - 抽出された語幹候補を macSKK に返却。macSKK 側で記憶している送り仮名と結合され、「書く」「交ぜ書き」として画面上の第1候補に表示されます。

### 送りあり復元機能の無効化（ロールバック）
従来の動作（送りあり見出しをそのまま透過照会）に戻したい場合、再ビルド不要で即座に切り替えられます。

- **CLI / 一時実行**:
  ```sh
  ruskk --okuri-expansion=false
  # または
  RUSKK_OKURI_EXPANSION=0 ./target/release/ruskk
  ```
- **LaunchAgent 運用**:
  `launchd/config.env` で `RUSKK_OKURI_EXPANSION=0` を設定し、`./scripts/install-launchd.sh install` を実行します。

---

## 直前確定単語に基づく文脈連動候補昇格

「肉」を入力した直後に「きr」を打った場合は「切る（切）」、「服」の直後なら「着る（着）」、「木」の直後なら「伐る（伐）」を第1候補に自動昇格させます。

### 仕組みと安全性
- クライアント接続ごとに前回の確定単語（漢字を含む単語のみ）を文脈（`session_context`、有効期限60秒）として記憶します。
- 同一見出し語での次候補送り（Space キー連打）中は保留を維持します。
- **補完連射ガード**: macSKK がキー入力中に送信する超短時間（50ms未満）の自動補完連射 Lookup は、同一 TCP 接続判定によって文脈登録から自動除外されます。これにより、タイピング過渡状態の文字で確定文脈が誤って上書きされることはありません。

### 文脈連動機能の無効化（ロールバック）
- **CLI / 一時実行**:
  ```sh
  ruskk --context-ranking=false
  # または
  RUSKK_CONTEXT_RANKING=0 ./target/release/ruskk
  ```
- **LaunchAgent 運用**:
  `launchd/config.env` で `RUSKK_CONTEXT_RANKING=0` を設定し、`./scripts/install-launchd.sh install` を実行します。

---

## 候補の並び替えと Seed ファイル (`~/.ruskk-frequency.json`)

RuSKK は自動学習を行わず、ユーザーが手動で編集・確認できる読み取り専用の seed ファイル (`~/.ruskk-frequency.json`) を使って候補を安全に並び替えます。

`~/.ruskk-frequency.json` 例:
```json
{
  "frequencies": {
    "きr": { "切る": 2, "着る": 2, "伐る": 2 }
  },
  "context_frequencies": {
    "肉": { "切る": 10 },
    "服": { "着る": 10 },
    "木": { "伐る": 10 },
    "スイッチ": { "切る": 10 }
  }
}
```

### 文脈共起プリセットの導入 (`init-seed`)
日本語で頻出する代表的な同音異義語・送りあり動詞のコロケーション（共起ペア）を組み込んだプリセットを、ワンコマンドで安全に導入・マージできます。

```sh
# 既存の個人頻度データを保持したまま、文脈プリセットをマージ
ruskk init-seed

# 完全にプリセットの初期状態に戻す場合
ruskk init-seed --force
```

### macSKK ユーザー辞書からの自動インポート
macSKK のローカル辞書（`skk-jisyo.utf8`）に蓄積されたユーザーの変換履歴を取り込むことができます（`context_frequencies` は維持されたまま、`frequencies` のみが更新されます）。

```sh
ruskk import-user-dict ~/Library/Containers/net.mtgto.inputmethod.macSKK/Data/Documents/Dictionaries/skk-jisyo.utf8
```

#### 1時間ごとの完全自動定期インポート (LaunchAgent)
`launchd/config.env` に `MACSKK_USER_DICT_PATH` を指定して `./scripts/install-launchd.sh install` を実行すると、macOS の TCC 権限を恒久保持する専用アプリ `~/Applications/RuSKKImporter.app` が生成され、1時間ごとに自動インポートが実行されます。

初回のみ、「システム設定」→「プライバシーとセキュリティ」→「フルディスクアクセス」で `~/Applications/RuSKKImporter.app` を追加・許可してください。

---

## macSKK の推奨設定

### ① 辞書設定（Fallback 巨大辞書化の推奨）
macSKK はローカルの「ユーザー辞書」および「追加辞書」を skkserv よりも最優先で表示する仕様があります。ローカル辞書が大量に有効化されていると、RuSKK や azooKey の高精度な文脈予測がローカル辞書の固定順で上書きされてしまいます。

**推奨構成**:
- macSKK 側で追加していた静的辞書（`SKK-JISYO.L`, `neologd`, `jawiki`, `hatena` 等）は、`yaskkserv2_make_dictionary` で 1 つの統合バイナリ辞書（`~/Documents/SKK/dictionary.yaskkserv2`）に集約し、yaskkserv2（:1179）に持たせます。
- macSKK の「辞書」設定画面では、追加辞書をすべて OFF（無効化）にし、SKKServ（`127.0.0.1:1178`、UTF-8）のみを有効にします。

### ② 補完機能の誤爆確定防止設定
macSKK 内部では、補完候補表示後に一定時間（標準 0.5〜1.0秒）経過すると、タイピング途中の打鍵が候補選択キーと誤認されて勝手に確定される仕様があります。

リアルタイム補完を快適に使いつつ誤爆確定を防ぐため、以下の設定をターミナルで実行することを推奨します（補完選択は Tab キーで行います）：

```sh
# 読み入力から候補選択に切り替わるまでの時間を 5秒（5000ms）に延長
defaults write net.mtgto.inputmethod.macSKK completionConfirmationTimeLimit -int 5000

# ピリオド確定を無効化
defaults write net.mtgto.inputmethod.macSKK fixedCompletionByPeriod -int 0
```

---

## ビルドとインストール

### ビルド
```sh
cargo build --release  # ./target/release/ruskk
cargo test             # 全 34 件のテスト合格を確認
```

### LaunchAgent による常駐運用
macOS ログイン時に yaskkserv2 および RuSKK を自動起動します（azooKey SKKServ は macOS アプリ側で自動起動します）。

```sh
# 1. 設定ファイルを作成・編集
cp launchd/config.env.example launchd/config.env

# 2. サービスを登録・起動
./scripts/install-launchd.sh install

# 稼働ステータス確認
./scripts/install-launchd.sh status

# 停止・アンインストール
./scripts/install-launchd.sh uninstall
```

ログファイル:
- RuSKK 本体ログ: `~/Library/Logs/ruskk/ruskk.log`
- 定期インポートログ: `~/Library/Logs/ruskk/import.log`

---

## CLI オプション一覧

```text
Usage: ruskk [OPTIONS] [COMMAND]

Commands:
  import-user-dict  macSKK ユーザー辞書から頻度データをインポート
  init-seed         ~/.ruskk-frequency.json に文脈共起プリセットを初期化/マージ
  help              ヘルプ表示

Options:
  --listen <LISTEN>                      macSKK が接続するアドレス [default: 127.0.0.1:1178]
  --azookey <AZOOKEY>                    azooKey SKKServ のアドレス [default: 127.0.0.1:1180]
  --yaskkserv2 <YASKKSERV2>              yaskkserv2 のアドレス [default: 127.0.0.1:1179]
  --azookey-timeout-ms <MS>              azooKey 照会のタイムアウト (ms) [default: 1500]
  --yaskkserv2-timeout-ms <MS>           yaskkserv2 照会のタイムアウト (ms) [default: 700]
  --okuri-expansion <BOOL>               送りあり見出し活用形復元 [default: true] [env: RUSKK_OKURI_EXPANSION]
  --context-ranking <BOOL>               直前単語に基づく文脈連動候補昇格 [default: true] [env: RUSKK_CONTEXT_RANKING]
  -h, --help                             ヘルプ表示
  -V, --version                          バージョン表示
```

---

## プロトコル仕様

| opcode | 動作 | 処理内容 |
| :---: | :--- | :--- |
| `0` | 切断 | セッション終了 |
| `1` | 変換照会 | 送りあり復元 → azooKey 照会 → yaskkserv2 フォールバック → 文脈・頻度整列 |
| `2` | バージョン | `ruskk/0.1.0 ` を返却 |
| `3` | ホスト情報 | ホスト情報を返却 |
| `4` | 補完照会 | azooKey / yaskkserv2 に照会して見出し補完リストを返却（文脈保留には影響しない） |
