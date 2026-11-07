# tadoru: 補足ブランチ(任意の寄り道設問)

## Context

本流の設問に正解したとき、「本当に理解して正解したのか」を確かめる/深掘りするための
**補足ブランチ**に任意で寄り道できるようにしたい。

例: `q2「奇数の2乗を4で割った余りは？」` に正解(1)したあと、
「次へ(q3)」と「なぜ 1 なのか確認する(補足)」のどちらかを選べる。

```
q2 ──(正解)──┬──→ q3(本流)
             └──→ q2s1 ──→ q2s2 ──(終わり / いつでも離脱)──→ q3 に戻る
                  補足ブランチ
```

決定済みの要件:

- 補足ブランチは**任意**。正解後に本流へ進むか補足へ寄るかをユーザーが選ぶ
- 補足ブランチは**一部の設問にだけ**存在する(無い設問が普通)
- 補足ブランチ内の設問は**正解しなくてよい**。いつでも離脱して本流に戻れる
- **補足の補足は作らない**(ネストは深さ1まで)

## 設計方針

### 本流への戻り先はフロントが保持する(サーバーはステートレスのまま)

本流の設問に正解した時点のレスポンスに、本流の次の設問(`next_question`)と補足の入口
(`branch_question`)を**両方**含めて返す。フロントは補足に入る際に `next_question` を手元に
保持しておき、補足の途中離脱・補足の終了時にそれを表示する。

これにより:

- attempt(進捗の保存)が無い現状でも戻り先を見失わない
- 補足ブランチの末尾から本流へ戻る `leads_to` エッジは**不要**(張らない)
- 本流の設問を ID で直接取得する API を新設しなくてよい(先読み防止の方針を維持)

### 本流/補足の区別は `question.branch_of` で行う

- 本流の設問: `branch_of = NONE`
- 補足の設問: `branch_of = question:<親の本流設問>`(同じブランチの設問はすべて同じ親を指す)

`leads_to` エッジは新しいテーブルを作らず流用する。正解選択肢から2本出ている場合、
遷移先の `branch_of` を見て本流か補足かを判定する。

## 変更するファイル

### 1. `src/model.rs`

- `QuestionRow` に `branch_of: Option<RecordId>` を追加
- `QuestionResponse` に `is_branch: bool` を追加(`branch_of.is_some()` から作る)
- `AnswerResponse` を拡張:

```rust
pub struct AnswerResponse {
    pub correct: bool,
    pub next_question: Option<QuestionResponse>,   // 本流の次 or 補足ブランチ内の次
    pub branch_question: Option<QuestionResponse>, // 本流で正解したときだけ。補足の入口
    pub unit_completed: bool,
    pub branch_completed: bool,                    // 補足ブランチの末尾に正解した
}
```

### 2. `src/main.rs` — `answer_question`

1. 正誤判定(現状どおり)。同時に、回答した設問の `branch_of` も取得する
2. 不正解 → `correct: false`、他は全部 `None` / `false`(現状どおり、再挑戦)
3. 正解 → `SELECT ->leads_to->question.{id, body, branch_of} AS next FROM ONLY $choice_id;`
   - 現状の `row.next.into_iter().next()`(最初の1件だけ見る)をやめ、`branch_of` で振り分ける
4. 回答した設問が**本流**の場合
   - 本流の次(`branch_of = NONE`)があれば `next_question` に入れる。無ければ `unit_completed: true`
   - 補足の入口(`branch_of` あり)があれば `branch_question` に入れる
   - 最後の本流設問に補足がある場合は `unit_completed: true` と `branch_question` が同時に返る
     (フロントは「完了」と「補足を見る」を並べ、補足終了後に完了画面へ)
5. 回答した設問が**補足**の場合
   - 次があれば `next_question` に入れる(`is_branch: true`)
   - 無ければ `branch_completed: true`(フロントは保持していた本流の設問へ戻る)
   - 補足内では `unit_completed` は常に `false`

### 3. `src/main.rs` — `unit_start`

`start_question.branch_of AS branch_of` も SELECT して `QuestionRow` を埋める
(開始設問は本流なので常に `None` になるはず)。

### 4. `src/main.rs` — シード投入後の整合性チェック

ルールはデータで守るしかないので、`ensure_schema()` の最後に違反を探すクエリを流し、
1件でも見つかったら起動を失敗させる。

| ルール | 違反の例 |
|---|---|
| 1つの選択肢から出る本流行きのエッジは1本まで | `q2_b → q3` と `q2_b → q9` |
| 1つの選択肢から出る補足行きのエッジは1本まで | `q2_b → q2s1` と `q2_b → q2t1` |
| 本流→補足のエッジは、補足の `branch_of` と同じ設問の選択肢からのみ | `q3_a → q2s1` |
| 補足の設問の選択肢は、同じ `branch_of` の設問にしか行かない(補足の補足・本流への直接合流を禁止) | `q2s1_a → q5s1`, `q2s1_a → q3` |
| 補足への遷移は正解選択肢からのみ | `q2_a(誤答) → q2s1` |

クエリ例(SurrealQL の `$parent` がグラフフィルタ内で期待どおり動くかは実装時に要確認):

```surql
-- 補足設問の選択肢が、別の branch_of を持つ設問(または本流)に遷移している
SELECT id FROM choice
WHERE question.branch_of != NONE
  AND count(->leads_to->question[WHERE branch_of != $parent.question.branch_of]) > 0;
```

### 5. シードデータ — 補足ブランチの例を追加

`q2`(奇数² mod 4)に補足ブランチを1本付ける。

```surql
UPSERT question:q2s1 SET branch_of = question:q2,
    body = '奇数 2k+1 を2乗して、4 でくくった形はどれか。';
UPSERT choice:q2s1_a SET question = question:q2s1, label = 'A', body = '4(k²+k)+1', is_correct = true;
UPSERT choice:q2s1_b SET question = question:q2s1, label = 'B', body = '4k²+1', is_correct = false;
UPSERT choice:q2s1_c SET question = question:q2s1, label = 'C', body = '4(k²+k)+2', is_correct = false;

RELATE choice:q2_b -> leads_to -> question:q2s1;  -- q2_b → q3 と並ぶ2本目
-- q2s1_a にはエッジを張らない = 補足ブランチ終了
```

### 6. `docs/SPEC.md`

§1 コンセプト、§2 データモデル(`question.branch_of`)、§3 API(`AnswerResponse` の新フィールド)
に補足ブランチを追記する。

## 検証手順

1. `scripts/start-db.bat` でDBを起動、`cargo run`
2. curl で確認:
   - `q2` に `q2_b` で回答 → `next_question` が `q3`、`branch_question` が `q2s1`(`is_branch: true`)
   - `q2s1` に `q2s1_b` で回答 → `correct: false`(再挑戦。離脱はフロントの責務)
   - `q2s1` に `q2s1_a` で回答 → `correct: true, branch_completed: true, unit_completed: false`
   - 補足の無い設問(例: `q3`)に正解 → `branch_question: null`
   - 最後の設問 `q6` に正解 → `unit_completed: true`
3. 整合性チェック: わざと `RELATE choice:q2s1_a -> leads_to -> question:q3;` を足して起動が失敗することを確認し、戻す

## 未決事項

- 補足ブランチで正解できずに離脱するとき、正解(や解説)を見せるか
  - 見せるなら「答えを見る」API が必要(`is_correct` を返さない方針との兼ね合い)
- フロントで補足の入口をどう見せるか(ボタン文言、補足中であることの表示)
