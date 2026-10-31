# tadoru: 「最初の設問取得」「回答」APIの実装

## Context

`GET /api/units`(ユニット一覧)は動いているが、実際にクイズを解き進めるための2つのAPIがまだ無い。前回の会話で以下は既に決定済み:

- `POST /api/questions/:question_id/answer` + body `{ choice_id }`(回答API)
- `GET /api/units/:unit_id/start`(最初の設問取得API)
- `model.rs`の`from_parts`は使う方針で確定

`src/model.rs`には必要な型(`QuestionRow`/`QuestionResponse`/`ChoiceRow`/`ChoiceResponse`/`AnswerRequest`/`AnswerResponse`/`NextQuestionRow`/`CorrectnessRow`)がすでに用意済み(前回のセッションで追加・承認済み)。今回はこれらを使って`main.rs`側の実装と、動作確認用のシードデータ拡充を行う。

## 変更するファイル

### 1. `src/main.rs` — `ensure_schema()` にシードデータを追加

現状は`unit`テーブルとunitレコード1件しか作っていない(`start_question`も未設定)。`GET /api/units/:id/start`と回答APIを実際に試せるように、`db_demo.rs`と同じQ1→Q2の2問チェーンを本番用の`unit`/`question`/`choice`/`leads_to`テーブルとして追加する。

```
DEFINE TABLE OVERWRITE question SCHEMALESS;
DEFINE TABLE OVERWRITE choice SCHEMALESS;
DEFINE TABLE OVERWRITE leads_to TYPE RELATION FROM choice TO question SCHEMALESS;

UPSERT question:q1 SET body = '原始ピタゴラス数 (a,b,c) において、a,b はどのような偶奇になるか。';
UPSERT choice:q1_a SET question = question:q1, label = 'A', body = '奇数 : 奇数', is_correct = false;
UPSERT choice:q1_b SET question = question:q1, label = 'B', body = '偶数 : 奇数 または 奇数 : 偶数', is_correct = true;

UPSERT question:q2 SET body = 'なぜ「奇数 : 奇数」はありえないのか。';
UPSERT choice:q2_a SET question = question:q2, label = 'A', body = '奇数²は奇数だから', is_correct = false;
UPSERT choice:q2_b SET question = question:q2, label = 'B', body = '奇数²+奇数²は偶数になるが4の倍数にはならないから', is_correct = true;

RELATE choice:q1_b -> leads_to -> question:q2;

UPSERT unit:pythagorean_parity SET
    title = '原始ピタゴラス数の偶奇性を証明する',
    description = 'a, b の偶奇を mod 4 の議論から導く',
    start_question = question:q1;
```

RELATEは既にエッジが存在する状態で再実行すると重複行が増えるため、`ensure_schema`の冒頭で`DELETE leads_to;`してから作り直す(`db_demo.rs`と同じ対処)。

### 2. `src/main.rs` — 共通ヘルパー `fetch_choices`

```rust
async fn fetch_choices(db: &Surreal<Any>, question_id: &RecordId) -> surrealdb::Result<Vec<ChoiceRow>> {
    db.query("SELECT id, label, body FROM choice WHERE question = $question_id ORDER BY label;")
        .bind(("question_id", question_id.clone()))
        .await?
        .take(0)
}
```

### 3. `src/main.rs` — `GET /api/units/{unit_id}/start`

- パスパラメータ`unit_id`は`list_units`が返す形式そのまま(例: `unit:pythagorean_parity`)を受け取る
- `SELECT start_question.id AS id, start_question.body AS body FROM ONLY <record> $unit_id;` で`QuestionRow`を取得(`Option`、無ければ404)
- `fetch_choices`で選択肢を取得
- `QuestionResponse::from_parts(question, choices)`をJSONで返す

### 4. `src/main.rs` — `POST /api/questions/{question_id}/answer`

- body: `AnswerRequest { choice_id }`
- `SELECT is_correct FROM ONLY <record> $choice_id;` → `CorrectnessRow`(無ければ404)
- 不正解なら `AnswerResponse { correct: false, next_question: None, unit_completed: false }` を返して終了
- 正解なら `SELECT ->leads_to->question.* AS next FROM ONLY <record> $choice_id;` → `NextQuestionRow`
  - `next`が空なら `AnswerResponse { correct: true, next_question: None, unit_completed: true }`
  - `next`に要素があれば`fetch_choices`で選択肢も取得し、`AnswerResponse { correct: true, next_question: Some(...), unit_completed: false }`
- パスパラメータ`question_id`は今回は使わない(choice_idだけで判定できるため。将来「choiceが本当にそのquestionに属しているか」を検証したくなったら使う)

### 5. `src/main.rs` — `HttpServer`にサービス登録

`.service(unit_start).service(answer_question)` を追加。

## 検証手順

1. `scripts/start-db.bat`でローカルSurrealDBサーバーが起動しているか確認(無ければ起動)
2. `cargo build` でエラーが無いことを確認
3. `cargo run`でアプリを起動し、以下をcurlで確認:
   - `GET /api/units/unit:pythagorean_parity/start` → `question:q1`の本文と選択肢A/Bが返る(is_correctは含まれないこと)
   - `POST /api/questions/question:q1/answer` body `{"choice_id":"choice:q1_a"}` → `correct:false, next_question:null`
   - `POST /api/questions/question:q1/answer` body `{"choice_id":"choice:q1_b"}` → `correct:true, next_question`に`question:q2`とその選択肢
   - `POST /api/questions/question:q2/answer` body `{"choice_id":"choice:q2_b"}` → `correct:true, next_question:null, unit_completed:true`
4. 確認後、起動したプロセスを`taskkill`で止める
