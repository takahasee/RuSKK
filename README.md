# RuSKK

macSKK 向けの skkserv プロキシです。  
[azoo-key-skkserv](https://github.com/gitusp/azoo-key-skkserv) を優先し、見つからない場合のみ [yaskkserv2](https://github.com/wachikun/yaskkserv2) にフォールバックします。

```
macSKK  →  RuSKK(:1178)  →  azoo-key-skkserv(:1180)   # primary
                         →  yaskkserv2(:1179)         # fallback
```

## 特徴

- 送りあり見出し（`かk`, `きr`, `よm` 等）の平仮名活用形自動復元と語幹抽出（動詞・形容詞の高精度変換）
- 直前確定単語に基づく文脈連動候補昇格（「肉」の後は「切る」、「服」の後は「着る」、「木」の後は「伐る」など）
- 順次フォールバック（azookey → yaskkserv2）— opcode `1`（変換）
- 勝手な確定（`addFixedText`）防止のため、opcode `4`（補完）は常に候補なし（`4\n`）を返却
- 変換候補は seed データ (`~/.ruskk-frequency.json`) で指定した出現頻度・文脈共起で安全に並べ替え
- クライアントへの応答は **UTF-8**
- azoo-key-skkserv の UTF-8 応答はそのまま転送
- yaskkserv2 の EUC-JP 応答は UTF-8 に変換
- skkserv プロトコル `0` / `1` / `2` / `3` / `4` に対応
- **即時切り替え・ロールバック安全機能**: 各機能は環境変数（`RUSKK_OKURI_EXPANSION=0`, `RUSKK_CONTEXT_RANKING=0`）または CLI 引数で即座に無効化可能

## 送りあり見出しの活用形復元・語幹抽出

SKK では動詞の送り仮名の最初の子音/母音をローマ字で入力します（例: `かk` で「書く」）。  
これをそのまま azooKey SKKServ に問い合わせると、「化/家/下/科...」など大量の同音名詞の単漢字が返ってしまい、目的の動詞（「書」）が数十番目に埋没してしまいます。

RuSKK は形態素解析器（MeCab / ChaSen 活用表）の規則に基づき、見出し語を自動判定・復元します：
1. `かk` → `かく`、`きr` → `きる`、`よm` → `よむ`、`あかi` → `あかい` に平仮名復元
2. azooKey に照会して「書く」「描く」などの高精度な候補を取得
3. 末尾の送り仮名（`く`）を取り除き、語幹「書」「描」を抽出（送り仮名を持たない名詞「各」等は自動除外）
4. macSKK に返却し、macSKK 側で「書く」「描く」として画面上に第1候補で表示

### 従来の動作に戻す方法（ロールバック）

何らかの理由で従来の動作（送りあり見出しをそのまま透過照会）に戻したい場合、再ビルド不要で即座に切り替えられます。

- **一時的に無効化**:
  ```sh
  RUSKK_OKURI_EXPANSION=0 ./target/release/ruskk
  # または
  ./target/release/ruskk --okuri-expansion=false
  ```
- **LaunchAgent 運用で無効化**:
  `launchd/config.env` に以下を追記・設定して再インストールします。
  ```sh
  RUSKK_OKURI_EXPANSION=0
  ```
  ```sh
  ./scripts/install-launchd.sh install
  ```

## 候補の並び替えと文脈判定 (Seed ファイル)

skkserv プロトコルの制約上、クライアント（macSKK）側でユーザーが確定した候補をサーバー側で知ることはできません。そのため RuSKK は自動学習を行わず、代わりにユーザーが手動で定義する読み取り専用の seed ファイル (`~/.ruskk-frequency.json`) を使って候補を並び替えます。

`~/.ruskk-frequency.json` 例:
```json
{
  "frequencies": {
    "きr": { "切る": 2, "着る": 2, "伐る": 2 }
  },
  "context_frequencies": {
    "肉": { "切る": 10 },
    "服": { "着る": 10 },
    "木": { "伐る": 10 }
  }
}
```

この例では、「服」と入力した直後に「きr」を変換すると「着る」が、「肉」の直後なら「切る」が第1候補になるようにスコアリングされます。

### 代表的な文脈共起プリセットの導入 (init-seed)

日本語で頻出する代表的な同音異義語・送りあり動詞のコロケーション（共起ペア）を組み込んだプリセットを、ワンコマンドで `~/.ruskk-frequency.json` に安全に生成・マージできます。

```sh
# 既存の個人頻度データを保持したまま、文脈プリセットをマージ
ruskk init-seed

# 完全にプリセットの初期状態に戻す場合
ruskk init-seed --force
```

### macSKK ユーザー辞書からの頻度インポート

macSKK のローカル辞書に蓄積された「最近選択した候補」の情報を `~/.ruskk-frequency.json` に取り込むことができます（`context_frequencies` は維持されたまま、`frequencies` のみが更新されます。文脈共起が空の場合はプリセットも自動注入されます）。

```sh
# フルディスクアクセス権限を持つターミナルから実行してください
ruskk import-user-dict ~/Library/Containers/net.mtgto.inputmethod.macSKK/Data/Documents/Dictionaries/skk-jisyo.utf8
```

※ macSKK のユーザー辞書は `~/Library/Containers` 配下にあるため、ターミナルから手動実行する場合はターミナルに「フルディスクアクセス」権限が必要です。

#### 1時間ごとの完全自動定期インポート (LaunchAgent)

`launchd/config.env` に `MACSKK_USER_DICT_PATH` を指定して `./scripts/install-launchd.sh install` を実行すると、専用のバックグラウンドヘルパー `~/Applications/RuSKKImporter.app` が自動生成され、1時間ごとに macSKK ユーザー辞書から自動インポートされます。

初回のみ、「システム設定」→「プライバシーとセキュリティ」→「フルディスクアクセス」で `~/Applications/RuSKKImporter.app` を追加・許可してください（一度許可すれば、以降の再ビルドや再起動後も権限が保持されます）。

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
./target/release/ruskk
```

オプション:

```text
--listen 127.0.0.1:1178          macSKK が接続するアドレス
--azookey 127.0.0.1:1180         azoo-key-skkserv
--yaskkserv2 127.0.0.1:1179      yaskkserv2
--azookey-timeout-ms 1500        azookey のタイムアウト
--yaskkserv2-timeout-ms 500      yaskkserv2 のタイムアウト
--okuri-expansion <bool>         送りあり見出し活用形復元 (デフォルト: true, 環境変数: RUSKK_OKURI_EXPANSION)
--context-ranking <bool>        直前単語に基づく文脈連動候補昇格 (デフォルト: true, 環境変数: RUSKK_CONTEXT_RANKING)
```

ログレベルは `RUST_LOG` で変更できます（例: `RUST_LOG=debug`）。

## macOS ログイン時の自動起動

LaunchAgent で yaskkserv2 / RuSKK を登録できます。  
azooKey SKKServ は GUI アプリで自動起動する想定のため、LaunchAgent には含めません。

```sh
# 1. 設定ファイルを作成
cp launchd/config.env.example launchd/config.env
# config.env を編集（yaskkserv2 のパスなどを指定）

# 2. リリースビルド
cargo build --release

# 3. 登録・起動
chmod +x scripts/install-launchd.sh scripts/wait-and-run-ruskk.sh
./scripts/install-launchd.sh install
```

yaskkserv2 は次のオプションで起動します:

```text
yaskkserv2 --port 1179 --google-suggest ~/Documents/SKK/dictionary.yaskkserv2
```

ログは `~/Library/Logs/ruskk/` に出力されます。

```sh
# 状態確認
./scripts/install-launchd.sh status

# 停止・削除
./scripts/install-launchd.sh uninstall
```

RuSKK は azooKey SKKServ (:1180) と yaskkserv2 (:1179) の起動を待ってから立ち上がります。

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
| `2` | `ruskk/0.1.0 ` を返却 |
| `3` | ホスト情報を返却 |
| `4` | サーバー補完（両バックエンドを並列照会 → マージ） |

opcode `1` では primary がタイムアウト・接続失敗・`4`（未検出）の場合に fallback を照会します。  
opcode `4` では両方の結果を統合し、seed データによる並び替えを適用してクライアントに返します。
