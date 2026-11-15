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
/// OVERWRITE / UPSERT にしてあるので毎回実行しても安全だが、本来は
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
        DELETE leads_to;
        DELETE choice;
        DELETE question;

        -- 結論(ゴール)
        UPSERT question:q1 SET body = '原始ピタゴラス数 (a,b,c) において、a,b はどのような偶奇になるか。';
        UPSERT choice:q1_a SET question = question:q1, label = 'A', body = '奇数 : 奇数', is_correct = false;
        UPSERT choice:q1_b SET question = question:q1, label = 'B', body = '偶数 : 奇数 または 奇数 : 偶数', is_correct = true;

        -- 前提知識(1): 奇数² mod 4
        UPSERT question:q2 SET body = '奇数の2乗を4で割った余りはいくつか。(ヒント: 奇数は 2k+1 と書ける)';
        UPSERT choice:q2_a SET question = question:q2, label = 'A', body = '0', is_correct = false;
        UPSERT choice:q2_b SET question = question:q2, label = 'B', body = '1', is_correct = true;
        UPSERT choice:q2_c SET question = question:q2, label = 'C', body = '2', is_correct = false;
        UPSERT choice:q2_d SET question = question:q2, label = 'D', body = '奇数によって変わる', is_correct = false;

        -- 補足ブランチ: q2 の「なぜ 1 なのか」
        -- (本流の設問には branch_of を書かない = NONE)
        UPSERT question:q2s1 SET
            branch_of = question:q2,
            body = '奇数 2k+1 を2乗して、4 でくくった形はどれか。';
        UPSERT choice:q2s1_a SET question = question:q2s1, label = 'A', body = '4(k²+k)+1', is_correct = true;
        UPSERT choice:q2s1_b SET question = question:q2s1, label = 'B', body = '4k²+1', is_correct = false;
        UPSERT choice:q2s1_c SET question = question:q2s1, label = 'C', body = '4(k²+k)+2', is_correct = false;
        -- q2s1_a には leads_to を張らない = 補足ブランチ終了

        -- 前提知識(2): 平方数 mod 4
        UPSERT question:q3 SET body = '整数の2乗(平方数)を4で割った余りとして、ありえるものはどれか。';
        UPSERT choice:q3_a SET question = question:q3, label = 'A', body = '0 と 1 だけ', is_correct = true;
        UPSERT choice:q3_b SET question = question:q3, label = 'B', body = '0 と 2 だけ', is_correct = false;
        UPSERT choice:q3_c SET question = question:q3, label = 'C', body = '1 と 3 だけ', is_correct = false;
        UPSERT choice:q3_d SET question = question:q3, label = 'D', body = '0, 1, 2, 3 のすべて', is_correct = false;

        -- 奇数 : 奇数 の否定
        UPSERT question:q4 SET body = 'なぜ a, b が「奇数 : 奇数」になることはありえないのか。';
        UPSERT choice:q4_a SET question = question:q4, label = 'A', body = '奇数²は奇数だから', is_correct = false;
        UPSERT choice:q4_b SET question = question:q4, label = 'B', body = '奇数²+奇数²は4で割ると2余るが、そのような平方数 c² は存在しないから', is_correct = true;
        UPSERT choice:q4_c SET question = question:q4, label = 'C', body = '奇数²+奇数²は奇数になるから', is_correct = false;

        -- 偶数 : 偶数 の否定
        UPSERT question:q5 SET body = 'では、なぜ「偶数 : 偶数」もありえないのか。';
        UPSERT choice:q5_a SET question = question:q5, label = 'A', body = '偶数²+偶数²は必ず奇数になるから', is_correct = false;
        UPSERT choice:q5_b SET question = question:q5, label = 'B', body = 'a, b, c がすべて2で割り切れ、互いに素(原始的)という条件に反するから', is_correct = true;
        UPSERT choice:q5_c SET question = question:q5, label = 'C', body = '偶数²は4で割ると余りが2になるから', is_correct = false;

        -- 仕上げ: c の偶奇
        UPSERT question:q6 SET body = 'a, b の一方が偶数で他方が奇数のとき、c の偶奇はどうなるか。';
        UPSERT choice:q6_a SET question = question:q6, label = 'A', body = '偶数', is_correct = false;
        UPSERT choice:q6_b SET question = question:q6, label = 'B', body = '奇数', is_correct = true;
        UPSERT choice:q6_c SET question = question:q6, label = 'C', body = 'a, b の値によって変わる', is_correct = false;

        RELATE choice:q1_b -> leads_to -> question:q2;
        RELATE choice:q2_b -> leads_to -> question:q3;
        RELATE choice:q2_b -> leads_to -> question:q2s1;  -- 2本目: 補足ブランチの入口
        RELATE choice:q3_a -> leads_to -> question:q4;
        RELATE choice:q4_b -> leads_to -> question:q5;
        RELATE choice:q5_b -> leads_to -> question:q6;
        -- q6_b には leads_to を張らない = 正解でユニット完了

        UPSERT unit:pythagorean_parity SET
            title = '原始ピタゴラス数の偶奇性を証明する',
            description = 'a, b の偶奇を mod 4 の議論から導く',
            start_question = question:q1;

        -- ===== unit: ターメリックと黒コショウ =====
        -- 成分 → 吸収の概念 → 吸収されにくい理由 → 代謝の仕組み → 黒コショウの成分
        -- → 作用の仕組み → 効果の大きさ → 注意点 → 数字の読み方

        -- t1: ターメリックの主成分
        UPSERT question:turmeric1 SET
            body = 'カレーの黄色のもとになるスパイス、ターメリック(ウコン)。その黄色い色素であり、健康効果の研究対象としてよく取り上げられる主な成分はどれか。',
            explanation = 'クルクミンはポリフェノールの一種で、ターメリックの黄色のもと。抗炎症作用などが研究されてきたが、「摂っても体にほとんど取り込まれない」という大きな弱点がある。その弱点を理解するために、まず「体に取り込まれる」とはどういうことかを考える。';
        UPSERT choice:turmeric1_a SET question = question:turmeric1, label = 'A', body = 'カプサイシン', is_correct = false,
            feedback = 'カプサイシンは唐辛子の辛味成分。';
        UPSERT choice:turmeric1_b SET question = question:turmeric1, label = 'B', body = 'クルクミン', is_correct = true;
        UPSERT choice:turmeric1_c SET question = question:turmeric1, label = 'C', body = 'ピペリン', is_correct = false,
            feedback = 'ピペリンは黒コショウの辛味成分。このユニットの後半で登場する。';
        UPSERT choice:turmeric1_d SET question = question:turmeric1, label = 'D', body = 'アントシアニン', is_correct = false,
            feedback = 'アントシアニンはブルーベリーや紫芋などに含まれる青紫色の色素。';

        -- t2: バイオアベイラビリティとは
        UPSERT question:turmeric2 SET
            body = '栄養学や薬学でいう「バイオアベイラビリティ(生物学的利用能)」とは、何を表す指標か。',
            explanation = '口から摂った成分は、腸で吸収され、肝臓を通ってから全身の血液に入る。その途中で吸収されなかったり分解されたりすると、血中に届く量は減る。この「届く割合」がバイオアベイラビリティ。次は、クルクミンの場合にこの割合がどうなるかを見る。';
        UPSERT choice:turmeric2_a SET question = question:turmeric2, label = 'A', body = '食品1gあたりに含まれる成分の量', is_correct = false,
            feedback = 'それは含有量。たくさん含まれていても、体に取り込まれなければ意味がない。';
        UPSERT choice:turmeric2_b SET question = question:turmeric2, label = 'B', body = '成分が体内で効果を発揮し始めるまでの時間', is_correct = false,
            feedback = 'それは効き始めの速さで、別の指標。';
        UPSERT choice:turmeric2_c SET question = question:turmeric2, label = 'C', body = '摂取した成分のうち、変化せずに血液(全身の循環)まで届いて利用できる割合', is_correct = true;
        UPSERT choice:turmeric2_d SET question = question:turmeric2, label = 'D', body = '成分が体の外に排出されるまでの時間', is_correct = false,
            feedback = '体内にとどまる時間(半減期など)は別の指標。';

        -- t3: クルクミンのバイオアベイラビリティが低い理由
        UPSERT question:turmeric3 SET
            body = 'クルクミンはバイオアベイラビリティが非常に低いことで知られる。その理由として最も適切なものはどれか。',
            explanation = 'クルクミンは脂溶性で水に溶けにくく、そもそも腸からの吸収が悪い。さらに、吸収された分も腸の壁や肝臓で素早く代謝され、すぐに排出されてしまう。この「吸収後の代謝」が鍵になるので、次はその代謝の中身を見る。';
        UPSERT choice:turmeric3_a SET question = question:turmeric3, label = 'A', body = '胃酸で完全に分解され、腸まで届かないから', is_correct = false,
            feedback = '胃酸で全部分解されるわけではない。主な問題は、腸での吸収されにくさと、吸収後の速い代謝。';
        UPSERT choice:turmeric3_b SET question = question:turmeric3, label = 'B', body = '水に溶けにくいため腸から吸収されにくく、吸収されても腸の壁や肝臓ですぐ代謝されて排出されるから', is_correct = true;
        UPSERT choice:turmeric3_c SET question = question:turmeric3, label = 'C', body = '分子が大きすぎて、腸の壁を一切通過できないから', is_correct = false,
            feedback = '一切通過しないわけではない。少しは吸収されるが、その後すぐ代謝されてしまう点が大きい。';
        UPSERT choice:turmeric3_d SET question = question:turmeric3, label = 'D', body = '腸内細菌がすべて分解してしまうから', is_correct = false,
            feedback = '腸内細菌による代謝も一部あるが、「すべて」ではなく、主な理由でもない。';

        -- t4: グルクロン酸抱合
        UPSERT question:turmeric4 SET
            body = 'クルクミンの代謝で中心となる「グルクロン酸抱合」は、肝臓や腸の壁で行われる。これは体にとってどのような役割を持つ代謝か。',
            explanation = 'グルクロン酸抱合は、薬や体の外から入ってきた物質を水に溶けやすい形に変えて排出する、いわば「体の掃除の仕組み」。クルクミンはこの仕組みですぐに処理されてしまう。では、この掃除を一時的に遅らせることができたら? ここで黒コショウが登場する。';
        UPSERT choice:turmeric4_a SET question = question:turmeric4, label = 'A', body = '成分にグルクロン酸をくっつけて水に溶けやすくし、尿や胆汁で体の外へ出しやすくする', is_correct = true;
        UPSERT choice:turmeric4_b SET question = question:turmeric4, label = 'B', body = '成分を分解してエネルギーに変える', is_correct = false,
            feedback = 'エネルギーを作る代謝ではない。体の外に出すための変換。';
        UPSERT choice:turmeric4_c SET question = question:turmeric4, label = 'C', body = '成分を脂肪組織に蓄え、必要なときに放出する', is_correct = false,
            feedback = 'むしろ逆で、体の外に出しやすくする仕組み。';
        UPSERT choice:turmeric4_d SET question = question:turmeric4, label = 'D', body = '成分の効き目を強めるために、より作用の強い形に変える', is_correct = false,
            feedback = '抱合は多くの場合、成分を作用の弱い、排出しやすい形に変える。';

        -- t5: 黒コショウの辛味成分
        UPSERT question:turmeric5 SET
            body = '黒コショウのピリッとした辛味のもとになっている主な成分はどれか。',
            explanation = 'ピペリンはアルカロイドの一種で、黒コショウの辛味のもと。実はこのピペリンには、体内での代謝を妨げる作用があることが知られている。';
        UPSERT choice:turmeric5_a SET question = question:turmeric5, label = 'A', body = 'カプサイシン', is_correct = false,
            feedback = 'カプサイシンは唐辛子の辛味成分。';
        UPSERT choice:turmeric5_b SET question = question:turmeric5, label = 'B', body = 'アリシン', is_correct = false,
            feedback = 'アリシンはニンニクの刺激臭・辛味のもと。';
        UPSERT choice:turmeric5_c SET question = question:turmeric5, label = 'C', body = 'ジンゲロール', is_correct = false,
            feedback = 'ジンゲロールはショウガの辛味成分。';
        UPSERT choice:turmeric5_d SET question = question:turmeric5, label = 'D', body = 'ピペリン', is_correct = true;

        -- t6: ピペリンがクルクミンの吸収率を上げる仕組み
        UPSERT question:turmeric6 SET
            body = 'クルクミンは黒コショウと一緒に摂ると、血中濃度が大きく上がることが報告されている。ここまでの内容を踏まえて、その理由として最も適切なものはどれか。',
            explanation = '黒コショウのピペリンは、腸の壁や肝臓でのグルクロン酸抱合などの代謝を妨げる。そのため、体の外へ出されるはずだったクルクミンが分解されないまま血中に残りやすくなる。では、実際にどのくらい上がるのか。';
        UPSERT choice:turmeric6_a SET question = question:turmeric6, label = 'A', body = '黒コショウの辛味で体が温まり血流が増えるため、クルクミンが全身に速く運ばれるから', is_correct = false,
            feedback = '体が温まる感覚や血流の変化は、主な理由ではない。問題なのは「吸収後すぐ代謝されて排出されてしまう」ことのほう。';
        UPSERT choice:turmeric6_b SET question = question:turmeric6, label = 'B', body = 'ピペリンが腸の壁や肝臓でのグルクロン酸抱合などの代謝を妨げるため、分解されないクルクミンが血中に残りやすくなるから', is_correct = true;
        UPSERT choice:turmeric6_c SET question = question:turmeric6, label = 'C', body = 'ピペリンがクルクミンと化学的に結合し、効き目の強い別の化合物に変わるから', is_correct = false,
            feedback = 'ピペリンはクルクミンを別の物質に変えるわけではない。クルクミンが代謝されるのを遅らせるだけ。';
        UPSERT choice:turmeric6_d SET question = question:turmeric6, label = 'D', body = '黒コショウ自体にもクルクミンが含まれており、摂る量そのものが増えるから', is_correct = false,
            feedback = 'クルクミンはターメリックに特徴的な成分で、黒コショウにはほとんど含まれていない。黒コショウ側の主な成分はピペリン。';

        -- t7: 効果の大きさ(% の読み方)
        UPSERT question:turmeric7 SET
            body = 'ヒトを対象にした研究(Shoba ら, 1998)では、クルクミン 2g にピペリン 20mg を加えて摂ると、クルクミンだけの場合に比べて、バイオアベイラビリティが 2000% に高まったと報告されている(クルクミンだけの場合を 100% とする)。これはおおよそ何倍か。',
            explanation = 'クルクミンだけの場合を 100% とすると、2000% は約20倍。しかも加えたピペリンは 20mg とごく少量だった。代謝を遅らせるという仕組みの効き目の大きさがわかる。ただ、代謝を遅らせる作用は、本当にクルクミンにだけ働くのだろうか。';
        UPSERT choice:turmeric7_a SET question = question:turmeric7, label = 'A', body = '約1.2倍(2割増し)', is_correct = false,
            feedback = '2000% を 20% と読み違えていないか。';
        UPSERT choice:turmeric7_b SET question = question:turmeric7, label = 'B', body = '約2倍', is_correct = false,
            feedback = '2倍なら 200%。桁が1つ違う。';
        UPSERT choice:turmeric7_c SET question = question:turmeric7, label = 'C', body = '約20倍', is_correct = true;
        UPSERT choice:turmeric7_d SET question = question:turmeric7, label = 'D', body = '約2000倍', is_correct = false,
            feedback = '% をそのまま「倍」と読んでしまっている。100% が1倍なので、2000% は 20倍。';

        -- t8: 注意点(医薬品との相互作用)
        UPSERT question:turmeric8 SET
            body = 'ピペリンが代謝を妨げるという仕組みを踏まえると、ターメリックとピペリンを組み合わせたサプリメントを日常的に摂る際の注意点として、最も適切なものはどれか。',
            explanation = '体の代謝の仕組みは、特定の成分専用ではない。そのためピペリンは、一部の医薬品の代謝も遅らせ、血中濃度を上げる可能性が報告されている。「吸収がよくなる」は良い面ばかりではない。最後に、「吸収率が上がった」というデータそのものの読み方を考える。';
        UPSERT choice:turmeric8_a SET question = question:turmeric8, label = 'A', body = '同じ代謝の仕組みで処理される医薬品の血中濃度まで上がり、薬が効きすぎるおそれがある。薬を飲んでいる人は医師や薬剤師に相談する', is_correct = true;
        UPSERT choice:turmeric8_b SET question = question:turmeric8, label = 'B', body = 'ピペリンは辛味成分なので、辛いものが苦手な人でなければ気にしなくてよい', is_correct = false,
            feedback = '味の問題ではない。代謝を妨げる作用は、クルクミン以外の物質にも及びうる。';
        UPSERT choice:turmeric8_c SET question = question:turmeric8, label = 'C', body = '代謝が妨げられるのはクルクミンだけなので、特に注意点はない', is_correct = false,
            feedback = '体の代謝の仕組みは特定の成分専用ではない。ほかの薬の代謝も妨げられうる。';
        UPSERT choice:turmeric8_d SET question = question:turmeric8, label = 'D', body = '吸収率が上がるので、ターメリックの量を増やすほど効果も比例して高まる', is_correct = false,
            feedback = '量を増やせば効果が比例して高まるとは言えない。とりすぎによる副作用のリスクも高まりうる。';

        -- t9: 「吸収率が上がる」と「健康効果」は別(ユニット最終問題)
        UPSERT question:turmeric9 SET
            body = 'あるサプリメントの広告に「黒コショウ配合でクルクミンの吸収率が20倍! だから健康効果も20倍!」と書かれていた。この主張に対する評価として最も適切なものはどれか。',
            explanation = '血中濃度は「効果が出るための前提条件」にすぎず、それ自体は健康上の効果ではない(代理の指標)。吸収率が20倍になっても効果が20倍になるとは限らないし、そもそも効果があるかどうかは、実際の健康状態を指標にした比較試験で確かめる必要がある。仕組みを知ったうえで、その数字が何を測っているのかを見極めよう。';
        UPSERT choice:turmeric9_a SET question = question:turmeric9, label = 'A', body = '研究で吸収率20倍が示されている以上、健康効果も20倍と考えてよい', is_correct = false,
            feedback = '吸収率は血中濃度の話。それが病気の予防や改善につながるかどうかは、別に確かめる必要がある。';
        UPSERT choice:turmeric9_b SET question = question:turmeric9, label = 'B', body = 'サプリメントの広告なので、吸収率が上がるという話も含めてすべて誤りである', is_correct = false,
            feedback = '吸収率が上がるという研究結果そのものを否定する根拠はない。問題は、そこから健康効果へ飛躍していること。';
        UPSERT choice:turmeric9_c SET question = question:turmeric9, label = 'C', body = '健康効果は吸収率と無関係なので、黒コショウを加える意味はまったくない', is_correct = false,
            feedback = '血中に届くことは、効果が出るための前提条件の一つ。無関係とまでは言えない。';
        UPSERT choice:turmeric9_d SET question = question:turmeric9, label = 'D', body = '吸収率(血中濃度)が上がることは示されていても、それが健康上の効果にどれだけ結びつくかは、効果そのものを測った研究で別に確かめる必要がある', is_correct = true;

        RELATE choice:turmeric1_b -> leads_to -> question:turmeric2;
        RELATE choice:turmeric2_c -> leads_to -> question:turmeric3;
        RELATE choice:turmeric3_b -> leads_to -> question:turmeric4;
        RELATE choice:turmeric4_a -> leads_to -> question:turmeric5;
        RELATE choice:turmeric5_d -> leads_to -> question:turmeric6;
        RELATE choice:turmeric6_b -> leads_to -> question:turmeric7;
        RELATE choice:turmeric7_c -> leads_to -> question:turmeric8;
        RELATE choice:turmeric8_a -> leads_to -> question:turmeric9;
        -- turmeric9_d には leads_to を張らない = 正解でユニット完了

        UPSERT unit:turmeric_pepper SET
            title = 'ターメリックと黒コショウの食べ合わせ',
            description = 'クルクミンが吸収されにくい理由から、黒コショウ(ピペリン)で吸収率が上がる仕組み、その数字の読み方まで',
            start_question = question:turmeric1;
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
