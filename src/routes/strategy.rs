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

use crate::strategies::storage::{StrategyStorage, Strategy, StrategyMetadata};
use crate::strategies::manager::StrategyRunner;
use crate::ffi_types::CEvent;

// ═══════════════════════════════════════════════════════════
// REQUEST/RESPONSE ТИПЫ
// ═══════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct CreateStrategyRequest {
    pub id: String,
    pub name: String,
    pub symbol: String,
    pub code: String,
}

#[derive(Deserialize)]
pub struct UpdateCodeRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct UpdateMetadataRequest {
    pub name: Option<String>,
    pub symbol: Option<String>,
    pub enabled: Option<bool>,
    pub open_positions: Option<bool>,
}

#[derive(Deserialize)]
pub struct StartInstanceRequest {
    pub symbol: String,
    pub params: Option<Value>,  // JSON объект с параметрами
}

#[derive(Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct StrategyResponse {
    pub metadata: StrategyMetadata,
    pub code: String,
}

#[derive(Serialize)]
pub struct CompilationResponse {
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lib_path: Option<String>,
}

// ═══════════════════════════════════════════════════════════
// STATE для runtime роутов
// ═══════════════════════════════════════════════════════════

type RuntimeState = (
    Arc<StrategyStorage>,
    Arc<StrategyRunner>,
    broadcast::Sender<CEvent>,
);

// ═══════════════════════════════════════════════════════════
// РОУТЕР ДЛЯ CRUD ОПЕРАЦИЙ
// ═══════════════════════════════════════════════════════════

pub fn strategy_routes(storage: Arc<StrategyStorage>) -> Router {
    Router::new()
        // CRUD операции
        .route("/strategies", post(create_strategy))
        .route("/strategies", get(list_strategies))
        .route("/strategies/:id", get(get_strategy))
        .route("/strategies/:id", delete(delete_strategy))
        
        // Работа с кодом
        .route("/strategies/:id/code", get(get_code))
        .route("/strategies/:id/code", put(update_code))
        
        // Работа с метаданными
        .route("/strategies/:id/metadata", put(update_metadata))
        
        // Компиляция и проверка
        .route("/strategies/:id/compile", post(compile_strategy))
        .route("/strategies/:id/check", post(check_strategy))
        
        .with_state(storage)
}

// ═══════════════════════════════════════════════════════════
// РОУТЕР ДЛЯ ЗАПУСКА/ОСТАНОВКИ
// ═══════════════════════════════════════════════════════════

pub fn runtime_routes(
    storage: Arc<StrategyStorage>,
    runner: Arc<StrategyRunner>,
    event_tx: broadcast::Sender<CEvent>,
) -> Router {
    Router::new()
        // Новый API (с instances)
        .route("/strategies/:id/instances/:symbol/start", post(start_instance))
        .route("/instances/:instance_id/stop", post(stop_instance))
        .route("/instances/running", get(list_running_instances))
        
        // Старый API (обратная совместимость)
        .route("/strategies/:id/start", post(start_strategy))
        .route("/strategies/:id/stop", post(stop_strategy))
        .route("/strategies/running", get(list_running))
        
        .with_state((storage, runner, event_tx))
}

// ═══════════════════════════════════════════════════════════
// CRUD ОПЕРАЦИИ
// ═══════════════════════════════════════════════════════════

/// POST /strategies - создать новую стратегию
async fn create_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Json(req): Json<CreateStrategyRequest>,
) -> (StatusCode, Json<ApiResponse>) {
    let strategy = Strategy::new(
        req.id.clone(),
        req.name,
        req.symbol,
        req.code,
    );
    
    match storage.create(strategy) {
        Ok(_) => {
            // Сразу пробуем скомпилировать
            match storage.compile(&req.id) {
                Ok(result) => {
                    if result.success {
                        (
                            StatusCode::CREATED,
                            Json(ApiResponse {
                                success: true,
                                message: format!("Strategy '{}' created and compiled", req.id),
                                data: Some(serde_json::json!({
                                    "id": req.id,
                                    "compiled": true,
                                })),
                            }),
                        )
                    } else {
                        (
                            StatusCode::CREATED,
                            Json(ApiResponse {
                                success: true,
                                message: format!(
                                    "Strategy '{}' created but compilation failed",
                                    req.id
                                ),
                                data: Some(serde_json::json!({
                                    "id": req.id,
                                    "compiled": false,
                                    "errors": result.errors,
                                })),
                            }),
                        )
                    }
                }
                Err(e) => {
                    (
                        StatusCode::CREATED,
                        Json(ApiResponse {
                            success: true,
                            message: format!(
                                "Strategy '{}' created but compilation error: {}",
                                req.id, e
                            ),
                            data: None,
                        }),
                    )
                }
            }
        }
        Err(e) => {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    success: false,
                    message: format!("Failed to create strategy: {}", e),
                    data: None,
                }),
            )
        }
    }
}

/// GET /strategies - список всех стратегий
async fn list_strategies(
    State(storage): State<Arc<StrategyStorage>>,
) -> (StatusCode, Json<Vec<StrategyMetadata>>) {
    match storage.list() {
        Ok(strategies) => (StatusCode::OK, Json(strategies)),
        Err(_) => (StatusCode::OK, Json(vec![])),
    }
}

/// GET /strategies/:id - получить стратегию
async fn get_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<StrategyResponse>), (StatusCode, Json<ApiResponse>)> {
    match storage.load(&id) {
        Ok(strategy) => Ok((
            StatusCode::OK,
            Json(StrategyResponse {
                metadata: strategy.metadata,
                code: strategy.code,
            }),
        )),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Strategy not found: {}", e),
                data: None,
            }),
        )),
    }
}

/// DELETE /strategies/:id - удалить стратегию
async fn delete_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResponse>) {
    match storage.delete(&id) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Strategy '{}' deleted", id),
                data: None,
            }),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to delete: {}", e),
                data: None,
            }),
        ),
    }
}

// ═══════════════════════════════════════════════════════════
// РАБОТА С КОДОМ
// ═══════════════════════════════════════════════════════════

/// GET /strategies/:id/code - получить только код
async fn get_code(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> Result<String, (StatusCode, Json<ApiResponse>)> {
    match storage.load(&id) {
        Ok(strategy) => Ok(strategy.code),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Strategy not found: {}", e),
                data: None,
            }),
        )),
    }
}

/// PUT /strategies/:id/code - обновить код
async fn update_code(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCodeRequest>,
) -> (StatusCode, Json<ApiResponse>) {
    match storage.update_code(&id, req.code) {
        Ok(_) => {
            // Проверяем синтаксис
            match storage.check(&id) {
                Ok(check_result) => {
                    if check_result.success {
                        // Компилируем
                        match storage.compile(&id) {
                            Ok(compile_result) => {
                                if compile_result.success {
                                    (
                                        StatusCode::OK,
                                        Json(ApiResponse {
                                            success: true,
                                            message: format!("Code updated and compiled for '{}'", id),
                                            data: Some(serde_json::json!({
                                                "compiled": true,
                                            })),
                                        }),
                                    )
                                } else {
                                    (
                                        StatusCode::OK,
                                        Json(ApiResponse {
                                            success: true,
                                            message: "Code updated but compilation failed".to_string(),
                                            data: Some(serde_json::json!({
                                                "compiled": false,
                                                "errors": compile_result.errors,
                                            })),
                                        }),
                                    )
                                }
                            }
                            Err(e) => (
                                StatusCode::OK,
                                Json(ApiResponse {
                                    success: true,
                                    message: format!("Code updated but compilation error: {}", e),
                                    data: None,
                                }),
                            ),
                        }
                    } else {
                        (
                            StatusCode::BAD_REQUEST,
                            Json(ApiResponse {
                                success: false,
                                message: "Code has syntax errors".to_string(),
                                data: Some(serde_json::json!({
                                    "errors": check_result.errors,
                                })),
                            }),
                        )
                    }
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse {
                        success: false,
                        message: format!("Check failed: {}", e),
                        data: None,
                    }),
                ),
            }
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to update code: {}", e),
                data: None,
            }),
        ),
    }
}

// ═══════════════════════════════════════════════════════════
// РАБОТА С МЕТАДАННЫМИ
// ═══════════════════════════════════════════════════════════

/// PUT /strategies/:id/metadata - обновить метаданные
async fn update_metadata(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateMetadataRequest>,
) -> (StatusCode, Json<ApiResponse>) {
    match storage.update_metadata(
        &id,
        req.name,
        req.symbol,
        req.enabled,
        req.open_positions,
    ) {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Metadata updated for '{}'", id),
                data: None,
            }),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to update metadata: {}", e),
                data: None,
            }),
        ),
    }
}

// ═══════════════════════════════════════════════════════════
// КОМПИЛЯЦИЯ И ПРОВЕРКА
// ═══════════════════════════════════════════════════════════

/// POST /strategies/:id/compile - скомпилировать стратегию
async fn compile_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<CompilationResponse>) {
    match storage.compile(&id) {
        Ok(result) => {
            let status = if result.success {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            };
            
            (
                status,
                Json(CompilationResponse {
                    success: result.success,
                    output: result.output,
                    errors: result.errors,
                    lib_path: result.lib_path.map(|p| p.to_string_lossy().to_string()),
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CompilationResponse {
                success: false,
                output: format!("Compilation error: {}", e),
                errors: vec![e.to_string()],
                lib_path: None,
            }),
        ),
    }
}

/// POST /strategies/:id/check - проверить синтаксис
async fn check_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<CompilationResponse>) {
    match storage.check(&id) {
        Ok(result) => {
            let status = if result.success {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            };
            
            (
                status,
                Json(CompilationResponse {
                    success: result.success,
                    output: result.output,
                    errors: result.errors,
                    lib_path: None,
                }),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CompilationResponse {
                success: false,
                output: format!("Check error: {}", e),
                errors: vec![e.to_string()],
                lib_path: None,
            }),
        ),
    }
}

// ═══════════════════════════════════════════════════════════
// RUNTIME: ЗАПУСК И ОСТАНОВКА (НОВЫЙ API С INSTANCES)
// ═══════════════════════════════════════════════════════════

/// POST /strategies/:id/instances/:symbol/start - запустить instance на конкретной монете
async fn start_instance(
    State((storage, runner, event_tx)): State<RuntimeState>,
    Path((id, symbol)): Path<(String, String)>,
    Json(req): Json<StartInstanceRequest>,
) -> (StatusCode, Json<ApiResponse>) {
    // Проверяем что стратегия существует
    if storage.load(&id).is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Strategy '{}' not found", id),
                data: None,
            }),
        );
    }
    
    // Получаем путь к библиотеке
    let lib_path = match storage.get_lib_path(&id) {
        Ok(path) => path,
        Err(_) => {
            // Пробуем скомпилировать
            match storage.compile(&id) {
                Ok(result) if result.success => {
                    result.lib_path.unwrap()
                }
                Ok(result) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiResponse {
                            success: false,
                            message: "Compilation failed".to_string(),
                            data: Some(serde_json::json!({ 
                                "errors": result.errors 
                            })),
                        }),
                    );
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ApiResponse {
                            success: false,
                            message: format!("Compilation error: {}", e),
                            data: None,
                        }),
                    );
                }
            }
        }
    };
    
    // Конвертируем параметры в JSON строку
    let params_json = match req.params {
        Some(p) => serde_json::to_string(&p).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    };
    
    // Формируем instance_id
    let instance_id = format!("{}:{}", id, symbol.to_uppercase());
    
    // Запускаем instance с параметрами
    match runner.start(
        instance_id.clone(),
        lib_path,
        event_tx.subscribe(),
        symbol.clone(),
        params_json,
    ).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Instance '{}' started", instance_id),
                data: Some(serde_json::json!({
                    "instance_id": instance_id,
                    "strategy_id": id,
                    "symbol": symbol,
                })),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to start: {}", e),
                data: None,
            }),
        ),
    }
}

/// POST /instances/:instance_id/stop - остановить конкретный instance
async fn stop_instance(
    State((_storage, runner, _event_tx)): State<RuntimeState>,
    Path(instance_id): Path<String>,
) -> (StatusCode, Json<ApiResponse>) {
    match runner.stop(&instance_id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(ApiResponse {
                success: true,
                message: format!("Instance '{}' stopped", instance_id),
                data: None,
            }),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                success: false,
                message: format!("Failed to stop: {}", e),
                data: None,
            }),
        ),
    }
}

/// GET /instances/running - список всех запущенных instances
async fn list_running_instances(
    State((_storage, runner, _event_tx)): State<RuntimeState>,
) -> Json<Vec<String>> {
    Json(runner.list_running())
}

// ═══════════════════════════════════════════════════════════
// RUNTIME: СТАРЫЙ API (ОБРАТНАЯ СОВМЕСТИМОСТЬ)
// ═══════════════════════════════════════════════════════════

/// POST /strategies/:id/start - запустить стратегию (старый API)
/// Использует symbol из metadata, без дополнительных параметров
async fn start_strategy(
    State((storage, runner, event_tx)): State<RuntimeState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResponse>) {
    // Загружаем стратегию чтобы получить symbol из metadata
    let strategy = match storage.load(&id) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse {
                    success: false,
                    message: format!("Strategy '{}' not found", id),
                    data: None,
                }),
            );
        }
    };
    
    let symbol = strategy.metadata.symbol;
    
    // Используем новый метод с пустыми параметрами
    let req = StartInstanceRequest {
        symbol: symbol.clone(),
        params: None,
    };
    
    start_instance(
        State((storage, runner, event_tx)),
        Path((id, symbol)),
        Json(req),
    ).await
}

/// POST /strategies/:id/stop - остановить стратегию (старый API)
async fn stop_strategy(
    State((storage, runner, _event_tx)): State<RuntimeState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<ApiResponse>) {
    // Загружаем стратегию чтобы получить symbol
    let strategy = match storage.load(&id) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse {
                    success: false,
                    message: format!("Strategy '{}' not found", id),
                    data: None,
                }),
            );
        }
    };
    
    let instance_id = format!("{}:{}", id, strategy.metadata.symbol.to_uppercase());
    
    stop_instance(
        State((storage, runner, broadcast::channel(1).0)),
        Path(instance_id),
    ).await
}

/// GET /strategies/running - список запущенных стратегий (старый API)
async fn list_running(
    State((_storage, runner, _event_tx)): State<RuntimeState>,
) -> Json<Vec<String>> {
    // Возвращаем все instances (для совместимости)
    Json(runner.list_running())
}