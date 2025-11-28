// src/routes/strategy.rs

use axum::{
    http::StatusCode,
    routing::{get, post, put, delete},
    extract::{Json, State, Path},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::strategies::storage::StrategyStorage;
use crate::strategies::manager::{StrategyRunner, InstanceInfo};
use crate::ffi_types::CEvent;

// ═══════════════════════════════════════════════════════════
// STATE
// ═══════════════════════════════════════════════════════════

#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<StrategyStorage>,
    pub runner: Arc<StrategyRunner>,
    pub event_tx: broadcast::Sender<CEvent>,
}

// ═══════════════════════════════════════════════════════════
// REQUESTS
// ═══════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct CreateRequest {
    pub id: String,
    pub code: String,
}

#[derive(Deserialize)]
pub struct CodeRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct StartRequest {
    pub symbol: String,
    #[serde(default)]
    pub params: Value,
}

// ═══════════════════════════════════════════════════════════
// RESPONSES
// ═══════════════════════════════════════════════════════════

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
    
    fn created(data: T) -> (StatusCode, Json<Self>) {
        (StatusCode::CREATED, Json(Self { ok: true, error: None, data: Some(data) }))
    }
    
    // Generic error - работает для любого T
    fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        (status, Json(Self { ok: false, error: Some(msg.into()), data: None }))
    }
}

impl ApiResult<()> {
    fn ok_empty() -> (StatusCode, Json<Self>) {
        (StatusCode::OK, Json(Self { ok: true, error: None, data: None }))
    }
    
    fn created_empty() -> (StatusCode, Json<Self>) {
        (StatusCode::CREATED, Json(Self { ok: true, error: None, data: None }))
    }
}

#[derive(Serialize)]
pub struct StrategyDetail {
    pub id: String,
    pub code: String,
    pub compiled: bool,
    pub instances: Vec<InstanceInfo>,
}

#[derive(Serialize)]
pub struct StrategyListItem {
    pub id: String,
    pub compiled: bool,
    pub instances: usize,
}

#[derive(Serialize)]
pub struct CompileResult {
    pub success: bool,
    pub errors: Vec<String>,
}

// ═══════════════════════════════════════════════════════════
// ROUTER
// ═══════════════════════════════════════════════════════════

pub fn routes(state: AppState) -> Router {
    Router::new()
        // Стратегии
        .route("/strategies", get(list_strategies))
        .route("/strategies", post(create_strategy))
        .route("/strategies/{id}", get(get_strategy))
        .route("/strategies/{id}", delete(delete_strategy))
        .route("/strategies/{id}/code", put(update_code))
        .route("/strategies/{id}/compile", post(compile))
        
        // Запуск/остановка
        .route("/strategies/{id}/start", post(start))
        .route("/strategies/{id}/stop", post(stop_all))
        .route("/strategies/{id}/stop/{symbol}", post(stop_one))
        
        // Инстансы
        .route("/instances", get(list_instances))
        .route("/instances/{instance_id}", get(get_instance))
        .route("/instances/{instance_id}/stop", post(stop_instance))
        
        .with_state(state)
}

// ═══════════════════════════════════════════════════════════
// СТРАТЕГИИ
// ═══════════════════════════════════════════════════════════

async fn list_strategies(State(s): State<AppState>) -> Json<Vec<StrategyListItem>> {
    let list = s.storage.list().unwrap_or_default();
    
    Json(list.into_iter().map(|info| {
        StrategyListItem {
            id: info.id.clone(),
            compiled: info.compiled,
            instances: s.runner.list_for(&info.id).len(),
        }
    }).collect())
}


async fn create_strategy(
    State(s): State<AppState>,
    Json(req): Json<CreateRequest>,
) -> (StatusCode, Json<ApiResult>) {
    if let Err(e) = s.storage.create(&req.id, &req.code) {
        return ApiResult::err(StatusCode::BAD_REQUEST, e.to_string());
    }
    
    let _ = s.storage.compile(&req.id);
    
    ApiResult::created_empty()
}

async fn get_strategy(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ApiResult<StrategyDetail>>), (StatusCode, Json<ApiResult<StrategyDetail>>)> {
    let code = s.storage.get_code(&id)
        .map_err(|e| ApiResult::<StrategyDetail>::err(StatusCode::NOT_FOUND, e.to_string()))?;
    
    let compiled = s.storage.get_lib_path(&id).is_ok();
    let instances = s.runner.list_for(&id);
    
    Ok(ApiResult::ok(StrategyDetail { id, code, compiled, instances }))
}

async fn delete_strategy(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResult>) {
    s.runner.stop_all(&id).await;
    
    match s.storage.delete(&id) {
        Ok(_) => ApiResult::ok_empty(),
        Err(e) => ApiResult::err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

async fn update_code(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CodeRequest>,
) -> (StatusCode, Json<ApiResult<CompileResult>>) {
    if !s.runner.list_for(&id).is_empty() {
        return ApiResult::err(StatusCode::CONFLICT, "Stop all instances first");
    }
    
    if let Err(e) = s.storage.update_code(&id, &req.code) {
        return ApiResult::err(StatusCode::NOT_FOUND, e.to_string());
    }
    
    match s.storage.compile(&id) {
        Ok(r) => ApiResult::ok(CompileResult { success: r.success, errors: r.errors }),
        Err(e) => ApiResult::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn compile(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResult<CompileResult>>) {
    match s.storage.compile(&id) {
        Ok(r) => {
            let status = if r.success { StatusCode::OK } else { StatusCode::BAD_REQUEST };
            (status, Json(ApiResult {
                ok: r.success,
                error: if r.success { None } else { Some("Compilation failed".into()) },
                data: Some(CompileResult { success: r.success, errors: r.errors }),
            }))
        }
        Err(e) => ApiResult::err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

async fn start(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<StartRequest>,
) -> (StatusCode, Json<ApiResult<InstanceInfo>>) {
    if !s.storage.exists(&id) {
        return ApiResult::err(StatusCode::NOT_FOUND, "Strategy not found");
    }
    
    let lib_path = match s.storage.get_lib_path(&id) {
        Ok(p) => p,
        Err(_) => {
            match s.storage.compile(&id) {
                Ok(r) if r.success => r.lib_path.unwrap(),
                Ok(r) => return ApiResult::err(
                    StatusCode::BAD_REQUEST, 
                    format!("Compilation failed: {}", r.errors.join("; "))
                ),
                Err(e) => return ApiResult::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
    };
    
    match s.runner.start(
        id,
        req.symbol,
        lib_path,
        s.event_tx.subscribe(),
        req.params,
    ).await {
        Ok(info) => ApiResult::ok(info),
        Err(e) => ApiResult::err(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

async fn stop_all(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResult<Vec<String>>>) {
    let stopped = s.runner.stop_all(&id).await;
    
    if stopped.is_empty() {
        ApiResult::err(StatusCode::NOT_FOUND, "No running instances")
    } else {
        ApiResult::ok(stopped)
    }
}

async fn stop_one(
    State(s): State<AppState>,
    Path((id, symbol)): Path<(String, String)>,
) -> (StatusCode, Json<ApiResult>) {
    let instance_id = format!("{}:{}", id, symbol.to_uppercase());
    
    match s.runner.stop(&instance_id).await {
        Ok(_) => ApiResult::ok_empty(),
        Err(e) => ApiResult::err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

async fn stop_instance(
    State(s): State<AppState>,
    Path(instance_id): Path<String>,
) -> (StatusCode, Json<ApiResult>) {
    match s.runner.stop(&instance_id).await {
        Ok(_) => ApiResult::ok_empty(),
        Err(e) => ApiResult::err(StatusCode::NOT_FOUND, e.to_string()),
    }
}

async fn get_instance(
    State(s): State<AppState>,
    Path(instance_id): Path<String>,
) -> Result<Json<InstanceInfo>, (StatusCode, Json<ApiResult<InstanceInfo>>)> {
    s.runner.get(&instance_id)
        .map(Json)
        .ok_or_else(|| ApiResult::<InstanceInfo>::err(StatusCode::NOT_FOUND, "Not found"))
}

// ═══════════════════════════════════════════════════════════
// ИНСТАНСЫ
// ═══════════════════════════════════════════════════════════

async fn list_instances(State(s): State<AppState>) -> Json<Vec<InstanceInfo>> {
    Json(s.runner.list())
}

// async fn get_instance(
//     State(s): State<AppState>,
//     Path(instance_id): Path<String>,
// ) -> Result<Json<InstanceInfo>, (StatusCode, Json<ApiResult>)> {
//     s.runner.get(&instance_id)
//         .map(Json)
//         .ok_or_else(|| ApiResult::err(StatusCode::NOT_FOUND, "Not found"))
// }