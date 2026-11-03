use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, RecordIdKey, SurrealValue, ToSql};

/// DB から取得する生のレコード形
#[derive(Debug, SurrealValue)]
pub struct UnitRow {
    pub id: RecordId,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnitResponse {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, SurrealValue)]
pub struct QuestionRow {
    pub id: RecordId,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct QuestionResponse {
    pub id: String,
    pub body: String,
    pub choices: Vec<ChoiceResponse>,
}

impl QuestionResponse {
    /// choices は別クエリ(または別フィールド)で取ってくるので、
    /// QuestionRow 単体からの From ではなく、組み合わせて作る専用関数にする
    pub fn from_parts(row: QuestionRow, choices: Vec<ChoiceRow>) -> Self {
        Self {
            id: record_key(&row.id),
            body: row.body,
            choices: choices.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, SurrealValue)]
pub struct ChoiceRow {
    pub id: RecordId,
    pub label: String,
    pub body: String,
}

#[derive(Debug, Serialize)]
pub struct ChoiceResponse {
    pub id: String,
    pub label: String,
    pub body: String,
}

impl From<ChoiceRow> for ChoiceResponse {
    fn from(row: ChoiceRow) -> Self {
        Self {
            id: record_key(&row.id),
            label: row.label,
            body: row.body,
        }
    }
}

/// POST /api/questions/:id/answer のリクエストボディ
#[derive(Debug, Deserialize)]
pub struct AnswerRequest {
    pub choice_id: String,
}

/// POST /api/questions/:id/answer のレスポンス。
#[derive(Debug, Serialize)]
pub struct AnswerResponse {
    pub correct: bool,
    pub next_question: Option<QuestionResponse>,
    pub unit_completed: bool,
}

/// `SELECT ->leads_to->question.* AS next FROM ONLY $choice_id;` の結果の形
/// エッジが無ければ next は空配列になる
#[derive(Debug, SurrealValue)]
pub struct NextQuestionRow {
    pub next: Vec<QuestionRow>,
}

#[derive(Debug, SurrealValue)]
pub struct CorrectnessRow {
    pub is_correct: bool,
}

pub fn record_key(id: &RecordId) -> String {
    match &id.key {
        RecordIdKey::String(s) => s.clone(),
        key => key.to_sql(),
    }
}

impl From<UnitRow> for UnitResponse {
    fn from(row: UnitRow) -> Self {
        Self {
            id: record_key(&row.id),
            title: row.title,
            description: row.description,
        }
    }
}