# RuSKKserv

macSKK 向けの高速・インテリジェントな skkserv プロキシサーバーです。  
[azooKey SKKServ](https://github.com/gitusp/azoo-key-skkserv) を最優先し、見つからない場合のみ [yaskkserv2](https://github.com/wachikun/yaskkserv2)（Google Suggest 連携・統合辞書）にフォールバックします。

```text
macSKK  →  RuSKKserv (:1178, UTF-8)  →  azoo-key-skkserv (:1180, Primary)
                                    →  yaskkserv2 (:1179, Fallback)

※ 通常の単語は見つからない場合のみ Fallback に照会しますが、
   送りあり見出しの場合は Primary の活用形復元結果と Fallback の全候補を自動的にマージして返却します。
```

---

## 主な特徴

1. **送りあり見出し・複合語の活用形自動復元（Okuri Expansion）**:
   - `かk` → `かく`（「書」）、`きr` → `きる`（「切」「着」）などの通常動詞に対応。
   - 語幹2文字以上の複合動詞・多音節語（`ねあg` → 「値上げ」「値上」、`ひきあt` → 「引き当て」「引当」、`とりあつかw` → 「取り扱い」「取扱」、`交ぜGk` → 「交ぜ書」など）を全子音・母音で均質に展開。
   - 五十音全段（連用形・下一段名詞）から語幹を漏れなく抽出し、SKK 辞書固有語とマージ。
2. **直前確定単語に基づく文脈連動候補昇格（Context Ranking）**:
   - 「肉」確定後の `きr` → 「切る（切）」、「服」確定後なら「着る（着）」、「木」確定後なら「伐る（伐）」を第1候補に自動昇格。
   - 「時間」→「計る」、「体重」→「量る」、「距離」→「測る」など、800組以上の文脈共起ペアを標準サポート。
   - **macSKK 特有の挙動対策**: ▽ モード開始時の過渡的 1 文字 Lookup やキー連射による文脈汚染を自動防御。
3. **macSKK の辞書競合を解消するシームレス統合**:
   - 巨大辞書群（NEologd, Wikipedia 等）を yaskkserv2 に集約し、macSKK は RuSKKserv 一本に一本化。
   - ユーザー辞書の学習順序を冪等に取り込むインポートコマンド（`import-user-dict`）と SIGHUP ホットリロードを完備。
4. **ゼロコピー・Apple Silicon 最適化による極小レイテンシ**:
   - パースの借用スライス化、EUC-JP 一括 SIMD デコード、Apple Silicon 128バイト キャッシュライン整合（`#[repr(align(128))]`）、`memchr` NEON ベクトル走査、Full LTO、`mimalloc` により 100〜200ms 台の高速応答を維持。

---

## クイックスタート

### 1. ビルド & テスト
```sh
cargo build --release  # ./target/release/ruskkserv
cargo test             # 全 52 件のユニット・結合テスト合格を確認
```

### 2. LaunchAgent による常駐運用
macOS ログイン時に yaskkserv2 および RuSKKserv を自動起動します（azooKey SKKServ は macOS アプリ側で自動起動）。

```sh
# 1. 設定ファイルを作成
cp launchd/config.env.example launchd/config.env

# 2. サービスを登録・起動
./scripts/install-launchd.sh install

# 稼働ステータス確認
./scripts/install-launchd.sh status
```

### 3. macSKK の推奨設定（プラン A: 誤爆ゼロ・王道構成）
macSKK のローカル履歴保存による文脈上書きと、補完タイマーによる誤爆を根絶します。

```sh
# 1. 変換履歴のユーザー辞書保存を OFF（RuSKKserv の文脈予測をフル活用）
python3 -c "
import subprocess, plistlib
out = subprocess.check_output(['defaults', 'export', 'net.mtgto.inputmethod.macSKK', '-'])
d = plistlib.loads(out)
d.setdefault('skkserv', {})['saveToUserDict'] = False
p = subprocess.Popen(['defaults', 'import', 'net.mtgto.inputmethod.macSKK', '-'], stdin=subprocess.PIPE)
p.communicate(plistlib.dumps(d))
"

# 2. ホームポジションキー誤爆を防止（macSKK 公式の「24 Hours」= 86400000ms に設定）
defaults write net.mtgto.inputmethod.macSKK completionConfirmationTimeLimit -int 86400000

# 3. AZIK と衝突するピリオド（.）確定を無効化
defaults write net.mtgto.inputmethod.macSKK fixedCompletionByPeriod -int 0

# 4. macSKK を再起動して反映
killall macSKK
```

---

## アーキテクチャと機能詳細

RuSKKserv は、以下の独立したレイヤーが役割分担して連携することで、高精度と超低遅延を両立しています。

```text
macSKK (入力) ─┬─ [opcode 4 (補完)] ─→ Primary/Fallback 透過照会 ──→ Tab 先読み補完
               │
               └─ [opcode 1 (変換)] ─┬─ 送りあり活用復元 & 全子音マージ (azooKey 活用)
                                     └─ 直前確定単語 (session_context) ──→ 共起スコア昇格
```

### ① 送りあり見出しの活用形復元・語幹抽出（Okuri Expansion）

SKK 特有の送りあり見出し（例: `かk`, `ねあg`, `もうしこm`）を azooKey SKKServ に直接照会すると、同音名詞の単漢字（化/家/科...）が大量に返って目的の動詞が埋没したり、複合語がヒットしない問題があります。

RuSKKserv は日本語の動詞・形容詞活用体系に準拠し、特定子音のアドホック分岐を排除した体系的復元を行います：

1. **全子音の体系的活用合成**:
   語幹 2 文字以上の見出し語に対し、全子音（k, s, t, n, m, r, g, b, u/w, d, c, y, j, p）の下一段（え段）、連用形（い段）、終止形（う段）、音便を均質に展開（`ねあg` → `ねあげ`, `ねあぎ`, `ねあぐ`、`ひきあt` → `ひきあて`, `ひきあつ`、`とりあつかw` → `とりあつかい`, `とりあつかう`）。アスタリスク記法（`ねあ*げ`）や大文字送りキー（`ねあG`）も認識します。
2. **Primary (azooKey) 限定照会と低遅延化**:
   活用バリエーション照会は azooKey に対してのみ実行し、不要な辞書照会を排除。各照会 300ms・全体 600ms でキャップし、100〜200ms 台の高速応答を維持します。
3. **五十音全段の語幹抽出 & マージ**:
   返却候補から送り仮名を剥がした語幹に加え、連用形・下一段名詞（「値上」「引当」「申込」「取扱」など）を五十音全段で抽出。元の見出し語による Fallback (yaskkserv2) 候補を末尾にマージして重複排除します。

> **ロールバック**: 送りあり復元を無効化したい場合は、CLI 引数 `--okuri-expansion=false` または環境変数 `RUSKKSERV_OKURI_EXPANSION=0` で即座に切り替えられます。

### ② 直前確定単語に基づく文脈連動候補昇格（Context Ranking）

直前に確定した単語（漢字を含む単語のみ）を記憶（複数 TCP セッション間で共有・有効期限 60 秒）し、共起辞書に基づいて候補スコアを加算（$\text{Context Score} \times 10$）して第1候補へ昇格させます。

* **macSKK の機械的 1 文字 Lookup 対策**:
  macSKK は ▽ 入力開始時に先頭 1 文字（例: `き`）を自動照会します。RuSKKserv は新しい見出し語が保留中見出しの前方一致伸長（`き`→`きr`、`に`→`にく`）かつ未確定（受信回数 2 未満）の場合、入力途中の伸長とみなして**文脈昇格せずに安全に破棄**し、真の確定文脈のみを保持します。
* **補完連射ガード**:
  macSKK がキー入力中に送信する 50ms 未満の機械的スキャンは文脈登録から自動除外されます。
* **組み込みプリセットへの自動フォールバック**:
  seed ファイル未作成時やデータ不在時でも、バイナリ組み込みの共起プリセット（800組以上）がメモリ上で自動適用されます。

> **ロールバック**: 文脈連動を無効化したい場合は、CLI 引数 `--context-ranking=false` または環境変数 `RUSKKSERV_CONTEXT_RANKING=0` で即座に切り替えられます。

### ③ システム辞書・注釈（annotation）優先機能（Annotation Priority）

macOS のシステム国語辞典（大辞林等）に項目が存在する単語、および SKK 辞書由来の注釈（`;注釈`）が付いている単語にボーナススコア（100点）を付与し、**加点スコア方式**で総合順位を決定します。

* **加点スコア方式（ハイブリッド）**:
  辞書掲載語や注釈付きの一般的な日本語単語をベース 100 点とし、辞書外の俗語・珍奇な当て字・誤変換残骸（0 点）を確実に後方へ沈めます。一方で、文脈共起スコア（直前単語との組み合わせ）やユーザー辞書の頻度スコアと合算されるため、強い文脈がある場合や高頻度な独自単語は自然に上位へ浮上できます。
* **送りあり（動詞・形容詞）の終止形復元照会**:
  送りあり見出し（例: `かk`, `すばらしi`）の語幹（`書`, `素晴らし`）に対しても、送り仮名を補った終止形（`書く`, `素晴らしい`）を自動合成して大辞林を引くため、動詞や形容詞も漏れなく優先判定されます。
* **インメモリキャッシュ（超低遅延）**:
  macOS DictionaryServices API (`DCSCopyTextDefinition`) の判定結果を最大 20,000 件までインメモリキャッシュし、2回目以降の変換をナノ秒単位でゼロオーバーヘッド処理します。

> **ロールバック**: 注釈・システム辞書優先を無効化したい場合は、CLI 引数 `--annotation-priority=false` または環境変数 `RUSKKSERV_ANNOTATION_PRIORITY=0` で即座に元の動作へ戻せます。

### ④ リアルタイム見出し補完（`Request::Completion` / opcode 4）

タイピング中に macSKK から送信される補完要求（opcode 4）を Primary / Fallback へ照会し、見出し候補リストを高速返却します。補完候補の選択中は `Tab` キーで巡回し、`Enter` で確定できます（補完照会は文脈追跡に影響しません）。

### ⑤ パフォーマンス・ハードウェア最適化

- **ゼロコピー・ファストパス**: 単一候補の単語や並び替え対象外の単語は、パースやヒープ確保を完全スキップして直結転送（**0.01ms 未満**）。
- **$O(1)$ 柔軟照合**: seed ロード時に前方一致エイリアス（「切る」↔「切」等）を双方向展開し、ランタイムの線形探索を完全排除。
- **EUC-JP 一括 SIMD デコード**: スラッシュ区切りごとの逐次デコードを全廃し、SIMD 最適化一括デコードを適用。
- **PDQsort**: 候補整列に `sort_unstable_by` を採用し、定常時アロケーションをゼロ化。
- **Apple Silicon 最適化**: `SharedContextState` を 128 バイト境界に配置（`#[repr(align(128))]`）し、M1〜M4 等のキャッシュライン跨ぎ・偽共有（False Sharing）を防止。手書き NEON ベクトル走査（`memchr`）、Full LTO、`mimalloc` を採用。

---

## Seed ファイルと辞書管理 (`~/.ruskkserv-frequency.json`)

RuSKKserv は自動学習を行わず、ユーザーが手動で編集・確認できる読み取り専用の seed ファイルを使って候補を安全に並び替えます。

```json
{
  "frequencies": {
    "きr": { "切る": 2, "着る": 2, "伐る": 2 }
  },
  "context_frequencies": {
    "肉": { "切る": 10 },
    "服": { "着る": 10 },
    "木": { "伐る": 10 },
    "時間": { "計る": 10 }
  }
}
```

### 文脈共起プリセットの導入 (`init-seed`)
日常会話・ビジネス・IT・医療・料理など幅広い分野を網羅したプリセット（基本約420エントリ、展開後800組以上）を安全にマージ・導入できます。

```sh
# 既存の個人頻度データを保持したまま、文脈プリセットをマージ
ruskkserv init-seed

# 完全にプリセットの初期状態に戻す場合
ruskkserv init-seed --force
```

### SIGHUP によるホットリロード
RuSKKserv プロセスは `SIGHUP` を受信すると、接続を切断することなく最新の `~/.ruskkserv-frequency.json` をメモリ上へ即座に再読み込みします。

```sh
pkill -HUP -x ruskkserv
```

### macSKK ユーザー辞書の自動インポート (`import-user-dict`)
macSKK の変換履歴（`skk-jisyo.utf8`）を取り込み、頻度スコアとして反映します。定期実行でスコアが肥大化しないよう、**最大値（`max`）更新による冪等処理**を採用しています。

```sh
# インポート後に自動で SIGHUP を送信して即座に反映
ruskkserv import-user-dict ~/Library/Containers/net.mtgto.inputmethod.macSKK/Data/Documents/Dictionaries/skk-jisyo.utf8 --send-reload
```

#### LaunchAgent による 1 時間ごとの自動定期インポート（任意）
`launchd/config.env` に `MACSKK_USER_DICT_PATH` を指定して `./scripts/install-launchd.sh install` を実行すると、macOS の TCC（フルディスクアクセス）を恒久保持する専用アプリ `~/Applications/RuSKKservImporter.app` が生成され、1時間ごとに自動インポート（`--send-reload` 付き）が常駐実行されます。

> **Note**: 「変換履歴の保存を OFF」にする推奨構成では、ユーザー辞書（`skk-jisyo.utf8`）に新たな履歴が蓄積されないため、毎時の定期インポートは不要です（空のままにしておけば常駐タスクは登録されません。手動で単語登録した語彙を取り込みたい場合や、従来の履歴保存運用を続けたい場合のみ設定してください）。

---

## macSKK の推奨設定（詳細ガイド）

### ① 辞書設定と「変換履歴の保存」無効化（最重要）

macSKK の候補優先順位は以下のように固定されています：
```text
ユーザー辞書  →  日付変換  →  ファイル辞書  →  skkserv (RuSKKserv)
```
macSKK の初期設定である「変換履歴をユーザー辞書に保存（`saveToUserDict = 1`）」が有効だと、RuSKKserv の候補を選ぶたびにユーザー辞書へ固定順で書き込まれ、**以後は直前単語にかかわらず過去の選択順が最優先で文脈予測を上書きしてしまいます。**

* **対策**:
  1. macSKK 設定 →「辞書」→「skkserv」で **「Save conversion history to User Dictionary（変換履歴をユーザー辞書に保存）」を OFF** にする（明示的な単語登録は引き続き保存され、最優先で利用可能）。
  2. 静的辞書（NEologd, Wikipedia 等）は `yaskkserv2_make_dictionary` で 1 つのバイナリ辞書（`~/Documents/SKK/dictionary.yaskkserv2`）に集約し、macSKK の追加辞書はすべて OFF にする。
  3. 既存のユーザー辞書の並び順は `ruskkserv import-user-dict` で取り込み、語彙は yaskkserv2 辞書に統合する。

### ② 誤爆ゼロ・ストレスフリー構成（プラン A）

macSKK の補完候補表示後のホームポジションキー（`ASDFGHJKL`）確定タイマーやピリオド（`.`）確定は、AZIK や通常打鍵と衝突して誤確定（「すずきせいじゅんあ」等）を引き起こします。

RuSKKserv の「文脈連動昇格」や「送りあり復元」は、SKK 本来の **「見出し語 ＋ Space 変換」** で最速・最高精度で動作するため、タイマーを無効化（`completionConfirmationTimeLimit -int 86400000`）した王道構成を推奨します。長い見出し語の先読み補完は **`Tab`** キーで行います。

---

## CLI オプション一覧

```text
Usage: ruskkserv [OPTIONS] [COMMAND]

Commands:
  import-user-dict  Import frequency data from a macSKK user dictionary
  init-seed         Initialize or merge default context co-occurrence presets into ~/.ruskkserv-frequency.json
  help              Print this message or the help of the given subcommand(s)

Options:
      --listen <LISTEN>
          Address to listen on (macSKK connects here) [default: 127.0.0.1:1178]
      --azookey <AZOOKEY>
          azoo-key-skkserv address (primary) [default: 127.0.0.1:1180]
      --yaskkserv2 <YASKKSERV2>
          yaskkserv2 address (fallback) [default: 127.0.0.1:1179]
      --azookey-timeout-ms <AZOOKEY_TIMEOUT_MS>
          Timeout for azoo-key-skkserv queries (milliseconds) [default: 1500]
      --yaskkserv2-timeout-ms <YASKKSERV2_TIMEOUT_MS>
          Timeout for yaskkserv2 queries (milliseconds) [default: 700]
      --okuri-expansion [<OKURI_EXPANSION>]
          送りあり見出し（例: かk -> かく）の活用復元を行うか（環境変数 RUSKKSERV_OKURI_EXPANSION でも制御可能） [default: true] [possible values: true, false]
      --context-ranking [<CONTEXT_RANKING>]
          直前確定単語に基づく文脈共起並び替えを行うか（環境変数 RUSKKSERV_CONTEXT_RANKING でも制御可能） [default: true] [possible values: true, false]
      --annotation-priority [<ANNOTATION_PRIORITY>]
          システム辞書（大辞林等）に項目がある単語および SKK 注釈付き単語を最優先するか（環境変数 RUSKKSERV_ANNOTATION_PRIORITY でも制御可能） [default: true] [possible values: true, false]
  -h, --help
          Print help
  -V, --version
          Print version

Subcommand: import-user-dict
Usage: ruskkserv import-user-dict [OPTIONS] <PATH>

Arguments:
  <PATH>  Path to macSKK user dictionary (e.g., skk-jisyo.utf8)

Options:
      --send-reload  インポート成功後に実行中の ruskkserv プロセスに SIGHUP を送りホットリロードを行う
  -h, --help         Print help

Subcommand: init-seed
Usage: ruskkserv init-seed [OPTIONS]

Options:
  -f, --force  Overwrite completely with presets instead of merging with existing data
  -h, --help   Print help
```

---

## プロトコル仕様

| opcode | 動作 | 処理内容 |
| :---: | :--- | :--- |
| `0` | 切断 | セッション終了 |
| `1` | 変換照会 | 送りあり復元 → azooKey 照会 → yaskkserv2 フォールバック → 文脈・頻度・システム辞書整列 |
| `2` | バージョン | `ruskkserv/<VERSION> ` を返却 |
| `3` | ホスト情報 | ホスト情報を返却 |
| `4` | 補完照会 | azooKey / yaskkserv2 に照会して見出し補完リストを返却（文脈保留には影響しない） |
