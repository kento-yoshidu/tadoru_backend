mod model;
use actix_web::post;
use actix_web::{
    App, HttpResponse, HttpServer, get, http::header, web,
};
use actix_cors::Cors;
use serde::Serialize;
use surrealdb::{Surreal, types::RecordId};
use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;
use model::{
    UnitRow,
    UnitResponse,
};

use crate::model::{
    AnswerRequest,
    AnswerResponse,
    ChoiceRow,
    CorrectnessRow,
    NextQuestionRow,
    QuestionResponse,
    QuestionRow,
};

/// DB接続の死活監視用
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    message: Option<String>,
}

#[get("/db-health")]
async fn db_health(db: web::Data<Surreal<Any>>) -> HttpResponse {
    match db.health().await {
        Ok(()) => HttpResponse::Ok().json(HealthResponse {
            status: "ok",
            message: None,
        }),
        Err(err) => HttpResponse::ServiceUnavailable().json(HealthResponse {
            status: "error",
            message: Some(err.to_string()),
        }),
    }
}

#[get("/api/units")]
async fn list_units(db: web::Data<Surreal<Any>>) -> HttpResponse {
    let mut res = match db.query("SELECT id, title, description FROM unit ORDER BY title;").await {
        Ok(res) => res,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let rows: Vec<UnitRow> = match res.take(0) {
        Ok(rows) => rows,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let units: Vec<UnitResponse> = rows.into_iter().map(Into::into).collect();
    HttpResponse::Ok().json(units)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[get("/api/units/{unit_id}/start")]
async fn unit_start(db: web::Data<Surreal<Any>>, path: web::Path<String>) -> HttpResponse {
    let unit_id = RecordId::new("unit", path.into_inner());

    let mut res = match db
        .query("SELECT start_question.id AS id, start_question.body AS body, start_question.branch_of AS branch_of FROM ONLY $unit_id;")
        .bind(("unit_id", unit_id))
        .await
    {
        Ok(res) => res,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let question: Option<QuestionRow> = match res.take(0) {
        Ok(question) => question,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let Some(question) = question else {
        return HttpResponse::NotFound().finish();
    };

    let choices = match fetch_choices(&db, &question.id).await {
        Ok(choices) => choices,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    HttpResponse::Ok().json(QuestionResponse::from_parts(question, choices))
}

#[post("/api/questions/{question_id}/answer")]
async fn answer_question(
    db: web::Data<Surreal<Any>>,
    path: web::Path<String>,
    body: web::Json<AnswerRequest>,
) -> HttpResponse {
    let question_id = RecordId::new("question", path.into_inner());
    let choice_id = RecordId::new("choice", body.into_inner().choice_id);

    let mut res = match db
        .query("SELECT
                    is_correct,
                    question.branch_of AS branch_of
                FROM
                    choice
                WHERE id = $choice_id AND question = $question_id;")
        .bind(("choice_id", choice_id.clone()))
        .bind(("question_id", question_id))
        .await
    {
        Ok(res) => res,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let rows: Vec<CorrectnessRow> = match res.take(0) {
        Ok(rows) => rows,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let Some(correctness) = rows.into_iter().next() else {
        return HttpResponse::NotFound().finish();
    };

    if !correctness.is_correct {
        return HttpResponse::Ok().json(AnswerResponse {
            correct: false,
            next_question: None,
            branch_question: None,
            unit_completed: false,
        });
    }

    let mut res = match db
        .query("SELECT ->leads_to->question.{id, body, branch_of} AS next FROM ONLY $choice_id;")
        .bind(("choice_id", choice_id))
        .await
    {
        Ok(res) => res,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let next: Option<NextQuestionRow> = match res.take(0) {
        Ok(next) => next,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let rows: Vec<QuestionRow> = next.map(|row| row.next).unwrap_or_default();
    let answered_is_branch = correctness.branch_of.is_some();

    let (next_row, branch_row) = if answered_is_branch {
        // 補足ブランチ内: 次も同じブランチの設問。エッジが無ければブランチ終端
        (rows.into_iter().next(), None)
    } else {
        // 本流: 遷移先の branch_of の有無で「本流の次」と「補足の入口」に振り分ける
        let (branch, main): (Vec<_>, Vec<_>) =
            rows.into_iter().partition(|q| q.branch_of.is_some());
        (main.into_iter().next(), branch.into_iter().next())
    };

    // 完了判定は本流だけ。補足ブランチの終端はユニット完了ではない
    let unit_completed = !answered_is_branch && next_row.is_none();

    let next_question = match with_choices(&db, next_row).await {
        Ok(q) => q,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let branch_question = match with_choices(&db, branch_row).await {
        Ok(q) => q,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    HttpResponse::Ok().json(AnswerResponse {
        correct: true,
        next_question,
        branch_question,
        unit_completed,
    })
}

async fn fetch_choices(db: &Surreal<Any>, question_id: &RecordId) -> surrealdb::Result<Vec<ChoiceRow>> {
    db.query("SELECT id, label, body FROM choice WHERE question = $question_id ORDER BY label;")
        .bind(("question_id", question_id.clone()))
        .await?
        .take(0)
}

/// 遷移先の設問(Some)なら選択肢を付けて QuestionResponse にする。
/// 遷移先が無い(None)ならそのまま None を返す(レスポンスでは null になる)
async fn with_choices(
    db: &Surreal<Any>,
    row: Option<QuestionRow>,
) -> surrealdb::Result<Option<QuestionResponse>> {
    let Some(row) = row else { return Ok(None) };
    let choices = fetch_choices(db, &row.id).await?;
    Ok(Some(QuestionResponse::from_parts(row, choices)))
}

async fn connect_db() -> surrealdb::Result<Surreal<Any>> {
    let endpoint = env_or("SURREAL_ENDPOINT", "ws://127.0.0.1:8000");
    let ns = env_or("SURREAL_NS", "tadoru");
    let db_name = env_or("SURREAL_DB", "dev");
    let username = env_or("SURREAL_USER", "root");
    let password = env_or("SURREAL_PASS", "root");

    let db = surrealdb::engine::any::connect(endpoint).await?;
    db.signin(Root { username, password }).await?;
    db.use_ns(ns).use_db(db_name).await?;

    Ok(db)
}

/// スキーマ定義と動作確認用のシードデータ投入。
///
/// テーブル定義は OVERWRITE、question / choice / leads_to は毎回消してから
/// 入れ直すので、毎回実行しても安全。ただし本来は
/// 「毎起動で走らせる」ものと「1回きりの初期データ投入」は別物なので、
/// 実データを扱うようになったらここは切り離すこと(README/SPEC.md 未決事項 参照)。
async fn ensure_schema(db: &Surreal<Any>) -> surrealdb::Result<()> {
    db.query(
        r#"
        DEFINE TABLE OVERWRITE unit SCHEMALESS;
        DEFINE TABLE OVERWRITE question SCHEMALESS;
        DEFINE TABLE OVERWRITE choice SCHEMALESS;
        DEFINE TABLE OVERWRITE leads_to TYPE RELATION FROM choice TO question SCHEMALESS;

        -- シードは毎回まっさらな状態から入れ直す。
        -- UPSERT だけだとコードから消した設問・選択肢が DB に残り続けるため。
        -- (RELATE も毎回実行すると重複行が増えるので、エッジも消して張り直す)
        --
        -- question / choice の ID は unit をまたいだ通し番号(q1, q2, ... / q1_a, ...)。
        -- 消した直後なので CREATE で入れる。ID が重複すると上書きされずにエラーになり、
        -- 起動時に気づける(UPSERT だと黙って上書きされる)。
        DELETE leads_to;
        DELETE choice;
        DELETE question;

        -- 結論(ゴール)
        CREATE question:q1 SET body = '原始ピタゴラス数 (a,b,c) において、a,b はどのような偶奇になるか。';
        CREATE choice:q1_a SET question = question:q1, label = 'A', body = '奇数 : 奇数', is_correct = false;
        CREATE choice:q1_b SET question = question:q1, label = 'B', body = '偶数 : 奇数 または 奇数 : 偶数', is_correct = true;

        -- 前提知識(1): 奇数² mod 4
        CREATE question:q2 SET body = '奇数の2乗を4で割った余りはいくつか。(ヒント: 奇数は 2k+1 と書ける)';
        CREATE choice:q2_a SET question = question:q2, label = 'A', body = '0', is_correct = false;
        CREATE choice:q2_b SET question = question:q2, label = 'B', body = '1', is_correct = true;
        CREATE choice:q2_c SET question = question:q2, label = 'C', body = '2', is_correct = false;
        CREATE choice:q2_d SET question = question:q2, label = 'D', body = '奇数によって変わる', is_correct = false;

        -- 補足ブランチ: q2 の「なぜ 1 なのか」
        -- (本流の設問には branch_of を書かない = NONE)
        -- ID は本流と同じ通し番号。補足かどうかは ID ではなく branch_of で判定する
        CREATE question:q13 SET
            branch_of = question:q2,
            body = '奇数 2k+1 を2乗して、4 でくくった形はどれか。';
        CREATE choice:q13_a SET question = question:q13, label = 'A', body = '4(k²+k)+1', is_correct = true;
        CREATE choice:q13_b SET question = question:q13, label = 'B', body = '4k²+1', is_correct = false;
        CREATE choice:q13_c SET question = question:q13, label = 'C', body = '4(k²+k)+2', is_correct = false;
        -- q13_a には leads_to を張らない = 補足ブランチ終了

        -- 前提知識(2): 平方数 mod 4
        CREATE question:q3 SET body = '整数の2乗(平方数)を4で割った余りとして、ありえるものはどれか。';
        CREATE choice:q3_a SET question = question:q3, label = 'A', body = '0 と 1 だけ', is_correct = true;
        CREATE choice:q3_b SET question = question:q3, label = 'B', body = '0 と 2 だけ', is_correct = false;
        CREATE choice:q3_c SET question = question:q3, label = 'C', body = '1 と 3 だけ', is_correct = false;
        CREATE choice:q3_d SET question = question:q3, label = 'D', body = '0, 1, 2, 3 のすべて', is_correct = false;

        -- 奇数 : 奇数 の否定
        CREATE question:q4 SET body = 'なぜ a, b が「奇数 : 奇数」になることはありえないのか。';
        CREATE choice:q4_a SET question = question:q4, label = 'A', body = '奇数²は奇数だから', is_correct = false;
        CREATE choice:q4_b SET question = question:q4, label = 'B', body = '奇数²+奇数²は4で割ると2余るが、そのような平方数 c² は存在しないから', is_correct = true;
        CREATE choice:q4_c SET question = question:q4, label = 'C', body = '奇数²+奇数²は奇数になるから', is_correct = false;

        -- 偶数 : 偶数 の否定
        CREATE question:q5 SET body = 'では、なぜ「偶数 : 偶数」もありえないのか。';
        CREATE choice:q5_a SET question = question:q5, label = 'A', body = '偶数²+偶数²は必ず奇数になるから', is_correct = false;
        CREATE choice:q5_b SET question = question:q5, label = 'B', body = 'a, b, c がすべて2で割り切れ、互いに素(原始的)という条件に反するから', is_correct = true;
        CREATE choice:q5_c SET question = question:q5, label = 'C', body = '偶数²は4で割ると余りが2になるから', is_correct = false;

        -- 仕上げ: c の偶奇
        CREATE question:q6 SET body = 'a, b の一方が偶数で他方が奇数のとき、c の偶奇はどうなるか。';
        CREATE choice:q6_a SET question = question:q6, label = 'A', body = '偶数', is_correct = false;
        CREATE choice:q6_b SET question = question:q6, label = 'B', body = '奇数', is_correct = true;
        CREATE choice:q6_c SET question = question:q6, label = 'C', body = 'a, b の値によって変わる', is_correct = false;

        RELATE choice:q1_b -> leads_to -> question:q2;
        RELATE choice:q2_b -> leads_to -> question:q3;
        RELATE choice:q2_b -> leads_to -> question:q13;  -- 2本目: 補足ブランチの入口
        RELATE choice:q3_a -> leads_to -> question:q4;
        RELATE choice:q4_b -> leads_to -> question:q5;
        RELATE choice:q5_b -> leads_to -> question:q6;
        -- q6_b には leads_to を張らない = 正解でユニット完了

        UPSERT unit:pythagorean_parity SET
            title = '原始ピタゴラス数の偶奇性を証明する',
            description = 'a, b の偶奇を mod 4 の議論から導く',
            start_question = question:q1;

        -- ===== unit: ターメリックと黒コショウ =====
        -- 成分 → 吸収の概念 → 吸収されにくい理由 → 代謝(排出)の仕組み
        -- → 代謝が遅れたらどうなるか(推論) → ピペリンの作用と組み合わせて結論

        -- q7: ターメリックの主成分
        CREATE question:q7 SET
            body = 'カレーの黄色のもとになるスパイス、ターメリック(ウコン)。その黄色い色素であり、健康効果の研究対象としてよく取り上げられる主な成分はどれか。',
            explanation = 'クルクミンはポリフェノールの一種で、ターメリックの黄色のもと。抗炎症作用などが研究されてきたが、「摂っても体にほとんど取り込まれない」という大きな弱点がある。その弱点を理解するために、まず「体に取り込まれる」とはどういうことかを考える。';
        CREATE choice:q7_a SET question = question:q7, label = 'A', body = 'カプサイシン', is_correct = false,
            feedback = 'カプサイシンは唐辛子の辛味成分。';
        CREATE choice:q7_b SET question = question:q7, label = 'B', body = 'クルクミン', is_correct = true;
        CREATE choice:q7_c SET question = question:q7, label = 'C', body = 'ピペリン', is_correct = false,
            feedback = 'ピペリンは黒コショウの辛味成分。このユニットの後半で登場する。';
        CREATE choice:q7_d SET question = question:q7, label = 'D', body = 'アントシアニン', is_correct = false,
            feedback = 'アントシアニンはブルーベリーや紫芋などに含まれる青紫色の色素。';

        -- q8: バイオアベイラビリティとは
        CREATE question:q8 SET
            body = '栄養学や薬学でいう「バイオアベイラビリティ(生物学的利用能)」とは、何を表す指標か。',
            explanation = '口から摂った成分は、腸で吸収され、肝臓を通ってから全身の血液に入る。その途中で吸収されなかったり分解されたりすると、血中に届く量は減る。この「届く割合」がバイオアベイラビリティ。次は、クルクミンの場合にこの割合がどうなるかを見る。';
        CREATE choice:q8_a SET question = question:q8, label = 'A', body = '食品1gあたりに含まれる成分の量', is_correct = false,
            feedback = 'それは含有量。たくさん含まれていても、体に取り込まれなければ意味がない。';
        CREATE choice:q8_b SET question = question:q8, label = 'B', body = '成分が体内で効果を発揮し始めるまでの時間', is_correct = false,
            feedback = 'それは効き始めの速さで、別の指標。';
        CREATE choice:q8_c SET question = question:q8, label = 'C', body = '摂取した成分のうち、変化せずに血液(全身の循環)まで届いて利用できる割合', is_correct = true;
        CREATE choice:q8_d SET question = question:q8, label = 'D', body = '成分が体の外に排出されるまでの時間', is_correct = false,
            feedback = '体内にとどまる時間(半減期など)は別の指標。';

        -- q9: クルクミンのバイオアベイラビリティが低い理由
        CREATE question:q9 SET
            body = 'クルクミンはバイオアベイラビリティが非常に低いことで知られる。その理由として最も適切なものはどれか。',
            explanation = 'クルクミンは脂溶性で水に溶けにくく、そもそも腸からの吸収が悪い。さらに、吸収された分も腸の壁や肝臓で素早く代謝され、すぐに排出されてしまう。このユニットでは、2つの弱点のうち後半の「すぐ代謝される」に注目する。次はその代謝の中身を見る。';
        CREATE choice:q9_a SET question = question:q9, label = 'A', body = '胃酸で完全に分解され、腸まで届かないから', is_correct = false,
            feedback = '胃酸で全部分解されるわけではない。主な問題は、腸での吸収されにくさと、吸収後の速い代謝。';
        CREATE choice:q9_b SET question = question:q9, label = 'B', body = '水に溶けにくいため腸から吸収されにくく、吸収されても腸の壁や肝臓ですぐ代謝されて排出されるから', is_correct = true;
        CREATE choice:q9_c SET question = question:q9, label = 'C', body = '分子が大きすぎて、腸の壁を一切通過できないから', is_correct = false,
            feedback = '一切通過しないわけではない。少しは吸収されるが、その後すぐ代謝されてしまう点が大きい。';
        CREATE choice:q9_d SET question = question:q9, label = 'D', body = '腸内細菌がすべて分解してしまうから', is_correct = false,
            feedback = '腸内細菌による代謝も一部あるが、「すべて」ではなく、主な理由でもない。';

        -- 補足ブランチ: q9 のもう一つの弱点「水に溶けにくい」
        -- q14: なぜ水に溶けにくいか
        CREATE question:q14 SET
            branch_of = question:q9,
            body = 'クルクミンは水にはほとんど溶けないが、油やアルコールにはよく溶ける。その理由として最も適切なものはどれか。',
            explanation = '水分子は、電気的な偏りを持つもの同士で引き合う(水素結合)。クルクミンは分子の大部分がベンゼン環や炭素の鎖といった偏りの少ない部分でできていて、水と結びつける部分(OH など)が分子の大きさの割に少ない。そのため水の輪に入れず、同じく偏りの少ない油にはなじむ。「似たものは似たものに溶ける」と覚えておくとよい。';
        CREATE choice:q14_a SET question = question:q14, label = 'A', body = '分子が重いから', is_correct = false,
            feedback = '重さだけでは決まらない。砂糖(ショ糖)はクルクミンと同じくらいの重さだが、水によく溶ける。';
        CREATE choice:q14_b SET question = question:q14, label = 'B', body = '黄色い色素だから', is_correct = false,
            feedback = '色と溶けやすさは関係ない。アントシアニンのように水によく溶ける色素もある。';
        CREATE choice:q14_c SET question = question:q14, label = 'C', body = '分子の大部分が油になじむ骨格(ベンゼン環や炭素の鎖)でできていて、水と結びつける部分が少ないから', is_correct = true;
        CREATE choice:q14_d SET question = question:q14, label = 'D', body = '酸性の物質だから', is_correct = false,
            feedback = '酸性かどうかでは決まらない。お酢の成分(酢酸)は酸性だが、水によく溶ける。';

        -- q15: だから油と一緒に摂る(補足ブランチ終端)
        CREATE question:q15 SET
            branch_of = question:q9,
            body = '腸の中は水が主体の環境で、成分は腸の中の液に溶けた状態でないと吸収されにくい。水に溶けにくいクルクミンを吸収されやすくする食べ方として、理にかなっているものはどれか。',
            explanation = 'クルクミンは油によく溶けるので、油を使った料理で摂ると油に溶け込んだ状態になる。食事の油は腸で胆汁によって細かい粒にされるので、その中に溶けたクルクミンも一緒に腸の中の液に散らばり、吸収されやすくなる。ターメリックを使うカレーが油と一緒に調理されるのは、この点でも理にかなっている。';
        CREATE choice:q15_a SET question = question:q15, label = 'A', body = 'ターメリックを冷たい水に溶いて飲む', is_correct = false,
            feedback = 'クルクミンは水にほとんど溶けないので、水に溶いても溶けないまま。問題はそのまま残る。';
        CREATE choice:q15_b SET question = question:q15, label = 'B', body = '空腹時に、ほかの食べ物と一緒にならないように摂る', is_correct = false,
            feedback = '空腹にしても水への溶けにくさは変わらない。むしろ食事に含まれる油が吸収の助けになる。';
        CREATE choice:q15_c SET question = question:q15, label = 'C', body = '砂糖と一緒に摂る', is_correct = false,
            feedback = '砂糖は水によく溶けるが、クルクミンを溶かしてくれるわけではない。';
        CREATE choice:q15_d SET question = question:q15, label = 'D', body = 'カレーのように、油を使った料理で一緒に摂る', is_correct = true;
        -- q15_d には leads_to を張らない = 補足ブランチ終了

        -- q10: グルクロン酸抱合
        CREATE question:q10 SET
            body = 'クルクミンの代謝で中心となる「グルクロン酸抱合」は、肝臓や腸の壁で行われる。これは体にとってどのような役割を持つ代謝か。',
            explanation = 'グルクロン酸抱合は、薬や体の外から入ってきた物質を水に溶けやすい形に変えて排出する、いわば「体の掃除の仕組み」。この反応は、グルクロン酸をくっつける専用の酵素(UDP-グルクロン酸転移酵素)が担っている。クルクミンはこの仕組みですぐに処理されてしまう。では、この掃除が遅れたら何が起きるだろうか。';
        CREATE choice:q10_a SET question = question:q10, label = 'A', body = '成分にグルクロン酸をくっつけて水に溶けやすくし、尿や胆汁で体の外へ出しやすくする', is_correct = true;
        CREATE choice:q10_b SET question = question:q10, label = 'B', body = '成分を分解してエネルギーに変える', is_correct = false,
            feedback = 'エネルギーを作る代謝ではない。体の外に出すための変換。';
        CREATE choice:q10_c SET question = question:q10, label = 'C', body = '成分を脂肪組織に蓄え、必要なときに放出する', is_correct = false,
            feedback = 'むしろ逆で、体の外に出しやすくする仕組み。';
        CREATE choice:q10_d SET question = question:q10, label = 'D', body = '成分の効き目を強めるために、より作用の強い形に変える', is_correct = false,
            feedback = '抱合は多くの場合、成分を作用の弱い、排出しやすい形に変える。';

        -- q11: 代謝(排出)が遅れたら血中濃度はどうなるか(推論)
        CREATE question:q11 SET
            body = 'クルクミンは、腸の壁や肝臓でグルクロン酸抱合を受けて、次々と体の外へ出されている。もし何らかの理由で、このグルクロン酸抱合が一時的に遅くなったとしたら、血中のクルクミンはどうなると考えられるか。',
            explanation = 'お風呂に例えると、腸からの吸収が「蛇口」、グルクロン酸抱合による排出が「排水口」。蛇口の勢いが同じでも、排水口が狭くなれば水位(血中濃度)は上がる。クルクミンの弱点の一つは、この排水口が大きすぎることだった。では、排水口を狭くしてくれるものが身近な食材にあるとしたら?';
        CREATE choice:q11_a SET question = question:q11, label = 'A', body = '腸からの吸収そのものが止まり、血中濃度は下がる', is_correct = false,
            feedback = '抱合が遅れても、吸収が止まるわけではない。変わるのは「取り込まれた後に処理されて出ていく速さ」。';
        CREATE choice:q11_b SET question = question:q11, label = 'B', body = '排出されなくなったクルクミンが別の物質に変わり、血中のクルクミンは減る', is_correct = false,
            feedback = '抱合が遅れれば、クルクミンは変化しないまま残る。別の物質に変わるわけではない。';
        CREATE choice:q11_c SET question = question:q11, label = 'C', body = '抱合されずに残るクルクミンが増え、血中濃度が上がる', is_correct = true;
        CREATE choice:q11_d SET question = question:q11, label = 'D', body = '抱合は排出のための仕組みなので、血中濃度には影響しない', is_correct = false,
            feedback = '排出の仕組みが遅れれば、体の中に残る量は増える。出ていく量が減れば、たまる量は増える。';

        -- q12: ピペリンの作用と組み合わせて結論(ユニット最終問題)
        CREATE question:q12 SET
            body = '黒コショウの辛味成分ピペリンには、グルクロン酸抱合を担う酵素の働きを弱める作用がある。ターメリックと黒コショウを一緒に摂ると、クルクミンの血中濃度が大きく上がることが知られている。その理由として最も適切なものはどれか。',
            explanation = 'ピペリンがグルクロン酸抱合の酵素の働きを弱めると、前の問題で考えたように「排水口」が狭くなる。そのため、体の外へ出されるはずだったクルクミンが、変化しないまま血中に残りやすくなる。ヒトを対象にした研究(Shoba ら, 1998)では、クルクミン 2g にピペリン 20mg を加えるだけで、バイオアベイラビリティが約20倍になったと報告されている。なお、黒コショウが補うのは「すぐ代謝される」という弱点のほうで、「水に溶けにくい」という弱点はそのまま残る。';
        CREATE choice:q12_a SET question = question:q12, label = 'A', body = '黒コショウの辛味で体が温まり血流が増えるため、クルクミンが全身に速く運ばれるから', is_correct = false,
            feedback = '問題文にあるピペリンの作用は「グルクロン酸抱合を弱める」こと。それによって変わるのは、血流ではなく排出の速さ。';
        CREATE choice:q12_b SET question = question:q12, label = 'B', body = 'ピペリンが腸の壁や肝臓でのグルクロン酸抱合などの代謝を妨げるため、分解されないクルクミンが血中に残りやすくなるから', is_correct = true;
        CREATE choice:q12_c SET question = question:q12, label = 'C', body = 'ピペリンがクルクミンと化学的に結合し、効き目の強い別の化合物に変わるから', is_correct = false,
            feedback = 'ピペリンはクルクミンを別の物質に変えるわけではない。クルクミンが代謝されるのを遅らせるだけ。';
        CREATE choice:q12_d SET question = question:q12, label = 'D', body = '黒コショウ自体にもクルクミンが含まれており、摂る量そのものが増えるから', is_correct = false,
            feedback = 'クルクミンはターメリックに特徴的な成分で、黒コショウにはほとんど含まれていない。黒コショウ側の主な成分はピペリン。';

        RELATE choice:q7_b -> leads_to -> question:q8;
        RELATE choice:q8_c -> leads_to -> question:q9;
        RELATE choice:q9_b -> leads_to -> question:q10;
        RELATE choice:q9_b -> leads_to -> question:q14;  -- 2本目: 補足ブランチの入口
        RELATE choice:q14_c -> leads_to -> question:q15;
        RELATE choice:q10_a -> leads_to -> question:q11;
        RELATE choice:q11_c -> leads_to -> question:q12;
        -- q12_b には leads_to を張らない = 正解でユニット完了

        UPSERT unit:turmeric_pepper SET
            title = 'ターメリックと黒コショウの食べ合わせ',
            description = 'クルクミンが吸収されにくい理由から、黒コショウ(ピペリン)で吸収率が上がる仕組みまで',
            start_question = question:q7;
        "#,
    )
    .await?
    .check()?;

    Ok(())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();

    let db = connect_db()
        .await
        .expect("failed to connect to SurrealDB (ローカルサーバーが起動していますか? scripts/start-db.bat)");

    ensure_schema(&db).await.expect("failed to ensure schema");

    let db = web::Data::new(db);

    let bind_port: u16 = env_or("PORT", "8080").parse().expect("PORT must be a number");

    let allowed_origins = env_or("ALLOWED_ORIGINS", "http://localhost:5173");

    HttpServer::new(move || {
        let mut cors = Cors::default()
            .allowed_methods(vec!["GET", "POST"])
            .allowed_header(header::CONTENT_TYPE)
            .max_age(3600);

        for origin in allowed_origins.split(',') {
            cors = cors.allowed_origin(origin.trim());
        }

        App::new()
            .wrap(cors)
            .app_data(db.clone())
            .service(db_health)
            .service(list_units)
            .service(unit_start)
            .service(answer_question)
    })
    .bind(("0.0.0.0", bind_port))?
    .run()
    .await
}
