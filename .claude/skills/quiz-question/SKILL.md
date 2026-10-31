---
name: quiz-question
description: Add a new connected quiz question to docs/content/*.md for tadoru, given a keyword/topic or a screenshot of an existing exam question. Use when the user drops a single word, a short topic phrase, or a photo of a quiz/exam question and wants it turned into app content — especially while a file under docs/content/ is open. Never copies an existing exam question's wording verbatim; always rewrites it as an original scenario testing the same underlying concept.
---

# クイズ問題の追加（tadoru content）

`docs/SPEC.md` のデータモデル（`unit` → `question` → `choice`、`leads_to` による連結）に沿った形で、
`docs/content/*.md` にクイズ問題を追記するスキル。ユーザーがキーワード一語だけ、または既存の試験問題のスクリーンショットを渡してくるので、それをアプリのコンテンツ形式に変換する。

## 入力の2パターン

1. **キーワードのみ**（例: 「無作為化比較試験」「武器軟膏」）
   → そのキーワードを題材にした、オリジナルの設問を新規に作成する。
2. **既存の試験問題の画像・引用**（例: 放送大学などの過去問スクリーンショット）
   → **絶対にそのまま書き写さない**（著作権上の理由）。問題が測ろうとしている論点・引っかけの構造だけを抽出し、
     場面設定・数値・選択肢の文言を全く新しく作り直す。固有名詞や独自の言い回しは変える。
     判断に迷う場合は「著作権に配慮してそのまま組み込むのはやめ、独自の設問に書き直す」と一言断ってから作業する。

## 出力フォーマット（既存ファイルのスタイルに合わせる）

各設問は以下の構成をそのまま踏襲する（`docs/content/critical-thinking.md` を参照）:

```markdown
## Q{n}. {タイトル}

{設問本文}

- A. {選択肢}
- B. {選択肢}
- C. **[正解 → Q{next}へ]** {選択肢}
- D. {選択肢}

**フィードバック（誤答時）**
- {誤答ラベル}: {なぜ誤りか、一言}
- ...(正解以外の選択肢すべてに書く)

**解説（正解時）**
{正解の理由の解説}。末尾に次の設問への軽い橋渡し文を添える（例:「次は〜を考える」）。
```

ルール:
- 選択肢ラベルは必ず A/B/C/D の4択。正解は1つだけ、`**[正解 → Q{next}へ]**` で明示する。
- 誤答選択肢すべてに `フィードバック` を書く（`choice.feedback` に相当）。
- `解説` は正解時のみ表示される想定（`question.explanation` に相当）。
- 「誤っているものを選べ」形式の設問を自作する場合、選ぶべき選択肢（＝出題意図上の「誤り」）に `[正解]` を付ける。

## 連結（chain）を優先する

このアプリは DAG 構造の連鎖型クイズなので、独立した単問より **既存の unit の続きに繋げる** ことを優先する。

1. まず対象ファイル（開いているファイル、または `docs/content/critical-thinking.md`）を読み、既存の unit 構成と各設問の論点を把握する。
2. 新しい設問が、既存のどの設問の発展・応用・対比になっているかを考え、差し込み位置を決める。
   - 既存の最終設問（`unit_completed` になっている設問）の後ろに追加するのが最も簡単。
   - 途中に差し込む方が話の流れとして自然な場合は、そこに挿入してよい。ただしその場合は後続の `Q{n}` 番号と
     `→ Q{n}へ` の参照、および `unit` 冒頭の `start_question` を含め、**全て連番になるよう振り直す**。
3. 直前の設問の正解選択肢が指す `→ Q{next}へ` を、新設問の番号に更新する。
4. 新設問の正解選択肢は、次に繋げる設問があれば `→ Q{next+1}へ`、unit の締めくくりなら `→ unit_completed`。
5. どうしても既存の文脈にうまく繋がらない場合のみ、新しい unit として独立させる（無理に繋げない）。

## 進め方

1. 対象ファイルを Read する。
2. 入力がキーワードか画像かを判断し、上記ルールに従って設問本文・選択肢・フィードバック・解説を作成する。
3. 連結先を決め、直前設問の `leads_to` 表記を更新しつつ Edit で追記する（番号がずれる場合は Write で該当範囲を書き直す）。
4. 何を作ったか（新設問の要旨、どこに繋げたか）を短く報告する。長い前置きや設問全文の再掲はしない。
