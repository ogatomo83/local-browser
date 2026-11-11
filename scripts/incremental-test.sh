#!/bin/sh
# 差分取り込みの動きを確かめる (段階 2b)。
#   変化なし → 読まない / touch だけ → ハッシュが同じなので索引は作り直さない /
#   書き換え → 新しいセグメント / 元に戻す → 抽出結果のキャッシュに当たる /
#   削除 → 削除の印 / セグメントが増えたら → まとめ直しと不要な objects の削除
set -eu
LB=${LB:-target/release/lb}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp -R docs/test "$WORK/docs"
DB="$WORK/index.db"
ix() { "$LB" index --db "$DB" "$WORK/docs" 2>/dev/null; }
get() { python3 -c "import json,sys; r=json.loads(sys.argv[1]); print(r['$2'])" "$1"; }
check() { # 名前 実際 期待
  if [ "$2" = "$3" ]; then echo "  ok  $1 = $2"; else echo "  NG  $1 = $2 (期待 $3)"; FAIL=1; fi
}
FAIL=0

echo "1. 最初の取り込み"
r=$(ix); check added "$(get "$r" added)" 18; check segments "$(get "$r" segments)" 1

echo "2. 何も変えずにもう一度"
r=$(ix); check unchanged "$(get "$r" unchanged)" 18; check added "$(get "$r" added)" 0
check generation "$(get "$r" generation)" 1

echo "3. 更新日時だけ変える (touch)"
sleep 1; touch "$WORK/docs/runbooks/deploy.md"
r=$(ix); check same_content "$(get "$r" same_content)" 1; check updated "$(get "$r" updated)" 0
check segments "$(get "$r" segments)" 1

echo "4. 中身を書き換える"
cp "$WORK/docs/runbooks/deploy.md" "$WORK/deploy.bak"
echo "追記: カナリアリリースを試す" >> "$WORK/docs/runbooks/deploy.md"
r=$(ix); check updated "$(get "$r" updated)" 1; check segments "$(get "$r" segments)" 2
check cache_hits "$(get "$r" cache_hits)" 0

echo "5. 元に戻す (抽出結果のキャッシュに当たるはず)"
sleep 1; cp "$WORK/deploy.bak" "$WORK/docs/runbooks/deploy.md"
r=$(ix); check updated "$(get "$r" updated)" 1; check cache_hits "$(get "$r" cache_hits)" 1

echo "6. ファイルを消す"
rm "$WORK/docs/faq.txt"
r=$(ix); check deleted "$(get "$r" deleted)" 1

echo "7. 何度も書き換えて、まとめ直しを起こす"
i=0; merged=False; seg_at_merge=-
while [ $i -lt 9 ]; do
  i=$((i+1)); echo "変更 $i" >> "$WORK/docs/index.md"
  r=$(ix)
  if [ "$(get "$r" merged)" = "True" ]; then merged=True; seg_at_merge=$(get "$r" segments); fi
done
check merged "$merged" True
check segments_at_merge "$seg_at_merge" 1
r=$(ix); check docs_after_merge "$(get "$r" unchanged)" 17
segs=$(get "$r" segments)
if [ "$segs" -le 8 ]; then echo "  ok  segments = $segs (上限 8 以下に保たれている)"; else echo "  NG  segments = $segs"; FAIL=1; fi
objs=$(ls "$DB.store/objects" | wc -l | tr -d ' ')
# まとめ直したセグメント 1 つ + 生きている 17 文書の抽出結果 (それ以降に足したものがあれば多少増える)
if [ "$objs" -le 24 ]; then echo "  ok  objects = $objs (古いセグメントと本文は消えている)"; else echo "  NG  objects = $objs"; FAIL=1; fi

echo "8. 消したファイルが検索に出ないこと (自前の索引)"
cat > "$WORK/q.json" <<'Q'
[{"q": "有給", "mode": "none"}, {"q": "カナリアリリース", "mode": "none"}, {"q": "デプロイ手順", "top": ["runbooks/deploy"], "k": 1}]
Q
if "$LB" eval "$WORK/q.json" --engine segments "$WORK/docs" > "$WORK/eval.txt" 2>&1; then echo "  ok  検索の結果"; else cat "$WORK/eval.txt"; FAIL=1; fi

[ $FAIL -eq 0 ] && echo "すべて ok" || { echo "失敗あり"; exit 1; }
