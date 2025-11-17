// src/routes/strategy.rs

use axum::{
    routing::{get, post, put, delete},
    extract::{Json, State, Path},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;

// Импортируем из модуля strategies
use crate::strategies::{Strategy, StrategyStorage};

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

pub fn strategy_routes(storage: Arc<StrategyStorage>) -> Router {
    Router::new()
        .route("/strategies", post(create_strategy))
        .route("/strategies", get(list_strategies))
        .route("/strategies/:id", get(get_strategy))
        .route("/strategies/:id/code", put(update_code))
        .route("/strategies/:id", delete(delete_strategy))
        .with_state(storage)
}

async fn create_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Json(req): Json<CreateStrategyRequest>,
) -> String {
    let strategy = Strategy {
        id: req.id.clone(),
        name: req.name,
        symbol: req.symbol,
        code: req.code,
        enabled: false,
    };
    
    match storage.create(strategy) {
        Ok(_) => format!("✅ Strategy '{}' created", req.id),
        Err(e) => format!("❌ Error: {}", e),
    }
}

async fn list_strategies(
    State(storage): State<Arc<StrategyStorage>>,
) -> axum::Json<Vec<Strategy>> {
    let strategies = storage.list().unwrap_or_default();
    axum::Json(strategies)
}

async fn get_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> String {
    match storage.load(&id) {
        Ok(strategy) => serde_json::to_string_pretty(&strategy).unwrap(),
        Err(e) => format!("❌ Error: {}", e),
    }
}

async fn update_code(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCodeRequest>,
) -> String {
    match storage.update_code(&id, req.code) {
        Ok(_) => format!("✅ Code updated for '{}'", id),
        Err(e) => format!("❌ Error: {}", e),
    }
}

async fn delete_strategy(
    State(storage): State<Arc<StrategyStorage>>,
    Path(id): Path<String>,
) -> String {
    match storage.delete(&id) {
        Ok(_) => format!("🗑️ Strategy '{}' deleted", id),
        Err(e) => format!("❌ Error: {}", e),
    }
}