// src/routes/strategy.rs

use axum::{
    http::StatusCode,
    routing::{get, post, put, delete},
    extract::{Json, State, Path},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::strategies::storage::{StrategyStorage, Strategy, StrategyMetadata};

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
    pub open_positions: Option<bool>
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
// РОУТЕР
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
// CRUD ОПЕРАЦИИ
// ═══════════════════════════════════════════════════════════

/// POST /strategies - создать новую стратегию
async fn create_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Json(req): Json<CreateStrategyRequest>,
) -> (StatusCode, Json<ApiResponse>) {
    // Создаём стратегию
    let strategy = Strategy::new(
        req.id.clone(),
        req.name,
        req.symbol,
        req.code,
    );
    
    // Сохраняем
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
    // Сохраняем новый код
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
                                            message: format!("Code updated but compilation failed"),
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
    match storage.update_metadata(&id, req.name, req.symbol, req.enabled, req.open_positions) {
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
