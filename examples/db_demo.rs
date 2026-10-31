//! SurrealDB(embedded, surrealkv engine)の使い方を試すための最小サンプル。
//!
//! 実行: `cargo run --example db_demo`
//!
//! 別プロセスの DB サーバーは不要。`target/db_demo.db` フォルダにファイルとして永続化される。
//! docs/SPEC.md で設計した question / choice / leads_to のミニ版を、実際に
//! CREATE・RELATE・SELECT してみる。

use surrealdb::Surreal;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::types::Value;

fn main() -> surrealdb::Result<()> {
    println!("Example");

    // SurrealDB(組み込み)はデフォルトのスタックサイズ(数MB)では
    // オーバーフローすることがあるため、スタックを増やした別スレッドで実行する。
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| actix_web::rt::System::new().block_on(run()))
        .expect("failed to spawn worker thread")
        .join()
        .expect("worker thread panicked")
}

async fn run() -> surrealdb::Result<()> {
    // 1. 接続する。サーバー起動は不要、ライブラリとして組み込まれる。
    let db: Surreal<Db> = Surreal::new::<SurrealKv>("target/db_demo.db").await?;
    db.use_ns("tadoru").use_db("dev").await?;

    // 2. スキーマを定義する(今回は簡略化して SCHEMALESS)。
    //    leads_to は choice -> question のグラフエッジ専用テーブル。
    //    OVERWRITE にして、既に定義済みでもエラーにならないようにする。
    //    データはファイルに永続化される(target/db_demo.db)ので、前回実行分のレコードが
    //    残っていると CREATE が「既に存在する」で失敗する。毎回きれいな状態から
    //    試せるよう、定義し直した直後に既存データを消しておく。
    db.query(
        r#"
        DEFINE TABLE OVERWRITE question SCHEMALESS;
        DEFINE TABLE OVERWRITE choice SCHEMALESS;
        DEFINE TABLE OVERWRITE leads_to TYPE RELATION FROM choice TO question SCHEMALESS;
        DELETE question;
        DELETE choice;
        DELETE leads_to;
        "#,
    )
    .await?
    .check()?;

    // 3. レコードを作成する(CREATE)。ID を明示すれば choice:q1_b のように参照できる。
    db.query(
        r#"
        CREATE question:q1 SET body = $q1_body;
        CREATE choice:q1_a SET question = question:q1, label = 'A', body = '奇数 : 奇数', is_correct = false;
        CREATE choice:q1_b SET question = question:q1, label = 'B', body = '偶数 : 奇数 または 奇数 : 偶数', is_correct = true;
        CREATE question:q2 SET body = 'なぜ「奇数 : 奇数」はありえないのか。';
        "#,
    )
    .bind((
        "q1_body",
        "原始ピタゴラス数 (a,b,c) において、a,b はどのような偶奇になるか。",
    ))
    .await?
    .check()?;
    println!("--- question / choice を作成した ---");

    // 4. 正解の選択肢(choice:q1_b)から次の設問(question:q2)へグラフエッジを張る。
    //    誤答(choice:q1_a)には張らない = 「次が無い」を自然に表現できる。
    db.query("RELATE choice:q1_b -> leads_to -> question:q2;").await?.check()?;
    println!("--- choice:q1_b -> question:q2 にエッジを張った ---\n");

    // 5. ある設問の選択肢一覧を取得する。
    let mut res =
        db.query("SELECT label, body FROM choice WHERE question = question:q1 ORDER BY label;").await?;
    let choices: Vec<Value> = res.take(0)?;
    println!("--- Q1 の選択肢 ---");
    for c in &choices {
        println!("{c:?}");
    }

    // 6. 正解(choice:q1_b)を選んだ場合の「次の設問」をグラフ traversal で取得する。
    let mut res = db.query("SELECT ->leads_to->question.* AS next FROM ONLY choice:q1_b;").await?;
    let next: Value = res.take(0)?;
    println!("\n--- choice:q1_b(正解)を選んだ時の次の設問 ---");
    println!("{next:?}");

    // 7. 誤答(choice:q1_a)には leads_to エッジが無いので、次の設問は空になる。
    let mut res = db.query("SELECT ->leads_to->question.* AS next FROM ONLY choice:q1_a;").await?;
    let next: Value = res.take(0)?;
    println!("\n--- choice:q1_a(誤答)を選んだ時の次の設問(無いはず) ---");
    println!("{next:?}");

    Ok(())
}
