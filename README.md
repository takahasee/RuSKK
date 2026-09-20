# RuSKKserv

macSKK 向けの高速・インテリジェントな skkserv プロキシサーバーです。  
[azooKey SKKServ](https://github.com/gitusp/azoo-key-skkserv) を最優先し、見つからない場合のみ [yaskkserv2](https://github.com/wachikun/yaskkserv2)（Google Suggest 連携・統合辞書）にフォールバックします。

```text
macSKK  →  RuSKKserv (:1178, UTF-8)  →  azoo-key-skkserv (:1180, Primary)
                                 →  yaskkserv2 (:1179, Fallback)

※ 通常の単語は見つからない場合のみ Fallback に照会しますが、送りあり見出しの場合は Primary の活用形復元結果と Fallback の全候補を自動的にマージして返却します。
```

---

## 主な特徴

- **送りあり見出し・複合語の活用形自動復元と全子音マージ（体系的大域統合）**:
  - 通常動詞（`かk` → `かく` → 「書」、`きr` → `きる` → 「切」「着」）
  - 語幹 2 文字以上の複合動詞・多音節語を全子音（k, s, t, n, m, r, g, b, u/w, d, c, y, j, p）で均質展開:
    - `ねあg`（`NeaG` / `ねあ*げ`）→ 下一段「値上げ」「値上」
    - `ひきあt`（`HikiaT`）→ 「引き当て」「引当て」「引当」
    - `いいだs`（`IidaS`）→ 「言い出し」「言出し」「言出」
    - `うけいr`（`UkeiR`）→ 「受け入れ」「受入れ」「受入」
    - `もうしこm`（`MousikoM`）→ 「申し込み」「申込」
    - `しらb`（`SiraB`）→ 「調べ」
    - `とりあつかw`（`ToriatukaW`）→ 「取り扱い」「取扱い」「取扱」
    - `かきおこs` → 「書き起こし」「書起こし」
    - `といあわs`（`ToiawaS`）→ 「問い合わせ」「問合せ」
    - `わりあt`（`WariaT`）→ 「割り当て」「割当て」「割当」
  - 大文字接尾辞・複合語（`交ぜGak` / `交ぜGk` → 「交ぜ書」、`まぜがk` → 「混ぜ書」）
  - アスタリスク明示（`ねあ*げ`）および大文字送りキー（`ねあG`）に完全対応
  - 各照会 300ms 上限 / 全体 600ms 動的キャップによる超低遅延設計（100〜200ms 台で完結）
  - 五十音全段（い段連用形・え段下一段名詞）から語幹候補を重複排除マージし、特定活用形への偏り（局所最適）を完全排除
- **直前確定単語に基づく文脈連動候補昇格**:
  - 「肉」確定後の `きr` → 「切る（切）」が第1候補
  - 「服」確定後の `きr` → 「着る（着）」が第1候補
  - 「木」確定後の `きr` → 「伐る（伐）」が第1候補
  - 同一接続単位での補完連射ガード（50ms）により、タイピング途中の過渡的照会による文脈汚染を確実に防止
- **高速な順次フォールバック（azooKey → yaskkserv2）**:
  - azooKey（AI推論・日常語・送りあり）にない固有名詞・専門用語・Wikipedia 語は yaskkserv2 巨大辞書から自動補完
- **見出し語リアルタイム補完（opcode 4）フォワード対応**:
  - 入力途中の推測補完リストを Primary / Fallback からシームレスに macSKK へ提供（Tab キーでの先読み補完）
- **seed ファイル（`~/.ruskkserv-frequency.json`）による安全な並び替え**:
  - 手動編集可能な単語頻度（`frequencies`）と文脈共起（`context_frequencies`）に基づき候補を整列
  - 420 エントリ以上の充実した文脈プリセット（`ruskkserv init-seed`）や macSKK ユーザー辞書自動インポート機能（LaunchAgent）を完備
  - **SIGHUP ホットリロード対応**: 設定ファイル更新や辞書インポート後も、サーバー再起動なしで即座にメモリ上の頻度・文脈共起データを最新化
- **ゼロコピー・徹底的な超低遅延設計**:
  - 並び替え不要な単語は不要なパース・ヒープ割り当てを完全スキップ（0.01ms 未満のゼロコピー直結転送）
  - seed ロード時エイリアス展開による $O(1)$ 柔軟照合、EUC-JP レスポンスの一括 SIMD デコード、候補整列のゼロアロケーション（PDQsort）を徹底
  - azooKey SKKServ（EUC-JP リクエスト / UTF-8 レスポンス）および yaskkserv2（EUC-JP）のエンコーディング差分を自動吸収
- **即時切り替え・ロールバック安全機能**:
  - 送りあり復元や文脈連動などの各機能は、環境変数または CLI 引数で再ビルドなしに即座に無効化可能

---

## 送りあり見出しの活用形復元・語幹抽出

### 課題と解決
SKK では動詞・形容詞の送り仮名の最初の子音/母音をローマ字（または大文字・アスタリスク記法）で入力します（例: `かk` で「書く」、`ねあg` で「値上げ」）。  
これをそのまま azooKey SKKServ に問い合わせると、「化/家/下/科...」など大量の同音名詞の単漢字が返ってしまい、目的の動詞・複合語が数十番目に埋没してしまいます。また、DDSKK や macSKK のアスタリスク明示（`ねあ*げ`）や大文字キー（`ねあG`）、`交ぜGak` のような複合語では目的の動詞・名詞候補がヒットしません。

RuSKKserv は DDSKK 仕様および日本語の動詞・形容詞活用体系に準拠し、特定子音のアドホック分岐を排除した体系的な復元と語幹抽出を行います：

1. **全子音の体系的活用合成（大域的展開）**:
   - 語幹文字数が 2 文字以上の見出し語（`char_count >= 2`）を複合語・多音節語として体系化。
   - 全子音（k, s, t, n, m, r, g, b, u/w, d, c, y, j, p）に対して、下一段名詞形（え段）、連用形名詞形（い段）、終止形（う段）、音便・促音便・拗音を均質に展開。
     - `ねあg`（`NeaG` / `ねあ*げ`）→ `ねあが`（音便）、`ねあげ`（下一段・名詞）、`ねあぎ`（連用形名詞）、`ねあぐ`（終止形）
     - `ひきあt`（`HikiaT`）→ `ひきあて`、`ひきあつ`、`ひきあち`
     - `いいだs`（`IidaS`）→ `いいだし`、`いいだす`、`いいだせ`
     - `うけいr`（`UkeiR`）→ `うけいれ`、`うけいる`、`うけいり`
     - `もうしこm`（`MousikoM`）→ `もうしこみ`、`もうしこむ`、`もうしこめ`
     - `しらb`（`SiraB`）→ `しらべ`、`しらぶ`、`しらび`
     - `とりあつかw`（`ToriatukaW`）→ `とりあつかい`、`とりあつかう`
   - アスタリスク明示記法（`ねあ*げ` → 語幹 `ねあ`、送り仮名 `げ`、子音 `g`）や大文字送りキー（`ねあG`）もシームレスに認識。
2. **Primary (azooKey) 限定照会と低遅延化**:
   - 送りありバリエーション照会は azooKey の動詞・形容詞辞書に対してのみ実行し、SKK 辞書（yaskkserv2）への不要なフォールバックを排除。
   - 各照会上限 300ms・全体締め切り 600ms の動的キャップを設けることで、未登録語や音便ミス時でも macSKK の 1.0 秒タイムアウト切断を防止し、100〜200ms 台の高速応答を維持。
3. **五十音全段の語幹抽出**:
   - 返却候補から送り仮名を剥がした語幹に加え、すでに語幹化された名詞形（い段連用形名詞・え段下一段名詞）を五十音全段（き・し・ち・に・ひ・み・り・ぎ・じ・び・い／け・せ・て・ね・へ・め・れ・げ・ぜ・で・べ・え）で漏れなく抽出。
   - 送り仮名なしの語幹名詞（「値上」「引当」「言出」「受入」「申込」「取扱」「問合」など）も安全に採用。
4. **辞書固有語の重複排除マージ**:
   - 各活用形から抽出された語幹候補を順序を維持しつつ重複排除してマージ。
   - 元の見出し語による Fallback (yaskkserv2 / 巨大SKK辞書) の照会結果を末尾にマージするため、辞書固有の専門用語や異体字も漏れなく提供されます。
5. **macSKK での表示**:
   - 抽出された語幹候補を macSKK に返却。macSKK 側で送り仮名と結合され、「値上げ」「引き当て」「言い出し」「受け入れ」「申し込み」「調べ」「取り扱い」として画面上の候補に表示されます。

### 送りあり復元機能の無効化（ロールバック）
従来の動作（送りあり見出しをそのまま透過照会）に戻したい場合、再ビルド不要で即座に切り替えられます。

- **CLI / 一時実行**:
  ```sh
  ruskkserv --okuri-expansion=false
  # または
  RUSKKSERV_OKURI_EXPANSION=0 ./target/release/ruskkserv
  ```
- **LaunchAgent 運用**:
  `launchd/config.env` で `RUSKKSERV_OKURI_EXPANSION=0` を設定し、`./scripts/install-launchd.sh install` を実行します。

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
  ruskkserv --context-ranking=false
  # または
  RUSKKSERV_CONTEXT_RANKING=0 ./target/release/ruskkserv
  ```
- **LaunchAgent 運用**:
  `launchd/config.env` で `RUSKKSERV_CONTEXT_RANKING=0` を設定し、`./scripts/install-launchd.sh install` を実行します。

---

## 予測変換と補完の仕組み（3つのレイヤー）

RuSKKserv の予測変換は、単一のエンジンではなく、以下の **3つの独立したレイヤーが役割分担して連携** することで、高い予測精度と超低遅延を両立しています。

```text
macSKK (入力) ─┬─ [opcode 4 (補完)] ─→ Primary/Fallback 透過照会 ──→ Tab 先読み補完
               │
               └─ [opcode 1 (変換)] ─┬─ 送りあり活用復元 & 全子音マージ (azooKey 活用)
                                     └─ 直前確定単語 (session_context) ──→ 共起スコア昇格
```

### レイヤー 1: リアルタイム見出し補完（`Request::Completion` / opcode 4）
- ユーザーがタイピングしている途中、macSKK から自動的に送信される補完要求（opcode 4、例: `4とうき \n`）を受信。
- Primary（azooKey SKKServ）および Fallback（yaskkserv2）へ照会し、見出し補完候補リスト（`1/とうきょう/とうきょうと/.../\n`）を高速返却。
- ユーザーは `Tab` キーを押すだけで、長い単語を最後まで打たずに先読み確定できます。
- **補完連射ガード**: macSKK がキー入力中に送信する 50ms 未満の機械的スキャンは文脈登録から自動除外されるため、タイピング途中の文字で文脈が誤って上書きされることはありません。

### レイヤー 2: 直前確定単語に基づく文脈連動候補昇格（RuSKKserv 独自実装）
- セッション内で直前に確定された単語（漢字を含む単語）を 60 秒間記憶。
- `~/.ruskkserv-frequency.json` の共起辞書（`context_frequencies`）に基づき、候補スコアに重み付け加算（$\text{Context Score} \times 10$）。
- 「服」の直後の「きr」は「着る（着）」、「肉」の直後なら「切る（切）」が自動的に第1候補へ昇格します。
- skkserv プロトコルにはユーザーの最終選択通知が存在しないため、勝手な自動学習による誤爆を排除し、手動定義・確定プリセットのみに基づくクリーンな挙動を保証します。

### レイヤー 3: azooKey 言語モデル予測と活用形マージの融合
- Primary の `azooKey skkserv.app` は、統計的言語モデルによる高精度な現代語・複合語予測変換エンジンを内蔵しています。
- RuSKKserv は SKK の送りあり見出しを全活用形（終止形・連用形・下一段形）に展開して azooKey に照会し、得られた語幹候補を重複排除マージすることで、azooKey 本来の予測変換力を 100% 引き出します。

### パフォーマンス設計（ゼロコピー・O(1)・SIMD 最適化）
- **ゼロコピー・ファストパス**: 並び替えルール（文脈共起や個別頻度）が存在しない大部分の通常単語では、候補リストの文字列パースやヒープ割り当てを完全にスキップし、バックエンドからのバイト列をクライアントへ直結転送（**0.01ms 未満**）します。
- **O(1) 柔軟照合**: seed ロード時に前方一致エイリアス（「切る」↔「切」等）を双方向展開しておくことで、ランタイムでの線形探索 $O(N)$ を完全排除。
- **EUC-JP 一括 SIMD デコード**: スラッシュ区切りの逐次変換を撤廃し、SIMD 最適化されたデコーダで一括変換。
- **ゼロアロケーション整列**: 候補ソートに `sort_unstable_by`（PDQsort）を採用し、定常時の不要ヒープ確保をゼロ化。
- **Apple Silicon 128バイト キャッシュライン整合**: `SharedContextState` に `#[repr(align(128))]` を適用し、Apple Silicon (M1〜M4 / A18 Pro) におけるマルチスレッド並行アクセス時の偽共有（False Sharing）を完全排除。
- **NEON 128-bit SIMD プロトコル走査**: `memchr` による手書き NEON ベクトル命令を活用し、デリミタ走査・候補リストパースを 16 バイト並列一括処理。
- **Full LTO (`lto = "fat"`) & デッドストリップ**: 単一バイナリ全体でのインライン化と不要シンボル削除により、実行バイナリサイズ 1.6MB を達成し命令キャッシュ（I-Cache）の局所性を最大化。
- **高スループットアロケータ (`mimalloc`)**: Apple Silicon の 16KB 仮想メモリページとスレッドローカルフリーリストを活用し、マルチスレッドでのロック競合を抑制。

---

## 候補の並び替えと Seed ファイル (`~/.ruskkserv-frequency.json`)

RuSKKserv は自動学習を行わず、ユーザーが手動で編集・確認できる読み取り専用の seed ファイル (`~/.ruskkserv-frequency.json`) を使って候補を安全に並び替えます。

`~/.ruskkserv-frequency.json` 例:
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
日本語で頻出する代表的な同音異義語・送りあり動詞のコロケーション（共起ペア）を組み込んだプリセット（約420エントリ）を、ワンコマンドで安全に導入・マージできます。

プリセットは日常会話・ビジネス・IT・法律・医療・料理・農業・教育・音楽・スポーツ・自然など幅広い分野を網羅しています（例: 着る/切る/伐る/斬る、計る/測る/量る/図る/謀る/諮る、観る/診る/看る、乗る/載る/撮る/採る/捕る、建てる/立てる、書く/描く、造る/創る/作る、炊く/炒める/焼く/煮る/揚げる、開く/開ける/閉める/消す/点ける/敷く、確認する/変更する/修正する/削除する/追加する、備える/払う/張る等）。

```sh
# 既存の個人頻度データを保持したまま、文脈プリセットをマージ
ruskkserv init-seed

# 完全にプリセットの初期状態に戻す場合
ruskkserv init-seed --force
```

### SIGHUP によるホットリロード
RuSKKserv プロセスは `SIGHUP` シグナルを受信すると、プロキシ接続やサービスを中断することなく `~/.ruskkserv-frequency.json` を再読み込みし、最新の頻度・文脈共起データをインメモリへ即座に反映します。

```sh
# 実行中の RuSKKserv に SIGHUP を送信して設定を即時再読み込み
pkill -HUP -x ruskkserv
# または
kill -HUP $(pgrep -x ruskkserv)
```

手動で `~/.ruskkserv-frequency.json` を編集した際も、サーバーを再起動せずに即座に反映させることができます。

### macSKK ユーザー辞書からの自動インポート
macSKK のローカル辞書（`skk-jisyo.utf8`）に蓄積されたユーザーの変換履歴を取り込むことができます（`context_frequencies` は維持されたまま、`frequencies` のみが更新されます）。

```sh
# インポートのみ実行
ruskkserv import-user-dict ~/Library/Containers/net.mtgto.inputmethod.macSKK/Data/Documents/Dictionaries/skk-jisyo.utf8

# インポート成功後に実行中の ruskkserv に自動で SIGHUP を送信して即座にホットリロード
ruskkserv import-user-dict ~/Library/Containers/net.mtgto.inputmethod.macSKK/Data/Documents/Dictionaries/skk-jisyo.utf8 --send-reload
```

#### 1時間ごとの完全自動定期インポート (LaunchAgent)
`launchd/config.env` に `MACSKK_USER_DICT_PATH` を指定して `./scripts/install-launchd.sh install` を実行すると、macOS の TCC 権限を恒久保持する専用アプリ `~/Applications/RuSKKservImporter.app` が生成され、1時間ごとに自動インポートが実行されます（`--send-reload` 付きで実行されるため、インポート完了と同時に RuSKKserv へ自動反映されます）。

初回のみ、「システム設定」→「プライバシーとセキュリティ」→「フルディスクアクセス」で `~/Applications/RuSKKservImporter.app` を追加・許可してください。

---

## macSKK の推奨設定

### ① 辞書設定（Fallback 巨大辞書化の推奨）
macSKK はローカルの「ユーザー辞書」および「追加辞書」を skkserv よりも最優先で表示する仕様があります。ローカル辞書が大量に有効化されていると、RuSKKserv や azooKey の高精度な文脈予測がローカル辞書の固定順で上書きされてしまいます。

**推奨構成**:
- macSKK 側で追加していた静的辞書（`SKK-JISYO.L`, `neologd`, `jawiki`, `hatena` 等）は、`yaskkserv2_make_dictionary` で 1 つの統合バイナリ辞書（`~/Documents/SKK/dictionary.yaskkserv2`）に集約し、yaskkserv2（:1179）に持たせます。
- macSKK の「辞書」設定画面では、追加辞書をすべて OFF（無効化）にし、SKKServ（`127.0.0.1:1178`、UTF-8）のみを有効にします。

### ② 補完・予測変換の全体最適設定（プラン A: 誤爆ゼロ・ストレスフリー構成）
macSKK 内部には、補完候補表示後にホームポジションキー（`ASDFGHJKL`）で候補確定するタイマー機能がありますが、AZIK やローマ字入力の打鍵キーとキー空間が 100% 衝突するため、タイピング途中に「勝手に確定される」「末尾に文字が漏れて誤確定される（例: すずきせいじゅんあ）」といった誤爆が原理的に発生します。また、ピリオド（`.`）確定も AZIK の句点打鍵と衝突します。

RuSKKserv の真骨頂である **「文脈連動候補昇格」**（「肉」→「切る」、「服」→「着る」）や **「送りあり活用形復元」** は、SKK 本来の **「通常入力 ＋ Space 変換」** で 0ms・誤爆ゼロ・最高精度で動作します。

このため、衝突を起こすタイマー機能とピリオド確定を無効化し、ストレスフリーで最速のタイピング環境を構築する**プラン A（SKK 王道・誤爆ゼロ構成）**を推奨します：

```sh
# 1. ホームポジションキー誤爆を完全に防止（macSKK 公式の「24 Hours」= 86,400,000ms に設定して実質無効化）
defaults write net.mtgto.inputmethod.macSKK completionConfirmationTimeLimit -int 86400000

# 2. AZIK と衝突するピリオド（.）確定を無効化
defaults write net.mtgto.inputmethod.macSKK fixedCompletionByPeriod -int 0

# 3. 設定を反映（macSKK を再起動）
killall macSKK
```

**操作方法**:
- **基本の入力（高精度変換）**: 通常どおり見出し語を入力して **`Space`** を押すだけで、RuSKKserv と azooKey による文脈予測・活用形復元が適用された候補が一発変換されます。
- **長い単語の補完**: 見出し語を途中まで入力した状態で **`Tab`** キーを押すと、補完候補の選択モードに入ります。次候補送りも **`Tab`**（戻すのは `Shift-Tab`）、確定は **`Enter`** です（※macSKK の仕様上、補完選択中の `Space` は動作しません）。

---

## ビルドとインストール

### ビルド
```sh
cargo build --release  # ./target/release/ruskkserv
cargo test             # 全 43 件のテスト合格を確認（ユニット・結合テスト）
```

### LaunchAgent による常駐運用
macOS ログイン時に yaskkserv2 および RuSKKserv を自動起動します（azooKey SKKServ は macOS アプリ側で自動起動します）。

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
- RuSKKserv 本体ログ: `~/Library/Logs/ruskkserv/ruskkserv.log`
- 定期インポートログ: `~/Library/Logs/ruskkserv/import.log`

---

## CLI オプション一覧

```text
Usage: ruskkserv [OPTIONS] [COMMAND]

Commands:
  import-user-dict  macSKK ユーザー辞書から頻度データをインポート
  init-seed         ~/.ruskkserv-frequency.json に文脈共起プリセットを初期化/マージ
  help              ヘルプ表示

Options:
  --listen <LISTEN>                      macSKK が接続するアドレス [default: 127.0.0.1:1178]
  --azookey <AZOOKEY>                    azooKey SKKServ のアドレス [default: 127.0.0.1:1180]
  --yaskkserv2 <YASKKSERV2>              yaskkserv2 のアドレス [default: 127.0.0.1:1179]
  --azookey-timeout-ms <MS>              azooKey 照会のタイムアウト (ms) [default: 1500]
  --yaskkserv2-timeout-ms <MS>           yaskkserv2 照会のタイムアウト (ms) [default: 700]
  --okuri-expansion                      送りあり見出し活用形復元 [default: true] [env: RUSKKSERV_OKURI_EXPANSION]
  --context-ranking                      直前確定単語に基づく文脈共起並び替え [default: true] [env: RUSKKSERV_CONTEXT_RANKING]
  -h, --help                             ヘルプ表示
  -V, --version                          バージョン表示

Subcommand: import-user-dict
Usage: ruskkserv import-user-dict [OPTIONS] <PATH>
  <PATH>                                 macSKK ユーザー辞書パス (skk-jisyo.utf8)
  --send-reload                          インポート成功後に実行中の ruskkserv プロセスへ SIGHUP を送信しホットリロード

Subcommand: init-seed
Usage: ruskkserv init-seed [OPTIONS]
  -f, --force                            既存データをマージせずプリセットで完全上書き
```

---

## プロトコル仕様

| opcode | 動作 | 処理内容 |
| :---: | :--- | :--- |
| `0` | 切断 | セッション終了 |
| `1` | 変換照会 | 送りあり復元 → azooKey 照会 → yaskkserv2 フォールバック → 文脈・頻度整列 |
| `2` | バージョン | `ruskkserv/<VERSION> ` を返却 |
| `3` | ホスト情報 | ホスト情報を返却 |
| `4` | 補完照会 | azooKey / yaskkserv2 に照会して見出し補完リストを返却（文脈保留には影響しない） |
