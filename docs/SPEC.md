# tadoru 仕様書（暫定）

前提知識を段階的に踏まえないと正解にたどり着けない、連鎖型の一問一答クイズアプリ。

- フロントエンド: React
- バックエンド: Rust (actix-web)
- DB: SurrealDB（本番は Surreal Cloud の無料枠。ORMは使わず `surrealdb` クレートで SurrealQL を直接書く）

この文書は暫定版であり、実装を進めながら随時更新する。

## 1. コンセプト

1つの `unit`（テーマ、例:「原始ピタゴラス数の偶奇性を証明する」）は複数の `question`（設問）から成る。
各 `question` には複数の `choice`（選択肢）があり、選んだ選択肢によって次に表示される設問が変わる。

- 正解を選ぶと、その内容をさらに深掘りする次の設問へ進む
- 誤答した場合はフィードバックを見せて再挑戦させる（設計上は誤答用の遷移先を持たせることも可能）
- 最終設問の正解選択肢まで到達すると unit 完了

グラフ構造（DAG）なので、複数の経路が同じ設問に合流することも許容する。

未回答の設問をクライアントに先読みさせないため、**サーバーは「現在の設問と選択肢」のみを返し、正解や次の設問はレスポンスとして都度返す**。全設問を最初に一括で渡すことはしない。

## 2. データベース設計（SurrealDB）

`choice`（選択肢）から `question`（次の設問）への遷移だけをグラフエッジ（`RELATE`）で表現する。それ以外（unit と question の親子関係、attempt 周り）は SurrealDB のレコードリンク（`record<table>` 型フィールド、SQL の外部キーに近いもの）で十分なのでエッジにはしない。

エッジにした理由: 「次の設問があるか」は選択肢ごとに違い、無いことも普通にある（誤答で終端、など）。RDB案では `next_question_id` を NULL 許容カラムにしていたが、グラフだと「エッジが無い」がそのまま「次が無い」を表すので不自然な NULL 分岐が要らない。

```
unit
  ├─ start_question ─→ question（レコードリンク）
question
  ├─ unit ─→ unit（レコードリンク、親テーマ）
choice
  ├─ question ─→ question（レコードリンク、所属する設問）
  └─ (leads_to) ─→ question（グラフエッジ。正解/誤答時の遷移先。無ければ終端 or 再挑戦）
attempt
  ├─ user, unit, current_question（レコードリンク）
attempt_answer
  ├─ attempt, question, choice（レコードリンク）
```

### スキーマ定義（SurrealQL）

```surql
DEFINE TABLE unit SCHEMAFULL;
DEFINE FIELD title           ON unit TYPE string;
DEFINE FIELD description     ON unit TYPE option<string>;
DEFINE FIELD start_question  ON unit TYPE option<record<question>>;
DEFINE FIELD created_at      ON unit TYPE datetime DEFAULT time::now();

DEFINE TABLE question SCHEMAFULL;
DEFINE FIELD body         ON question TYPE string; -- Markdown/LaTeX 許容
DEFINE FIELD explanation  ON question TYPE option<string>;
DEFINE FIELD unit         ON question TYPE record<unit>;
DEFINE FIELD created_at   ON question TYPE datetime DEFAULT time::now();
DEFINE INDEX idx_question_unit ON question FIELDS unit;

DEFINE TABLE choice SCHEMAFULL;
DEFINE FIELD question    ON choice TYPE record<question>;
DEFINE FIELD label       ON choice TYPE string ASSERT $value IN ['A','B','C','D'];
DEFINE FIELD body        ON choice TYPE string;
DEFINE FIELD is_correct  ON choice TYPE bool DEFAULT false;
DEFINE FIELD feedback    ON choice TYPE option<string>; -- 選択直後の一言解説
DEFINE FIELD sort_order  ON choice TYPE int DEFAULT 0;
DEFINE INDEX idx_choice_question ON choice FIELDS question;

-- 選択肢 → 次の設問（存在する場合のみ RELATE で作成する）
DEFINE TABLE leads_to SCHEMAFULL TYPE RELATION FROM choice TO question;

DEFINE TABLE user SCHEMAFULL;
DEFINE FIELD email       ON user TYPE option<string>;
DEFINE FIELD created_at  ON user TYPE datetime DEFAULT time::now();

DEFINE TABLE attempt SCHEMAFULL;
DEFINE FIELD user             ON attempt TYPE option<record<user>>; -- 匿名も許容
DEFINE FIELD unit             ON attempt TYPE record<unit>;
DEFINE FIELD current_question ON attempt TYPE option<record<question>>;
DEFINE FIELD status           ON attempt TYPE string DEFAULT 'in_progress'
    ASSERT $value IN ['in_progress', 'completed', 'abandoned'];
DEFINE FIELD started_at   ON attempt TYPE datetime DEFAULT time::now();
DEFINE FIELD completed_at ON attempt TYPE option<datetime>;

DEFINE TABLE attempt_answer SCHEMAFULL;
DEFINE FIELD attempt      ON attempt_answer TYPE record<attempt>;
DEFINE FIELD question     ON attempt_answer TYPE record<question>;
DEFINE FIELD choice       ON attempt_answer TYPE record<choice>;
DEFINE FIELD is_correct   ON attempt_answer TYPE bool; -- 回答時点のスナップショット
DEFINE FIELD answered_at  ON attempt_answer TYPE datetime DEFAULT time::now();
DEFINE INDEX idx_attempt_answer_attempt ON attempt_answer FIELDS attempt;
```

### 代表的なクエリ

```surql
-- ある question の選択肢一覧
SELECT id, label, body FROM choice WHERE question = $question_id ORDER BY sort_order;

-- ある choice を選んだときの次の設問（エッジが無ければ空 = 終端）
SELECT ->leads_to->question.* AS next FROM ONLY $choice_id;

-- 選択肢作成時、次の設問がある場合だけエッジを張る
RELATE $choice_id->leads_to->$next_question_id;
```

前段でRDB(PostgreSQL)向けに設計した `units/questions/choices/attempts/attempt_answers` の構成を概念的にそのまま踏襲しており、差分は「`choices.next_question_id`（NULL許容カラム）」を「`leads_to` エッジ（存在するかしないか）」に置き換えた点のみ。

## 3. バックエンド API（暫定）

REST + JSON。`actix-web` で実装。

すべてのレスポンスで **選択肢の `is_correct` は返さない**（クライアントに正解を漏らさないため）。正誤判定はサーバー側で行い、回答APIのレスポンスとしてのみ結果を返す。

### 3.1 ユニット一覧

```
GET /api/units
```

```json
[
  { "id": 1, "title": "原始ピタゴラス数の偶奇性を証明する", "description": "..." }
]
```

### 3.2 挑戦を開始する

```
POST /api/units/:unit_id/attempts
```

新しい `attempt` を作成し、最初の設問を返す。

レスポンス:

```json
{
  "attempt_id": 42,
  "question": {
    "id": 1,
    "body": "原始ピタゴラス数 (a,b,c) において、a,b はどのような偶奇になるか。",
    "choices": [
      { "id": 101, "label": "A", "body": "奇数 : 奇数" },
      { "id": 102, "label": "B", "body": "偶数 : 奇数 または 奇数 : 偶数" },
      { "id": 103, "label": "C", "body": "偶数 : 偶数" }
    ]
  }
}
```

### 3.3 現在の設問を取得する（再開用）

```
GET /api/attempts/:attempt_id/current
```

ブラウザリロード時などに、`attempt.current_question_id` を元に現在の設問＋選択肢を返す。

### 3.4 回答する

```
POST /api/attempts/:attempt_id/answers
Content-Type: application/json

{ "question_id": 1, "choice_id": 102 }
```

レスポンス（正解の場合）:

```json
{
  "correct": true,
  "feedback": null,
  "explanation": "a,b の一方が偶数、他方が奇数でなければならない。",
  "next_question": {
    "id": 2,
    "body": "なぜ「奇数 : 奇数」はありえないのか。",
    "choices": [ ... ]
  },
  "unit_completed": false
}
```

レスポンス（誤答の場合）:

```json
{
  "correct": false,
  "feedback": "偶数²+偶数² は 4 の倍数になってしまいます。",
  "explanation": null,
  "next_question": null,
  "unit_completed": false
}
```

`next_question` が `null` かつ `unit_completed: true` の場合は unit 完了。

### 3.5 エンドポイント一覧まとめ

| Method | Path | 説明 |
|---|---|---|
| GET | /api/units | ユニット一覧 |
| POST | /api/units/:unit_id/attempts | 挑戦開始（最初の設問を返す） |
| GET | /api/attempts/:attempt_id/current | 現在の設問を取得（再開用） |
| POST | /api/attempts/:attempt_id/answers | 回答送信 |

将来的に認証を入れる場合は `users`/セッションと `attempts.user_id` を紐付ける想定（今は匿名 `attempt` のみでも成立する設計）。

## 4. フロントエンド（React、暫定）

### 4.1 画面構成

- `UnitListPage` — ユニット一覧を表示、選択すると挑戦開始APIを叩いて Quiz 画面へ遷移
- `QuizPage` — 現在の設問と選択肢を表示。回答すると結果（正誤・フィードバック・解説）を一瞬表示してから次の設問へ切り替わる。完了時は完了画面を表示

### 4.2 状態管理

`QuizPage` はシンプルなローカル state で足りる想定（Redux 等は不要）。

```
{
  attemptId: number,
  question: { id, body, choices: [{id, label, body}] } | null,
  lastResult: { correct, feedback, explanation } | null,
  status: "answering" | "showing_result" | "completed"
}
```

- ページ離脱・リロード時は `GET /api/attempts/:id/current` で復帰できるよう `attemptId` を `localStorage` 等に保持する

### 4.3 UI フロー

1. ユニット一覧から選択 → `POST /api/units/:id/attempts` → 最初の設問を表示
2. 選択肢をクリック → `POST /api/attempts/:id/answers`
3. 結果表示（正解: 緑＋解説／誤答: 赤＋フィードバック、選び直させる）
4. 正解時は次の設問へ自動 or ボタン遷移。誤答時は同じ設問に留まり再選択させる
5. `unit_completed: true` で完了画面へ

### 4.4 数式レンダリング

`question.body` / `choice.body` はDB上ではただの文字列で、本格的な数式はLaTeX記法をそのまま埋め込んで表現する(DB側のスキーマ変更は不要)。

```
body = 'n が奇数のとき $n^2 \equiv 1 \pmod{4}$ となる理由は?'
```

- 区切り記法: インライン数式は `$...$`、独立行の数式は `$$...$$` に統一する
- レンダリングライブラリは **KaTeX** を採用する(`react-katex` などのラッパー経由でNext.jsに組み込む)。MathJaxより軽量・高速で、個人開発で扱う範囲の数式には十分な網羅性があるため

## 5. 未決事項（要検討）

- 認証を入れるか、当面は匿名 `attempt` のみで進めるか
- 誤答時に「同じ設問で再挑戦」以外に、誤答専用の深掘り設問（`leads_to` エッジ）を用意するか

## 6. DB運用・デプロイ方針

DBとアプリのデプロイ先は分離する。

- **DB: Surreal Cloud の無料枠**（1GBストレージまで永久無料）を使う。マネージドなので自前でVMや永続ディスクを用意する必要がなく、Surrealist(公式GUI)からもそのまま接続できる
  - 開発中のローカル動作確認・学習用には `examples/db_demo.rs` のような **embedded モード**（`kv-surrealkv` feature、ローカルファイルに永続化）も引き続き使う。こちらはサーバー不要で手軽な反面、外部GUIからは接続できない(単一プロセスの中に閉じたDB)ため、GUIで覗きたい時だけ別途 `surreal start` でローカルサーバー化する
  - 本番は embedded ではなく、actix-web から Surreal Cloud のエンドポイントへ `Ws`(remote client)で接続する。接続情報(エンドポイントURL・namespace・db・認証情報)は環境変数で渡し、コードにハードコードしない
- **アプリ(actix-web): Render の無料プラン**。Rustのネイティブ環境として動かせる。DB側の状態はSurreal Cloudに逃がしてあるので、Renderの無料プランがエフェメラルディスクでも問題ない(actix側はステートレス)
  - 無料プランは15分アクセスが無いとスピンダウンし、次のリクエストで約1分のコールドスタートが発生する点は許容する。気になるようになったら有料プランへの引き上げを検討する
- 比較検討したが不採用にした選択肢: Fly.io(2024年に無料枠廃止、月額$8〜25程度)、Oracle Cloud Free Tier(2026年6月に容量半減・確保しづらい)、自前VPS(Hetzner/Contabo、月500〜800円で悪くはないが管理の手間が増える)
- SurrealDB は API・スキーマともに変更が比較的活発なので、`Cargo.toml` でバージョンを固定し、アップデート時は変更点を確認してから上げる
