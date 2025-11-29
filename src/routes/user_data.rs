// src/routes/user_data.rs

use axum::{
    routing::{get, post, delete},
    extract::{Json, State},
    Router,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Импортируем StreamInfo из user_data::manager
use crate::user_data::manager::{UserDataManager, StreamInfo};

#[derive(Clone)]
pub struct UserDataState {
    pub manager: Arc<UserDataManager>,
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub api_key: String,
    pub secret_key: String,
}

#[derive(Serialize)]
pub struct ApiResult<T = ()> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T: Serialize> ApiResult<T> {
    fn ok(data: T) -> (StatusCode, Json<Self>) {
        (StatusCode::OK, Json(Self { ok: true, error: None, data: Some(data) }))
    }
    
    fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        (status, Json(Self { ok: false, error: Some(msg.into()), data: None }))
    }
}

impl ApiResult<()> {
    fn ok_empty() -> (StatusCode, Json<Self>) {
        (StatusCode::OK, Json(Self { ok: true, error: None, data: None }))
    }
}

pub fn routes(state: UserDataState) -> Router {
    Router::new()
        .route("/userdata/streams", get(list_streams))
        .route("/userdata/streams", post(connect_stream))
        .route("/userdata/streams", delete(disconnect_stream))
        .with_state(state)
}

async fn list_streams(State(s): State<UserDataState>) -> Json<Vec<StreamInfo>> {
    // Просто возвращаем список, который генерирует manager
    Json(s.manager.list())
}

async fn connect_stream(
    State(s): State<UserDataState>,
    Json(req): Json<ConnectRequest>,
) -> (StatusCode, Json<ApiResult<StreamInfo>>) {
    // Connect теперь возвращает Receiver<CEvent>, но для API нам важен только факт успеха
    match s.manager.connect(req.api_key.clone(), req.secret_key).await {
        Ok(_) => {
            let streams = s.manager.list();
            let hash = sha2_hash(&req.api_key);
            
            if let Some(info) = streams.into_iter().find(|i| i.api_key_hash == hash) {
                ApiResult::ok(info)
            } else {
                ApiResult::err(StatusCode::INTERNAL_SERVER_ERROR, "Stream not found after connect")
            }
        }
        Err(e) => ApiResult::err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn disconnect_stream(
    State(s): State<UserDataState>,
    Json(req): Json<ConnectRequest>,
) -> (StatusCode, Json<ApiResult>) {
    match s.manager.disconnect(&req.api_key).await {
        Ok(_) => ApiResult::ok_empty(),
        Err(e) => ApiResult::err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

fn sha2_hash(s: &str) -> String {
    use sha2::{Sha256, Digest};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}