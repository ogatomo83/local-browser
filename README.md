# Local Browser

ローカルの docs ディレクトリなどを、ブラウザで Google 検索のように探して読むためのツール。
段階 4a まで実装済み (正規化 + 打ち間違いの補正 + 内容ハッシュと不変セグメントによる自前の索引 +
サーバーなしでブラウザだけで動くモード C)。

## 使い方

```sh
make setup                     # 初回だけ: wasm32-wasip2 / wasm32-unknown-unknown ターゲットを入れる
make run DOCS=~/work/docs      # ビルドして起動 → http://localhost:7878/
make ci                        # 単体テスト + ブラウザ用 wasm のビルド確認 + 黄金のクエリ集 (lb とブラウザ用 wasm) + 差分取り込みの確認
```

### モード C: ブラウザだけで使う (サーバーなし)

**Rust も Node もいらない: ビルド済みのアプリ (`web/dist/`) がリポジトリに入っている。**
macOS に最初から入っている python3 で配って、Chrome で開くだけ:

```sh
git clone https://github.com/ogatomo83/local-browser && cd local-browser
make open                      # → http://localhost:8080/ (WEB_PORT=8081 などで変更)
# make がなければ: python3 -m http.server 8080 --bind 127.0.0.1 --directory web/dist
```

(ファイルを直接ダブルクリックで開く file:// では動かない。Worker と wasm の読み込みに http が要るため)

ソースを変えたら `make web` で `web/dist/` を作り直してコミットする。手元でビルドして試すとき:

```sh
make web-serve                 # web/dist/ を組み立てて http://localhost:8080/ で配る (WEB_PORT で変更)
```

`web/dist/` はただの静的ファイル (HTML + JS + 1.3MB の wasm)。どこに置いても動く
(フォルダを選ぶ API は https か localhost でしか動かない)。

1. 「フォルダを選ぶ」でドキュメントのフォルダを選ぶ (読み取り専用)。中身はこのブラウザの中で読むだけで、どこにも送らない
2. 索引はブラウザの IndexedDB に置く。次に開いたときはそこから開くので、すぐ検索できる
3. **変更を取り込むには、ページを再読み込みする。** 更新日時とサイズが変わったファイルだけ読み直す
   (lb と同じ判断。中身の BLAKE3 が同じなら索引は作り直さない)

| ブラウザ | フォルダ | 変更の取り込み |
|---|---|---|
| Chrome / Edge (デスクトップ) | 覚えておける (`showDirectoryPicker`) | 再読み込みだけ。初回の再読み込みで「アクセスを許可」を 1 回押すことがある |
| Firefox / Safari | 覚えておけない (`<input webkitdirectory>`) | フォルダを選び直す。索引は残るので、選び直すまでは前回の内容で検索できる |

読めないフォルダ (許可がない・選び直していない) の文書は、消えた扱いにせず前回のまま残す。
「×」で一覧から外したときだけ消える。

Confluence の模擬データも取り込める (本物のコネクタは段階 5):

```sh
target/release/lb ~/work/docs --confluence fixtures/confluence-dev.json
target/release/lb ~/work/docs --confluence fixtures/confluence-dev.json --scope public   # 閲覧制限付きのページを除く
```

複数のディレクトリを渡せる: `target/release/lb ~/work/docs ~/notes`

- 起動時に索引を作り、その後 30 秒ごとに差分だけ更新する (`--interval`)
- 索引は `~/.local-browser/index.db` と `~/.local-browser/index.db.store/` (`--db` で変更)
- `lb index <dir>` で 1 回だけ更新して結果を JSON で出す
- 対応形式: `.md` `.markdown` `.txt` `.html` `.htm`
- 隠しディレクトリ、`node_modules` `target` `dist` `build` `vendor` は読まない

### アドレスバーから検索する

Chrome の「設定 → 検索エンジン → サイト内検索」に以下を追加すると、
アドレスバーで `d` + スペース + キーワード で直接検索できる。

- ショートカット: `d`
- URL: `http://localhost:7878/search?q=%s`

### キーボード

| キー | 動作 |
|---|---|
| `/`, `⌘K`, `⌘L` | 検索欄へ |
| `Alt+T` / `Alt+W` | タブを開く / 閉じる |
| `Alt+←` / `Alt+→` | 戻る / 進む |
| `Alt+1`…`9`, `Alt+[` `]` | タブ切替 |
| `⌘`/`Ctrl`/中クリック | 新しいタブで開く |

(`⌘T` `⌘W` は本物のブラウザが先に取るので Alt を使う)

## 構成

```
core/               検索の中心部分 lb-core (正規化 / 語の分割 / 補正 / セグメント / 差分取り込みの判断 / 段階的な検索)。純粋な Rust
extract/            文書の読み取り lb-extract (Markdown / HTML / テキスト)。純粋な Rust
extractor/          lb-extract を包む wasm コンポーネント (wasm32-wasip2、wasmtime で隔離)
wit/extractor.wit   抽出部品とホストの契約 (WIT)
host/               本体 `lb`: 取り込み元 / 置き場 / 取り込み / 段階的な検索 / HTTP サーバ / wasmtime
host/ui/index.html  ブラウザ風の画面 (lb のバイナリに埋め込み。モード C も同じ画面を使う)
web/                モード C: lb-web (ブラウザ用 wasm、import なし) と Worker / IndexedDB / フォルダを歩く JS
golden/             黄金のクエリ集 (`lb eval` で回す)
fixtures/           Confluence の模擬データ (ページ ID・版番号・閲覧制限)
scripts/            差分取り込みの確認 / モード C の組み立て・クエリ集 (Node)・通しの確認 (Chrome)
docs/test/          クエリ集が前提にしているテスト文書
sample-docs/        動作確認用の文書
```

core と extract はファイル操作・DB・スレッドを使わない。モード C で同じコードを
wasm32-unknown-unknown にして使うため。何を読み直すか (`core/src/ingest.rs`) と段階的な検索
(`core/src/search.rs`) も core にあり、lb とブラウザで同じ判断をする。`make web-eval` で、ブラウザ用
wasm が lb と同じクエリ集に通ることを毎回確かめる。

モード C の流れ:

```
選んだフォルダ ──(Worker が歩く: 更新日時+サイズだけ)──▶ lb-web.wasm: 読み直すものを決める
      │ 変わったファイルだけ読む                              │ 抽出 → 新しいセグメント → manifest
      ▼                                                      ▼
   バイト列 ────────────────────────────────▶ IndexedDB (objects + manifest、1 トランザクションで書く)
                                                             │
                              画面 (lb と同じ app.js) ◀── Worker が検索に答える
```

lb-web.wasm は import が 1 つもなく、ファイルもネットも触れない。ファイルを読むのは JS (読み取り専用の
許可だけ)、ページは CSP で自分のオリジン以外への通信を止めている。

```
ディレクトリ / Confluence ──版 (mtime+size / 版番号) で足切り → BLAKE3 で本当に変わったか──▶ host
                                          │ バイト列
                                          ▼
                              extractor.wasm (wasmtime)
                              ・ファイル / ネット / 環境変数へのアクセスなし
                              ・燃料 (命令数) とメモリに上限
                                          │ title / headings / body / html
                                          ▼
                    objects/ (中身の名前の不変セグメント + 本文) + manifest.json
                    SQLite docs (画面用の文書。FTS5 は比べ合わせ用に残している)
                                          │
                              axum (127.0.0.1 のみ) ──▶ ブラウザ
```

### 検索

索引にも検索語にも同じ正規化をかける (`core/src/normalize.rs`)。

- NFKC (全角英数字 → 半角、半角カナ → 全角)、小文字化
- カタカナ語の末尾の長音を落とす (サーバー = サーバ)
- 人名でよく揺れる異体字を揃える (髙橋 = 高橋)

検索は段階的に進む (`host/src/search.rs`)。

1. **そのままの語で AND。** 語は字種の切れ目で分ける (`デプロイ手順書` → `デプロイ` `手順書`)。
   「書」「者」などの接尾辞は落として語幹で探し、語の一覧にない漢字の複合語は 2 文字ずつに分ける
2. **0 件なら、打ち間違いを直して AND。** 画面に「k8s の結果を表示しています」と「元の語のまま検索」を出す
3. **それでも 0 件なら、3 文字の断片が半分以上一致する文書。** 「デプロイの手順を教えて」のような話し言葉を拾う

打ち間違いの補正は AI を使わず、索引にある語の一覧 (語 → 文書数) に対して行う (`core/src/query.rs`)。

| 語の種類 | 補正の規則 |
|---|---|
| かな | ひらがな ↔ カタカナを入れ替えて一覧にあれば直す (ろーるばっく → ロールバック) |
| 英数字 (4 文字以上)・かな (3 文字以上) | 8 文字未満は 1 文字違い、8 文字以上は 2 文字違いまで。1 位の候補の文書数が 2 位の 3 倍以上のときだけ自動で直し、そうでなければ「もしかして」にする |
| 2 文字の漢字語 | 1 文字違いの候補のうち、違う字どうしの音読みが同じものだけ自動で直す (検策 → 検索、障碍 → 障害)。読みが違えば「もしかして」だけ (検査 → 検索 は勝手に直さない) |

音読みは Unihan (Unicode, Inc.) の kJapaneseOn から作った表 (`core/data/onyomi.tsv`)。

抜粋と強調は、元の文字列の上で正規化前の位置に戻して作る。
正規化の規則・索引の形・抽出 wasm のどれかが変わったら、起動時に索引を自動で作り直す。

### 索引 (段階 2b)

文書を引くのは lb-core の自前の索引 (`core/src/segment.rs` `index.rs`)。ブラウザでも同じものが動く。

```text
<db>.store/
  manifest.json        目次: セグメントの並び、削除の印、出どころごとの版と中身のハッシュ
  objects/b3-…         セグメントと本文 (抽出結果の JSON)。名前は中身の BLAKE3。書いたら変えない
  cache/extract/b3-…   元のハッシュ + 抽出 wasm のハッシュ + 拡張子 → 抽出結果の名前
```

1. 版 (ファイルは更新日時+サイズ、Confluence は版番号) が前回と同じなら、中身を読まない
2. 違えば BLAKE3 を取る。同じなら版だけ書き換える (touch しただけ)
3. 中身が変わっていれば抽出する。元に戻したファイルはキャッシュに当たり、wasm を呼ばない
4. 変わった文書だけで新しいセグメントを 1 つ足し、消えたものに削除の印を付けて manifest を差し替える
5. セグメントが 8 つを超えるか削除の印が多くなったら、生きている文書だけで 1 つにまとめ直し、
   使われなくなった objects を消す (manifest を差し替えてから消す)

語の引き方は FTS5 (trigram) の部分一致に合わせてある。漢字は 2 文字ずつの出現位置が並ぶところ、
英数字とかなは 3 文字以上ならその語を含む語、2 文字ならその語で始まる語。順位は BM25F
(タイトル 10 / 見出し 4 / 本文 1)。FTS5 と違い、2 文字の語 (検索、DB) にも順位が付く。

各文書には閲覧範囲 (scope) がある。`--scope` を付けると、検索・文書の表示・最近の文書・ファイル配信の
どれでも、範囲外の文書は「ない」ものとして扱う。

**FTS5 との比べ合わせ (2026-11-11)**: 黄金のクエリ集 62 本はどちらも全部通り、検索時間も同程度
(中央値 0.3〜0.4ms、23 文書)。自前の索引はブラウザで動き、2 文字の語に順位が付き、閲覧範囲で絞れる
ので、既定にした。FTS5 は `--engine fts5` で比べられるように段階 3 まで残し、その後消す。
語の途中の一致は辞書を全部なめているので、語が数十万を超えたら作り直しが要る。

### 検索の質の確かめ方

```sh
make eval                                         # golden/docs-test.json を docs/test に対して回す (lb の 2 方式)
make web-eval                                     # 同じクエリ集をブラウザ用 wasm で (Node)
make web-e2e                                      # モード C を本物の Chrome (headless) で通しで動かす
target/release/lb eval my-queries.json ~/work/docs  # 自分のクエリ集で
```

クエリ集には、打ち間違い・表記の揺れ・2 文字の漢字語に加えて、「直してはいけない語」と
「何も出てはいけない語」も入れる。言い換え (切り戻し → ロールバック) は段階 3 の埋め込みで解く予定で、
`"pending": true` を付けて保留扱いにしている。

### 安全面

- 127.0.0.1 にだけ bind し、Host ヘッダが `localhost:PORT` / `127.0.0.1:PORT` 以外なら拒否 (DNS リバインディング対策)
- POST には `x-lb` ヘッダを必須にしている (他サイトからの CSRF 対策)
- Markdown 中の生 HTML はエスケープし、`javascript:` などの URL は無効化
- HTML ファイルはスクリプト無効の sandbox iframe で表示
- 画像などのファイル配信は索引対象のディレクトリ内に限定 (`..` やシンボリックリンクで外に出られない)
- モード C: CSP (`connect-src 'self'`、`frame-src 'none'` ほか)、wasm は import なし、フォルダは読み取り専用。
  文書内のリンク先が文書以外なら、画像と PDF 以外は文字として開く (選んだフォルダの HTML がこのオリジンで動かないように)。
  静的ホスティングに置くときは、同じ CSP をレスポンスヘッダでも付けると Worker にも効く

## この先 (ロードマップ)

設計の詳細は設計 v3 のアーティファクトを参照。

1. ✅ docs + FTS5
2. ✅ 2a: 正規化 + 語の一覧による補正 + 黄金のクエリ集 + ブラウザ用 wasm のビルド確認
3. ✅ 2b: 内容ハッシュ (BLAKE3) + 不変セグメント + manifest + まとめ直し + 閲覧範囲 + Confluence の模擬データ。自前の索引を既定に
4. ✅ 4a: モード C (静的ページ + 選んだフォルダをブラウザが読む + リロードで差分を取り込む)
5. 3: 埋め込み + RRF (言い換えに強くする)
6. 4b: モード B (`lb build` で静的サイトを書き出す、チーム共有)
7. 5: 本物の Confluence + コネクタの WASI 化
