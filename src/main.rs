mod model;
use actix_web::{
    App, HttpResponse, HttpServer, get, http::header, web,
};
use actix_cors::Cors;
use serde::Serialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;
use model::{
    UnitRow,
    UnitResponse,
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

        -- RELATE は毎回実行すると重複行が増えるので、先に張り直す
        DELETE leads_to;

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
    })
    .bind(("0.0.0.0", bind_port))?
    .run()
    .await
}
