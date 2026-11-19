# RuSKKserv

macSKK 向けの高速・インテリジェントな skkserv プロキシサーバーです。  
[yaskkserv2](https://github.com/wachikun/yaskkserv2)（Google Suggest 連携・統合辞書）を最優先し、見つからない・応答遅延（200ms 超過時ヘッジ）や失敗時のみ [azooKey SKKServ](https://github.com/gitusp/azoo-key-skkserv) へフォールバックします（`--backend-order=azookey-first` / `RUSKKSERV_BACKEND_ORDER=azookey-first` で従来の azooKey 優先順へ即座に切り替え可能）。

```text
macSKK  →  RuSKKserv (:1178, UTF-8)  →  yaskkserv2 (:1179, デフォルト優先)
                                    →  azoo-key-skkserv (:1180, フォールバック)

※ `--backend-order=yaskkserv2-first`（デフォルト）の動作:
   ・通常変換・補完とも yaskkserv2 を優先照会し、200ms 超過時の並列ヘッジおよびミス／失敗時は
     azoo-key-skkserv へフォールバックします（azooKey 側の送りあり活用復元・送りなし判定も維持）。
   ・yaskkserv2 が返す送りあり角括弧ブロック（例: `/[れ/切/]/[る/伐/.../`）は自動展開したうえで、
     文脈共起・単語頻度・macOS システム辞書優先で並び替えて返却します（単体記号 `[` / `]` は保護）。
   ・SKK の接頭語（`さい>` 等）・接尾語（`>せい`, `>わたs` 等）は引き続き yaskkserv2 を優先照会し、
     未登録時のみ `>` を除いた読みで azoo-key-skkserv へフォールバックします。
```

---

## 主な特徴

1. **参照順切替と送りあり活用形・角括弧ブロック統合（Backend Order & Okuri Expansion）**:
   - デフォルトで `yaskkserv2` 先 → ミス／失敗時 `azoo-key-skkserv` の参照順を採用し、`--backend-order=azookey-first`（または `RUSKKSERV_BACKEND_ORDER=azookey-first`）で従来の azooKey 優先＋yaskkserv2 マージ構成へ無ビルドで切り替え可能。
   - `yaskkserv2` の送りあり角括弧ブロック（`/[れ/切/]/...`）を単体記号候補（`[`, `]`）と厳密区別して展開し、文脈共起・単語頻度・システム辞書優先で再整列。
   - `azoo-key-skkserv` 側（フォールバック時および `azookey-first` 時）では、通常動詞（`かk` → `かく`）・複合語（`ねあg` → 「値上」、`ひきあt` → 「引当」、`とりあつかw` → 「取扱」、`交ぜGk` → 「交ぜ書」等）を全子音・母音で均質に展開し、五十音全段から語幹を抽出。
2. **SKK 仕様に準拠した通常変換・接頭語（`midashi>`）・接尾語（`>midashi`）の厳密分離**:
   - 接頭語（`さい>`→「再」、`ひ>`→「被」「非」、`ちょう>`→「超」等）および接尾語（`>せい`→「性」「制」「製」、`>ひ`→「比」「費」、`>てき`→「的」、送りあり接尾語 `>わたs`→「渡」等）を通常変換と明確に区別し、SKK 辞書を持つ `yaskkserv2` へ直接ルーティング（補完 `4>せ` 等にも対応、未登録時のみ `>` 除去で azooKey へフォールバック）。
   - 送りあり動詞の語幹エイリアス展開（`切る` ↔ `切`）のみを行い、漢字・カタカナ名詞の途中スライスによる接頭断片混入を完全排除。
3. **直前確定単語に基づく文脈連動候補昇格（Context Ranking）**:
   - 「肉」確定後の `きr` → 「切る（切）」、「服」確定後なら「着る（着）」、「木」確定後なら「伐る（伐）」を第1候補に自動昇格。
   - 「時間」→「計る」、「体重」→「量る」、「距離」→「測る」など、478組の文脈共起ペア（基本421キー、送り仮名語幹展開後441キー）を標準サポート。
   - **macSKK 特有の挙動対策**: ▽ モード開始時の過渡的 1 文字 Lookup やキー連射、接頭語・接尾語入力による文脈汚染を自動防御。
4. **macOS システム辞書（大辞林等）・SKK 注釈優先と送りなし断片降格（Annotation Priority）**:
   - 国語辞典の見出し語と一致する標準的な単語や注釈付き単語を優先し、動詞活用断片や助詞混じり断片（`がそ`→`貸そ`、`みぎや`→`右や`、`げそ`→`解そ` 等）を後方に降格（`t` → `う`/`る`、`k` → `い` の終止形復元により促音便・形容詞語幹も確実に保護）。`azookey-first` モードで azooKey が辞書未掲載の誤結合（`せつびご`→`設備後` 等）を返した際も yaskkserv2 候補を自動補完。
5. **macSKK の辞書競合を解消するシームレス統合**:
   - 巨大辞書群（NEologd, Wikipedia 等）を yaskkserv2 に集約し、macSKK との役割分担を最適化。
   - ユーザー辞書の学習順序を冪等に取り込むインポートコマンド（`import-user-dict`）と SIGHUP ホットリロードを完備。
6. **並行クエリプール・インメモリキャッシュ・初回変換耐性・Apple Silicon 最適化**:
   - **接続プール & 並行クエリ・初回変換耐性**: 容量 12 本のノンブロッキング接続プール（失効接続一括破棄＋余剰バッファ残留接続の破棄リカバリ対応）を導入し、送りあり活用展開＋yaskkserv2 並列照会、先頭バックエンド 200ms 超過時の並列ヘッジフォールバック、同期システム辞書 FFI のデッドライン（850ms）内制御＋`block_in_place` 隔離＋単一有界プリウォームワーカー（最大 256 件）に対応。
   - **インメモリ変換キャッシュ (`LookupCache`)**: 容量 4,096 件の高速キャッシュ（`LookupKeyRef` 借用キー照合・`Arc` 共有によるヒット時ゼロコピー・直前確定文脈分離・SIGHUP 自動クリア対応）により、過去に変換した単語や頻出語を **0.1ms 未満（ナノ秒オーダー）** で即答。
   - **ゼロコピー & Apple Silicon 最適化**: パースの借用スライス化、EUC-JP 一括 SIMD デコード（`decode_euc_response_to_utf8` / 2バイト UTF-8 衝突回避）、`encode_euc_jp_cow`、ハイブリッド $O(N)$ 重複排除、Apple Silicon 128バイト キャッシュライン整合（`#[repr(align(128))]`）、`memchr` NEON ベクトル走査、Full LTO、`mimalloc` を完備。
7. **セキュリティと運用の堅牢性**:
   - `yaskkserv2` を `--listen-address 127.0.0.1 --no-daemonize` でループバック限定・フォアグラウンド管理にし、意図しない外部公開とリスポーンループを根絶。
   - 本番 `RUST_LOG` の既定値を `info` とし、入力本文ログは `trace!` + エスケープ出力（`?`）へ隔離。`LOG_DIR` (`0700`)・ログ／seed ファイル（`0600` + 一時ファイル排他・`sync_all`・`rename` によるアトミック保存）・`/usr/bin/pkill -u <uid>` 限定の SIGHUP 送信・plist テンプレートエスケープ（`escape_plist_sed`）を徹底。
   - skkserv リクエストスマグリング防止（見出し語内空白・制御文字拒否）・同時接続数上限（64）・アイドルタイムアウト（600秒）・`--listen` の `SocketAddr` 検証（非ループバック指定時警告）・オーバーフロー安全な `saturating_*` 計算を実装。

---

## クイックスタート

### 1. ビルド & テスト
```sh
cargo build --release  # ./target/release/ruskkserv
cargo test             # 全 82 件のユニット・結合テスト合格を確認
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
macSKK (入力) ─┬─ [opcode 4 (補完)] ─→ 通常見出し: 参照順（デフォルト yaskkserv2 先 → azooKey）で照会
               │                      接頭語・接尾語 (`4>せ` 等): yaskkserv2 (SKK辞書) 直接照会
               │
               └─ [opcode 1 (変換)] ─┬─ 接頭語 (`さい>`)・接尾語 (`>せい`, `>わたs`): yaskkserv2 先 → azooKey
                                     ├─ 通常見出し: yaskkserv2 先（角括弧展開）→ 200ms 超過・ミス時 azooKey（送りあり復元維持）
                                     ├─ 直前確定単語 (session_context) ──→ 共起スコア昇格
                                     └─ システム辞書・注釈照合 & 送りなし断片降格 / マージ
```

> **参照順のロールバック**: 従来の `azoo-key-skkserv` 優先に戻したい場合は、CLI 引数 `--backend-order=azookey-first` または環境変数 `RUSKKSERV_BACKEND_ORDER=azookey-first` を指定してください（CLI 引数が優先されます）。

### ① 送りあり見出しの活用形復元・語幹抽出（Okuri Expansion）と角括弧ブロック展開

デフォルトの `yaskkserv2-first` モードでは、`yaskkserv2` が返す送りあり角括弧ブロック（例: `1/[れ/切/]/[る/伐;木を伐る/着/`）を単体記号候補（`[`, `]`）と誤認せずに自動展開し、文脈共起・単語頻度・システム辞書優先で整列して返却します。`yaskkserv2` に未登録の送りあり見出し・複合語が `azoo-key-skkserv` へフォールバックした場合（または `--backend-order=azookey-first` 時）は、azooKey の語彙を活用した体系的復元が動作します：

1. **全子音・母音の体系的活用合成**:
   語幹 2 文字以上の見出し語に対し、全子音・母音（k, s, t, n, m, r, g, b, u/w, a, i, e, o, d, c, y, j, p）の下一段（え段）、連用形（い段）、終止形（う段）、音便を均質に展開（`ねあg` → `ねあげ`, `ねあぎ`, `ねあぐ`、`ひきあt` → `ひきあて`, `ひきあつ`、`とりあつかw` → `とりあつかい`, `とりあつかう`）。アスタリスク記法（`ねあ*げ`）や大文字送りキー（`ねあG`）も認識します。
2. **azooKey 並行照会と `yaskkserv2` 並列マージ（`azookey-first` 時）**:
   活用バリエーション照会は azooKey に対して並行実行し、`azookey-first` モード時は `yaskkserv2` への元見出し照会も同時に開始して遅延を隠蔽します。各照会 300ms・全体 600ms でキャップし、低遅延を維持します。
3. **五十音全段の語幹抽出 & マージ**:
   返却候補から送り仮名を剥がした語幹に加え、連用形・下一段名詞（「値上」「引当」「申込」「取扱」など）を五十音全段で抽出します。
4. **接頭語・接尾語との厳密分離 (`MidashiKind`)**:
   SKK の接頭語（`さい>`, `ひ>`, `ちょう>` 等）および接尾語（`>せい`, `>ひ`, `>てき`, 送りあり接尾語 `>わたs` 等）は通常変換（`MidashiKind::Normal`）と厳密に分類し、SKK 辞書を持つ `yaskkserv2` へ最優先ルーティングします（ASCII 記号見出し `->`, `<->`, `>=` 等は通常見出しとして維持）。

> **ロールバック**: 送りあり復元を無効化したい場合は、CLI 引数 `--okuri-expansion=false` または環境変数 `RUSKKSERV_OKURI_EXPANSION=0` で即座に切り替えられます（CLI 引数が優先されます）。

### ② 直前確定単語に基づく文脈連動候補昇格（Context Ranking）

直前に確定した単語（漢字を含む単語のみ）を記憶（複数 TCP セッション間で共有・有効期限 60 秒）し、共起辞書に基づいて候補スコアを加算（$\text{Context Score} \times 10$）して第1候補へ昇格させます。

* **macSKK の機械的 1 文字 Lookup 対策**:
  macSKK は ▽ 入力開始時に先頭 1 文字（例: `き`, `さ`）を自動照会します。RuSKKserv は新しい見出し語が保留中見出しの前方一致伸長（`き`→`きr`、`に`→`にく`、`さ`→`さい`）かつ未確定の場合、入力途中の伸長とみなして**文脈昇格せずに安全に破棄**し、真の確定文脈のみを保持します。また、接頭語・接尾語は直前確定文脈を上書きしません。
* **補完連射ガード**:
  macSKK がキー入力中に送信する 50ms 未満の機械的スキャンは文脈登録から自動除外されます。
* **組み込みプリセットへの自動フォールバック**:
  seed ファイル未作成時やデータ不在時でも、バイナリ組み込みの共起プリセット（基本421キー / 478組、送り仮名語幹展開後441キー）がメモリ上で自動適用されます。

> **ロールバック**: 文脈連動を無効化したい場合は、CLI 引数 `--context-ranking=false` または環境変数 `RUSKKSERV_CONTEXT_RANKING=0` で即座に切り替えられます（CLI 引数が優先されます）。

### ③ システム辞書・注釈（annotation）優先機能（Annotation Priority）

macOS のシステム国語辞典（大辞林等）に項目が存在する単語、および SKK 辞書由来の注釈（`;注釈`）が付いている単語にボーナススコア（100点）を付与し、**加点スコア方式**で総合順位を決定します。

* **加点スコア方式（ハイブリッド）と送りなし形状補正**:
  辞書掲載語や注釈付きの一般的な日本語単語をベース 100 点とし、さらに送りなし見出しでは送り仮名・助詞混入のない純粋な名詞・記号候補に形状ボーナス（20 点）を付与することで、動詞活用断片や助詞混じり断片（`がそ`→`貸そ`、`みぎや`→`右や`、`げそ`→`解そ` 等）を確実に後方へ沈めます。また、`DCSCopyTextDefinition` の形態素ステミングによる誤ヒット（`貸そ` が `【貸す】` にマッチする等）は定義ヘッダの見出し語照合により厳密に排除されます。さらに、`azookey-first` モードで azooKey が未検証断片や国語辞典未掲載の誤結合（`せつびご`→`設備後` 等）を第1候補に返した場合、および `frequencies` の最上位登録語（`みぎや`→`→` 等）を欠いている場合は、自動的に `yaskkserv2` の候補を補完マージして再整列します（接頭語・接尾語見出しに対しては SKK 辞書の疑問符注釈 `;?` 等による順位逆転を防ぐため注釈加点を除外）。
* **送りあり（動詞・形容詞）の終止形復元照会**:
  送りあり見出し（例: `かk`, `すばらしi`, `おもt`, `たかk`）の語幹（`書`, `素晴らし`, `思`, `高`）に対しても、送り仮名を補った終止形（`書く`, `素晴らしい`, `思う`, `高い`）を自動合成して大辞林を引くため、動詞・形容詞も角括弧展開後に注釈付き異義候補へ追い越されません。
* **インメモリ FIFO キャッシュ & バックグラウンドプリウォーム**:
  macOS DictionaryServices API (`DCSCopyTextDefinition`) の判定結果を最大 20,000 件の FIFO キャッシュで保持し、マルチスレッドランタイム上では `block_in_place` で非同期ワーカーから隔離。予算超過でスキップした未キャッシュ候補は単一の有界バックグラウンドワーカースレッド（キュー上限 256 件・フォアグラウンド譲歩付き）でキャッシュへ登録します。

> **ロールバック**: 注釈・システム辞書優先を無効化したい場合は、CLI 引数 `--annotation-priority=false` または環境変数 `RUSKKSERV_ANNOTATION_PRIORITY=0` で即座に元の動作へ戻せます（CLI 引数が優先されます）。

### ④ リアルタイム見出し補完（`Request::Completion` / opcode 4）

タイピング中に macSKK から送信される補完要求（opcode 4）も設定された参照順（デフォルト `yaskkserv2` 先 → 200ms 超過・ミス時 `azoo-key-skkserv`、`4>せ` 等の接頭語・接尾語補完は `yaskkserv2` 直接照会）に従って見出し候補リストを高速返却します。補完候補の選択中は `Tab` キーで巡回し、`Enter` で確定できます（補完照会は文脈追跡に影響しません）。

### ⑤ パフォーマンス・ハードウェア最適化 & セキュリティ対策

- **ゼロコピー・ファストパス**: 単一候補やランキング対象外応答はパース・ヒープ確保を完全スキップして直結転送。`LookupCache` は `LookupKeyRef` 借用キー照合と `Arc<[u8]>` / `Arc<str>` 保持によりヒット時のキー確保とコピーをゼロ化。
- **$O(1)$ 送り仮名語幹照合 & $O(N)$ 重複排除**: seed ロード時に文脈キー・候補の双方へ送りあり動詞・形容詞の語幹（「切る」→「切」、「確認する」→「確認」等）のみを事前展開。候補展開・マージ時は 16 件超で `HashSet` を併用するハイブリッド重複排除を使用。
- **EUC-JP 一括 SIMD デコード & リクエスト構築**: `UpstreamEncoding::EucJp` 向けに `decode_euc_response_to_utf8` で走査を最小化しつつ 2 バイト UTF-8 衝突を回避し、`encode_euc_jp_cow` で送信時の中間バッファコピーを削減。
- **PDQsort & Apple Silicon 最適化**: 候補整列に `sort_unstable_by` を採用。`SharedContextState` を 128 バイト境界に配置（`#[repr(align(128))]`）し、`memchr` NEON ベクトル走査、Full LTO、`mimalloc` を採用。
- **プロトコル・接続・権限防御**: skkserv リクエスト内空白・制御文字の拒否（スマグリング防止）、`BufReader` 未読余剰バイト残留接続の破棄、同時接続上限（64）・無通信タイムアウト（600秒）、`--listen` の `SocketAddr` 検証、プライバシ保護ログ（既定 `info`・`LOG_DIR` `0700`・シード/ログ `0600`・アトミック保存）を実装。

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
日常会話・ビジネス・IT・医療・料理など幅広い分野を網羅したプリセット（基本421キー / 478ペア、送り仮名語幹展開後441キー）を安全にマージ・導入できます。

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
  2. 静的辞書（NEologd, Wikipedia 等）は `yaskkserv2_make_dictionary` で 1 つのバイナリ辞書（`~/Documents/SKK/dictionary.yaskkserv2`）に集約し、macSKK の通常ファイル辞書はすべて OFF にする。
  3. 既存のユーザー辞書の並び順は `ruskkserv import-user-dict` で取り込み、語彙は yaskkserv2 辞書に統合する。

### ② 接頭語・接尾語（`>`）と AZIK キーバインドの注意点

1. **接頭語・接尾語専用ファイル辞書 (`SKK-JISYO.affix.utf8`) の併用**:
   macSKK (`2.22.2`) の `SKKServDict` は、接頭語（`さい>`）や接尾語（`>せい`, `>ひ`）の変換時に `>` を除去した見出し（`さい`, `せい`, `ひ`）を `skkserv` に送信する仕様（`option: .prefix` / `.suffix` はローカルのファイル辞書のみが参照）となっています。そのため、`SKK-JISYO.L` から接頭語（`midashi>`）と接尾語（`>midashi`）の 1,185 件のみを抽出した軽量辞書 `SKK-JISYO.affix.utf8`（通常見出し 0 件）のみを **`saveToUserDict = OFF` で有効化**することで、通常変換は 100% RuSKKserv に委ねたまま、macSKK 上での接頭語・接尾語変換を完全動作させられます。
2. **キーバインドの重複回避**:
   AZIK 等で「未確定入力を始めるキー（`action = japanese`、デフォルト `⇧Q`）」を変更する際、「接頭辞・接尾辞の入力（`action = affix`、デフォルト `⇧.` = `>`）」と同じ `⇧.` を割り当てると、macSKK の優先順位により `.japanese` が `.affix` を常時遮蔽してしまいます。「未確定入力を始めるキー」は `⇧[` 等に分離し、`⇧.` (`>`) は「接頭辞・接尾辞の入力」専用としてください。

### ③ 誤爆ゼロ・ストレスフリー構成（プラン A）

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
          azoo-key-skkserv address [default: 127.0.0.1:1180]
      --yaskkserv2 <YASKKSERV2>
          yaskkserv2 address [default: 127.0.0.1:1179]
      --backend-order <BACKEND_ORDER>
          バックエンドの参照順（yaskkserv2-first [デフォルト] / azookey-first; 環境変数 RUSKKSERV_BACKEND_ORDER でも制御可能） [possible values: yaskkserv2-first, azookey-first]
      --azookey-timeout-ms <AZOOKEY_TIMEOUT_MS>
          Timeout for azoo-key-skkserv queries in ms (capped by 850ms overall deadline; 300ms/query in okuri expansion) [default: 600]
      --yaskkserv2-timeout-ms <YASKKSERV2_TIMEOUT_MS>
          Timeout for yaskkserv2 queries in ms (capped by 850ms overall deadline; 200ms in okuri merge) [default: 850]
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
| `1` | 変換照会 | 接頭語・接尾語は `yaskkserv2` 先 → `azooKey` / 通常変換は設定された参照順（既定 `yaskkserv2` 先 → ミス時 `azooKey`・送りあり復元）→ 角括弧展開 & 文脈・頻度・システム辞書整列 |
| `2` | バージョン | `ruskkserv/<VERSION> ` を返却 |
| `3` | ホスト情報 | ホスト情報を返却 |
| `4` | 補完照会 | 接頭語・接尾語補完は `yaskkserv2` 直接 / 通常補完は参照順（既定 `yaskkserv2` 先 → ミス時 `azooKey`）へ照会し見出し補完リストを返却（文脈保留には影響しない） |

---

## ライセンス

[MIT License](LICENSE)
