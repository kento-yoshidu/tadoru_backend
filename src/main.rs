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
