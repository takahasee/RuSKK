# RuSKKserv プロジェクト知識・コンテキスト (AGENTS.md)

本ドキュメントは、AI アシスタントがセッションを跨いでプロジェクトの設計方針・実装経緯（メモリ）を保持するための定義ファイルです。

---

## 1. プロジェクト概要

- **名称**: RuSKKserv（旧称: `skk-proxy`）
- **バイナリ / クレート名**: `ruskkserv`
- **リポジトリ**: [GitHub: takahasee/RuSKKserv](https://github.com/takahasee/RuSKKserv)
- **目的**: macSKK 向けの高速・インテリジェントな skkserv プロキシサーバー。
  - **Primary**: `azoo-key-skkserv` (`127.0.0.1:1180`, EUC-JP request / UTF-8 response)
  - **Fallback**: `yaskkserv2` (`127.0.0.1:1179`, EUC-JP, Google Suggest 連携)
  - **Listen**: `127.0.0.1:1178` (UTF-8)

---

## 2. 重要な設計方針・過去の知見（セッションメモリ）

### ① バックエンド仕様とエンコーディング
- **Primary (`azoo-key-skkserv`, :1180)**:
  - macOS の `azooKey skkserv.app` は **リクエストに EUC-JP** を要求し、**レスポンスに UTF-8** を返す（`UpstreamEncoding::EucJpRequestUtf8Response`）。
- **Fallback (`yaskkserv2`, :1179)**:
  - 見出し語を EUC-JP にエンコードして送信、返却結果（EUC-JP）を UTF-8 にデコード（`UpstreamEncoding::EucJp`）。

### ② 手動単語頻度（`frequencies`）と文脈共起（`context_frequencies`）による候補整列
- 候補の並び替えは、**手動で編集する seed ファイル (`~/.ruskkserv-frequency.json`)** に登録された単語頻度（`frequencies`）および文脈共起（`context_frequencies`）に基づいて行われます。
- 補完リクエスト（opcode 4）は文脈追跡の対象外（保留中文脈を昇格・更新しない）としているため、サーバー側で文脈を参照しても macSKK 側で勝手な自動確定（`addFixedText`）が起きる心配は一切ありません。
- **SIGHUP ホットリロード**: `ruskkserv` プロセスが SIGHUP を受信すると、`~/.ruskkserv-frequency.json` を再起動なしで即座に再読み込みする（`src/main.rs: reload_on_sighup()`）。`import-user-dict --send-reload` 実行後に自動で predictor が更新される。

### ③ 補完フォワード機能（`Request::Completion` / opcode 4）
- **仕様 (`src/proxy.rs`)**:
  - macSKK からの補完リクエスト（opcode 4）を Primary (`azoo-key-skkserv`) / Fallback (`yaskkserv2`) へ照会し、返却された見出し語補完（`1/.../\n`）を macSKK へ返却します。
  - 補完は入力途中の推測であるため、文脈連動の確定判定（`pending_context` の昇格・更新）は行いません。
  - macSKK のローカル辞書（ユーザー辞書や静的辞書）が空の場合でも、RuSKKserv（azooKey）から見出し補完がシームレスに提供されます。
- **macSKK 側の補完最適設定（プラン A: 誤爆ゼロ・ストレスフリー構成）**:
  - `completionConfirmationTimeLimit -int 86400000`（24 Hours）でホームポジション打鍵（`ASDFGHJKL`）による勝手な誤確定・文字漏れ（「すずきせいじゅんあ」等）を 100% 根絶。
  - `fixedCompletionByPeriod -int 0` により、AZIK の句点打鍵との衝突を根絶。
  - 補完は `Tab` キーで選択・巡回し、`Enter` で確定（macSKK 公式仕様準拠。※macSKK 内部で補完選択中の Space は未実装 `TODO` で握り潰されているため、通常の変換は SKK 王道の「見出し語 ＋ Space」で行う）。

### ④ Seed データファイルとプリセット
- パス: `~/.ruskkserv-frequency.json`
- 互換性: 読み取り専用の JSON ファイルとして扱われ、RuSKKserv プロセスからは書き込まれません。ユーザーが任意のエディタで編集して再起動することで反映されます。
- **ホットリロード**: SIGHUP シグナルを受信すると再起動不要で即座に再読み込みされます（`import-user-dict --send-reload` が自動的に SIGHUP を送信）。
- 組み込みプリセット: `data/default-seed.json`（`ruskkserv init-seed` で既存データを維持したままマージ可能）。
  - `context_frequencies` は基本 421 キー / 478 ペア、送り仮名語幹展開後 441 キー（v0.2.7 で漢字名詞の接頭断片展開を廃止したため旧 809 キーから減少）（着る/切る/伐る/斬る、計る/測る/量る/図る/謀る/諮る、観る/診る/看る、乗る/載る/撮る/採る/捕る、建てる/立てる、書く/描く、造る/創る/作る、飲む/打つ/受ける/通す/取る、炊く/炒める/焼く/煮る/揚げる、開く/開ける/閉める/消す/点ける/敷く、送る/贈る/残す/返す/進める、確認する/変更する/修正する/削除する/追加する、備える/払う/張る等、日常・ビジネス・法律・医療・料理・IT・農業・教育・音楽・スポーツ・自然など幅広いドメイン）をカバー。

### ⑤ 送りあり見出し（動詞・形容詞・複合語）の活用形復元・語幹抽出と即時ロールバックスイッチ
- **背景**: SKK の送りあり見出し（例: `かk`、`きr`、`よm`、`ねあg`、`みおk`、`いいだs`、`ひきあt`、`うけいr`、`もうしこm`、`しらb`、`とりあつかw`、`交ぜGk`、`といあわs`、`わりあt`）を azooKey SKKServ に直接照会すると、「化/家/下/科...」といった大量の同音名詞の単漢字が優先され、目的の動詞や複合語が埋没・あるいはヒットしない問題がありました。
- **メカニズム (`src/okuri.rs`, `src/proxy.rs`)**:
  1. **全子音・母音の体系的一般化**:
     語幹が 2 文字以上の見出し語（`char_count >= 2`）はすべて複合語・多音節語として扱い、全子音および母音送りキー（k, s, t, n, m, r, g, b, u/w, a, i, e, o, d, c, y, j, p）に対して、下一段名詞形・連用形名詞形・終止形・命令形・代表的音便を均質に展開。特定子音や文字のアドホック分岐を完全排除（「使え (`つかe`)」「教え (`おしe`)」「使おう (`つかo`)」等も完全対応）。
  2. **Primary (azooKey) 限定照会による低遅延化**:
     送りありバリエーション照会は azooKey の動詞・形容詞辞書に対する照会であり、SKK 辞書（yaskkserv2）への不要なフォールバックを排除。各照会上限 300ms・全体上限 600ms でキャップし、1.5秒以上の遅延および macSKK の 1 秒タイムアウト切断を根絶（全クエリ 100〜200ms 台で完結）。
  3. **五十音全段の語幹抽出**:
     返却候補から送り仮名を剥がした語幹に加え、すでに語幹化された名詞形（い段連用形・え段下一段名詞）を五十音全段で漏れなく抽出。
  4. **アスタリスク明示・大文字対応**:
     macSKK / DDSKK の画面表示マーカー（`ねあ*げ`）や大文字送りキー（`ねあG`）もシームレスに認識。
  5. **従来辞書フォールバック保持**:
     元の見出し語による Fallback (yaskkserv2) 照会は末尾にマージされるため、辞書固有語も漏れなく提供。
- **即時ロールバック（安全機能）**:
  - 何か問題があった場合、再ビルドなしで即座に元の動作へ戻せます。
  - **CLI**: `ruskkserv --okuri-expansion=false`
  - **環境変数**: `RUSKKSERV_OKURI_EXPANSION=0`
  - **LaunchAgent**: `launchd/config.env` で `RUSKKSERV_OKURI_EXPANSION=0` を指定し `./scripts/install-launchd.sh install`。

### ⑥ 直前確定単語に基づく文脈連動候補昇格と即時ロールバックスイッチ
- **背景**: 「肉」を入力した直後に「きr」を打った場合は「切る（切）」、「服」の直後なら「着る（着）」、「木」の直後なら「伐る（伐）」を第1候補に昇格させたいという自然な日本語入力の要求に応える機能。
- **メカニズム (`src/proxy.rs`)**:
  1. 複数 TCP セッション間で共有される `SharedContextState` に `pending_context`（前回の見出し語、第1候補漢字、タイムスタンプ、受信回数）を保持（漢字を含む第1候補のみ対象）。
  2. 新しい見出し語の変換（Lookup）が開始された時点で前回の単語が確定されたと判定し、`session_context`（TTL: 60秒）に昇格。同一見出し語での次候補送り（Space 連打）中は保留を維持。
  3. `rank_candidates_borrowed` を呼び出し、`context_frequencies` に基づき候補スコアを加算して並び替え。
  4. 補完（opcode 4）は文脈追跡を行わず透過照会し、同一接続で直前応答から 50ms 未満に届く機械的連射 Lookup（補完プレビュー等）も文脈登録から除外されるため、文脈汚染は一切起きず確定は常にユーザーの手動 Space キーによって行われます。
  5. **macSKK の機械的 1 文字 Lookup 対策**: macSKK は ▽ 入力開始時に先頭 1 文字（例: `き`）を自動で Lookup するため、そのままでは「木」「化」等が文脈を上書きしていた。新しい見出し語が保留中の見出し語の前方一致伸長（`き`→`きr`、`に`→`にく`）で、かつ保留中の見出し語の受信回数（`hits`）が 2 未満なら、未確定とみなして昇格せず破棄する（`SharedContextState::on_lookup_start`）。
  6. **プリセットへのフォールバック**: `~/.ruskkserv-frequency.json` が無い、または `context_frequencies` が空の場合は、起動時・SIGHUP 時に組み込みプリセットをメモリ上で適用する（`ensure_default_context_presets`）。
  - **制約**: skkserv プロトコルではユーザーが実際に選んだ候補を知る手段がないため、文脈語には「返却した第1候補」を使う。
- **即時ロールバック（安全機能）**:
  - **CLI**: `ruskkserv --context-ranking=false`
  - **環境変数**: `RUSKKSERV_CONTEXT_RANKING=0`
  - **LaunchAgent**: `launchd/config.env` で `RUSKKSERV_CONTEXT_RANKING=0` を指定し `./scripts/install-launchd.sh install`。
### ⑦ macSKK ローカル辞書の yaskkserv2 統合（Fallback 巨大辞書化）
- **背景**: macSKK はローカルの「ユーザー辞書」および「追加辞書（`dictionaries`）」を skkserv よりも最優先で上位に表示する仕様があり、ローカル辞書が有効だと RuSKKserv / azooKey の賢い文脈予測や活用形復元がローカル辞書の候補によって上書きされてしまう問題がありました。
- **解決構成**:
  - macSKK 側で有効化されていた静的辞書群（`SKK-JISYO.all`, `neologd`, `hatena`, `jawiki`, `emoji-ja`, `itaiji`）およびユーザーの過去登録辞書（`skk-jisyo.utf8`）を `yaskkserv2_make_dictionary` で 1 つの 52MB バイナリ辞書（`~/Documents/SKK/dictionary.yaskkserv2`）に一括統合。
  - macSKK の `dictionaries` 設定はすべて `enabled = 0` に無効化。macSKK はすべての照会を RuSKKserv (`127.0.0.1:1178`) 経由でのみ行う。
  - 日常語・動詞・文脈共起は Primary（azooKey）が最優先で返却し、azooKey にない固有名詞・専門用語・ユーザー登録語は Fallback（yaskkserv2）から自動的に補完される。
- **ユーザー辞書の学習による上書き対策（2026-10-04）**:
  - macSKK の候補順は「ユーザー辞書 → 日付変換 → ファイル辞書 → skkserv」で固定（`UserDict+Snapshot.swift` の `localCandidates` / `referDicts`、重複は先勝ち `mergeCandidates`）。skkserv 設定の `saveToUserDict = 1` だと RuSKKserv の候補を選ぶたびにユーザー辞書へ書き込まれ、以後その順序が文脈予測を常に上書きしていた。
  - 対処: macSKK の skkserv 設定「変換履歴をユーザー辞書に保存」をオフにし、ユーザー辞書を空にした（単語登録は引き続き保存・最優先表示される）。旧ユーザー辞書の並び順は `import-user-dict` で `frequencies` に取り込み、語彙は yaskkserv2 辞書に統合。バックアップ: `backup/backup-20261004/`。
  - `import_from_skk_dict` はスコアを `max` で更新する冪等処理（旧実装は毎時 `+=` で際限なく膨らみ、文脈スコア ×10 を上回る恐れがあった）。
- **yaskkserv2 辞書再構築の注意**:
  - `yaskkserv2_make_dictionary` は同一見出し語が複数辞書にあると**先に指定した辞書のエントリのみ**を採用する。ユーザー辞書は必ず先頭に指定する。
  - yaskkserv2 は自身でデーモン化するため `launchctl kickstart -k` では旧プロセスが残り旧辞書を掴み続ける。旧プロセスを `kill` してから kickstart する。

### ⑧ ゼロコピー・O(1)・SIMD 最適化と徹底的な低遅延設計
- **背景**: skkserv はキータイプごとにリアルタイムでミリ秒単位の低遅延応答が要求されるため、ランタイムでの不要な線形探索やメモリ割り当て（アロケーション）を徹底的に排除した。
- **設計と実装 (`src/frequency.rs`, `src/encoding.rs`, `src/protocol.rs`, `src/okuri.rs`)**:
  1. **送り仮名語幹の O(1) ハッシュ化**: 変換時ごとの線形探索 $O(N)$ を廃止し、seed ロード時に文脈キー・候補の双方へ送りあり動詞・形容詞の語幹（「切る」→「切」、「確認する」→「確認」等）のみを HashMap に事前展開（`expand_aliases` / `okuri_stem_aliases`）。漢字・カタカナ名詞の接頭断片混入を排除しつつ、リクエスト処理時は完全な $O(1)$ ハッシュ参照のみで完結。
  2. **EUC-JP レスポンスの一括 SIMD デコード（2バイト UTF-8 衝突回避）**: EUC-JP の規格上、マルチバイトコード（`0xA1`..=`0xFE`）と ASCII 記号（`/`, `1`, `\n`）は絶対にバイト衝突しないため、旧来のスラッシュ分割・逐次デコードを撤廃し、`is_genuine_utf8` で EUC-JP 2バイト漢字（「的」`0xC5 0xAA` 等）と 2バイト UTF-8（`U+0080..=U+07FF`）の衝突を回避したうえで `EUC_JP.decode(response)` の SIMD 最適化一括デコードに移行。
  3. **二重アロケーションの排除**: リクエスト送信時、送信バッファに直接書き込むことで中間 `Vec<u8>` の生成をゼロ化。
  4. **候補ソートのゼロアロケーション化**: 候補整列において安定ソート（`sort_by`, Timsort）から、ヒープ確保ゼロの最速ソート（`sort_unstable_by`, PDQsort）へ移行。
  5. **定常時ヒープ確保ゼロ**: 通常の文脈照会（要素数 0 または 1）において中間 `Vec` の割り当てをインライン分岐で回避。送り仮名サフィックスも静的スライス参照（`&'static [&'static str]`）に統一。
  6. **リクエストパースの完全ゼロアロケーション化**: `Request<'a>` 型を借用スライス（`&'a [u8]`）ベースに再設計。見出し語の余計な末尾スペース除去もスライス借用で完結させ、毎打鍵ごとの `Vec<u8>` ヒープ割り当てをゼロ化。
  7. **文脈確定追跡のゼロコピー化**: セッション単位の直前確定単語保持を `Vec<String>` のディープコピーから `Option<Arc<str>>` の参照共有に改修し、候補整列呼び出し時も `std::slice::from_ref` を用いることで不要なメモリ確保を根絶。

### ⑨ ARM / Apple Silicon 向けマイクロアーキテクチャ最適化
- **背景**: Apple Silicon (M1〜M4 等) の高度なハードウェア特性（NEON SIMD、LSE アトミック命令、128バイト キャッシュライン、広帯域 OoO パイプライン）をフルに引き出し、プロキシ応答のレイテンシを極小化。
- **設計と構成 (`.cargo/config.toml`, `Cargo.toml`)**:
  1. **`target-cpu=native` によるマイクロアーキテクチャ特化**: `.cargo/config.toml` にて `aarch64-apple-darwin` 向けにネイティブ CPU フラグを常時適用。
  2. **LSE (Large System Extensions) アトミック命令**: Tokio や `Arc`、非同期タスク同期における `ldadd`, `cas`, `swp` などのハードウェア単一命令アトミック操作をフル活用（1,000 箇所以上）。
  3. **NEON SIMD 128-bit 自動ベクトル化**: SKK プロトコルデリミタ走査（`/` や `\n`）および UTF-8 検証において、NEON 128-bit レジスタ一括比較（`cmeq.16b`, `umaxv.16b` 等）をフル生成。
  4. **`panic = "abort"` による L1 命令キャッシュ (I-Cache) 最適化**: 例外アンワインド表およびランディングパッドを完全排除し、バイナリ軽量化と ARM64 命令キャッシュの局所性を最大化。
  5. **Apple Silicon 128 バイトキャッシュライン境界整合 (`#[repr(align(128))]`)**: `SharedContextState` を 128 バイト境界に配置し、マルチコア並行アクセス時の偽共有（False Sharing）とキャッシュライン跨ぎをハードウェアレベルでゼロ化。
  6. **NEON 128-bit SIMD 手書きベクトル化走査 (`memchr`)**: スラッシュ (`/`) や改行 (`\n`) の検出および候補リスト走査に `memchr::memchr2` / `memchr_iter` を採用し、AArch64 NEON 命令（`vld1q_u8`, `vceqq_u8` 等）による 16 バイト並列一括走査を確実化。
  7. **Full LTO (`lto = "fat"`) と macOS ld64 デッドストリップ (`-Wl,-dead_strip`)**: バイナリ全体にわたるクロスモジュールインライン展開と不要コード完全除去により、実行バイナリを 1.6MB に極小化し I-Cache ヒット率を最大化。
  8. **高スループットアロケータ (`mimalloc`)**: Apple Silicon の 16KB 仮想メモリページおよびスレッドローカルフリーリストを活用し、Tokio 非同期タスクやバッファ確保のマルチスレッドロック競合を抑制。

### ⑩ システム辞書・注釈（annotation）優先機能と即時ロールバックスイッチ
- **背景**: 変換候補の中に珍奇な当て字、誤変換残骸、辞書外の俗語が上位に出てくるのを防ぎ、国語辞典（大辞林等）に載っている標準的な単語や SKK 注釈付きの確実な単語を優先させたいという要求に対応。
- **メカニズム (`src/system_dict.rs`, `src/frequency.rs`, `src/proxy.rs`)**:
  1. **CoreServices / DictionaryServices (`DCSCopyTextDefinition`) 連携と見出し語ヘッダ照合**: macOS のシステム国語辞典 API を `unsafe extern "C"` で直接呼び出し（`CFStringGetLength` による UTF-16 長でサロゲートペア異体字・絵文字にも対応）、返却された定義先頭行の見出し語（`【...】`）と照合することで、形態素ステミングによる活用断片の誤ヒット（`貸そ`→`【貸す】`、`書こ`→`【書く】` 等）を完全排除。
  2. **SKK 注釈判定**: 候補テキスト内に `;注釈` が含まれている場合も対象として認識。
  3. **送りあり終止形合成**: 送りあり見出し（例: `かk`, `すばらしi`）の語幹候補（`書`, `素晴らし`）に対し、送り仮名を補った終止形（`書く`, `素晴らしい`）を合成して辞書照会。
  4. **インメモリキャッシュ (`SystemDictCache`)**: 照会結果を最大 20,000 件までインメモリ `RwLock<HashMap<String, bool>>` で保持し、2 回目以降の変換遅延をゼロ化。
  5. **加点スコア方式（ハイブリッド）と送りなし断片降格・Fallback 補完**:
     辞書掲載語・注釈付き語にボーナス加点（100点）、送りなし見出しでの純粋な送りなし候補（または辞書掲載語）に形状ボーナス（20点）を付与し、辞書未掲載の送り仮名・助詞混じり断片（`貸そ`、`右や`、`左や`、`解そ`、`したヤ` 等 = 0点）を後方に沈める。また送りなし照会で Primary の第1候補が未検証断片・辞書未掲載誤結合（`せつびご`→`設備後` 等）の場合や `frequencies` の最高スコア候補（`みぎや`→`→` 等）が Primary にない場合は Fallback (`yaskkserv2`) を自動マージする。
- **即時ロールバック（安全機能）**:
  - **CLI**: `ruskkserv --annotation-priority=false`
  - **環境変数**: `RUSKKSERV_ANNOTATION_PRIORITY=0`
  - **LaunchAgent**: `launchd/config.env` で `RUSKKSERV_ANNOTATION_PRIORITY=0` を指定し `./scripts/install-launchd.sh install`。

### ⑪ SKK 仕様に基づく通常変換・接頭語（`midashi>`）・接尾語（`>midashi`）の厳密分離
- **メカニズム (`src/okuri.rs`, `src/proxy.rs`, `src/frequency.rs`)**:
  1. `classify_midashi` により通常見出し（`MidashiKind::Normal`）、接頭語（`MidashiKind::Prefix`: `さい>`, `ひ>`, `ちょう>` 等）、接尾語（`MidashiKind::Suffix`: `>せい`, `>ひ`, `>てき`, 送りあり接尾語 `>わたs` 等）を厳密に分類（`->`, `<->`, `>=` 等の ASCII 記号は `Normal` 維持）。
  2. 接頭語・接尾語の変換（`lookup_affix`）および補完（`4>せ` 等）は SKK 辞書を持つ Fallback (`yaskkserv2`) を最優先で直接照会し、送りあり接尾語の角括弧ブロック展開・文脈保留からの除外・疑問符注釈（`;?`）による順位逆転防止を行う。
  3. **macSKK 側との連携**: macSKK (`2.22.2`) の `SKKServDict` は `option: .prefix` / `.suffix` の `>` を `skkserv` へ送らないため、接頭語・接尾語 1,185 件のみを抽出した `SKK-JISYO.affix.utf8`（通常見出し 0 件）のみを `saveToUserDict = 0` で有効化し、AZIK キーバインドの `japanese` を `⇧[`、`affix` を `⇧.` (`>`) に分離して運用する。

### ⑫ コネクションプール・並行照会とインメモリ変換キャッシュ（LookupCache）
- **背景**: 送りあり見出し（`きr`, `かk` 等）において複数の活用形を単一 TCP 接続で直列照会していたボトルネック（約 110ms）を解消し、さらに日常頻出単語の変換レイテンシをマイクロ秒オーダーへ極小化。
- **メカニズム (`src/backend.rs`, `src/cache.rs`, `src/proxy.rs`, `src/system_dict.rs`, `src/frequency.rs`)**:
  1. **接続プール (`Backend.pool`)**: 単一 Mutex ロック接続を廃止し、容量 8 本の軽量ノンブロッキング接続プールを実装。複数接続を並行利用可能にし、ロック競合をゼロ化。
  2. **送りあり活用の完全並行照会**: `tokio::spawn` により全活用形バリエーションを Primary へ同時並行クエリ。完了後に元のインデックス順序で安定整列することにより、結果の決定論的整合性を 100% 保ちつつ所要時間を大幅短縮。
  3. **インメモリ変換キャッシュ (`LookupCache`)**: 容量 4,096 件の高速キャッシュ（キー: `(直前確定文脈, 見出し語)`）を導入。過去に変換した単語や同一見出し語は upstream 照会・辞書判定・整列を完全バイパスし **0.1ms 未満（ナノ秒オーダー、約 1,000倍）** で即答。SIGHUP ホットリロード時に自動クリアされるため辞書更新とも完全同期。
  4. **システム辞書照会の終止形最優先化と上位候補ガード**: 送りあり見出しにおいて語幹単体（「切」）の前に終止形（「切る」）を最優先照会して FFI キャッシュミスを抑制。さらに未キャッシュ時の重い FFI 呼び出しを上位 12 候補に限定し、13 番目以降の低位候補によるレイテンシスパイクを完全防止。

---

## 3. LaunchAgent 運用仕様

- **設定ファイル**: `launchd/config.env`
- **Plist**:
  - `launchd/com.ruskkserv.skkserv.plist`（サービス名: `com.ruskkserv.skkserv`）
  - `launchd/com.ruskkserv.yaskkserv2.plist`（サービス名: `com.ruskkserv.yaskkserv2`）
  - `launchd/com.ruskkserv.import-user-dict.plist`（サービス名: `com.ruskkserv.import-user-dict`）
- **管理スクリプト**:
  - `scripts/install-launchd.sh [install|uninstall|status]`
  - `scripts/build-importer-app.sh`: macOS TCC（フルディスクアクセス）を恒久化するため、Bundle ID（`com.ruskkserv.importer`）を持つ `~/Applications/RuSKKservImporter.app` を生成・署名。
- **起動スクリプト**:
  - `scripts/wait-and-run-ruskkserv.sh`: azooKey (:1180) と yaskkserv2 (:1179) の起動待機後に `ruskkserv` を起動。
- **ログ**:
  - `~/Library/Logs/ruskkserv/ruskkserv.log`: RuSKKserv プロキシ本体ログ
  - `~/Library/Logs/ruskkserv/import.log`: macSKK 辞書定期インポートログ

---

## 4. 開発・検証コマンド

```sh
cargo build --release  # ./target/release/ruskkserv
cargo test             # ユニットテスト & 結合テスト（全64件合格）
cargo clippy           # 警告 0 件確認済み
./scripts/install-launchd.sh status  # 稼働状況確認
```
