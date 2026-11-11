WASM := target/wasm32-wasip2/release/extractor.wasm
BIN  := target/release/lb
PORT ?= 7878
DOCS ?= sample-docs
GOLDEN ?= golden/docs-test.json
GOLDEN_DOCS ?= docs/test --confluence fixtures/confluence-dev.json

.PHONY: build wasm host run clean setup test check-wasm eval incremental ci web web-serve web-eval web-e2e
WEB_WASM := target/wasm32-unknown-unknown/release/lb_web.wasm
WEB_PORT ?= 8080

build: wasm host
	cp $(WASM) target/release/extractor.wasm

setup:
	rustup target add wasm32-wasip2 wasm32-unknown-unknown

wasm:
	cargo build -p extractor --target wasm32-wasip2 --release

host:
	cargo build --release

run: build
	$(BIN) --port $(PORT) $(DOCS)

# 検索コアと抽出の単体テスト
test:
	cargo test -p lb-core -p lb-extract -p lb-web

# 検索コアと抽出がブラウザ用 wasm にビルドできるか (ファイル操作やスレッドが紛れ込んでいないか)
check-wasm:
	cargo build -p lb-core -p lb-extract --target wasm32-unknown-unknown --release

# 黄金のクエリ集で検索の質を確かめる
eval: build
	$(BIN) eval $(GOLDEN) --engine segments $(GOLDEN_DOCS)
	$(BIN) eval $(GOLDEN) --engine fts5 $(GOLDEN_DOCS)

# 差分取り込み (ハッシュ / キャッシュ / 削除の印 / まとめ直し) の動き
incremental: build
	./scripts/incremental-test.sh

# モード C: 静的ページ (web/dist/)。フォルダを選ぶだけで、ブラウザの中だけで検索する
web:
	cargo build -p lb-web --target wasm32-unknown-unknown --release
	node scripts/build-web.mjs $(WEB_WASM)

# 手元で試す (showDirectoryPicker は localhost か https でしか動かない)
web-serve: web
	@echo "http://localhost:$(WEB_PORT)/"
	python3 -m http.server $(WEB_PORT) --bind 127.0.0.1 --directory web/dist

# 黄金のクエリ集を、ブラウザ用 wasm で回す (Node)
web-eval: web
	node scripts/web-eval.mjs $(GOLDEN) docs/test fixtures/confluence-dev.json

# 本物の Chrome (headless) で通しで動かす (Chrome が要るので ci には入れない)
web-e2e: web
	node scripts/web-e2e.mjs

ci: test check-wasm eval incremental web-eval

clean:
	cargo clean
