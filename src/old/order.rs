use anyhow::{bail, Result};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use http::HeaderValue;
use simd_json::{json, BorrowedValue as Value};
use sha2::Sha256;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
};

// ⚠️ Никогда не храни реальные ключи в исходниках — только для примера
const BINANCE_API_KEY: &str =
    "GmL57ltotetbNpwiKyaT6Vd6F3ygH3vJLrf53gjEL15zFLydV5Q8tjNuo0vwUmvj";
const BINANCE_API_SECRET: &str =
    "l6gSlmYQxvwMsP9kQoo80BztyzwQSo2JEYrwKteHJCt96QiRUNexvONZYHzaYpTa";

type HmacSha256 = Hmac<Sha256>;

fn sign(query: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(BINANCE_API_SECRET.as_bytes()).unwrap();
    mac.update(query.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn build_qs(mut pairs: Vec<(&str, String)>) -> String {
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // UM Futures WebSocket API
    let url = "wss://ws-fapi.binance.com/ws-fapi/v1";

    // Подготовка запроса на подключение
    let mut req = url.into_client_request()?;
    req.headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("json"));
    req.headers_mut()
        .insert("X-MBX-APIKEY", HeaderValue::from_str(BINANCE_API_KEY)?);

    let (mut ws, _resp) = connect_async(req).await?;
    println!("✅ WebSocket API подключен");

    // Параметры ордера
    let ts = Utc::now().timestamp_millis();
    let recv_window = 5000;
    let symbol = "BTCUSDT";
    let side = "BUY";
    let order_type = "LIMIT";
    let quantity = "0.0025";
    let price = "20000";
    let tif = "GTC";
    let position_side = "BOTH";

    // Формирование и подпись параметров
    let qs = build_qs(vec![
        ("positionSide", position_side.to_string()),
        ("price", price.to_string()),
        ("quantity", quantity.to_string()),
        ("recvWindow", recv_window.to_string()),
        ("side", side.to_string()),
        ("symbol", symbol.to_string()),
        ("timeInForce", tif.to_string()),
        ("timestamp", ts.to_string()),
        ("type", order_type.to_string()),
    ]);
    let signature = sign(&qs);

    let mut place_req = json!({
        "id": 1,
        "method": "order.place",
        "params": {
            "symbol": symbol,
            "side": side,
            "positionSide": position_side,
            "type": order_type,
            "timeInForce": tif,
            "quantity": quantity,
            "price": price,
            "recvWindow": recv_window,
            "timestamp": ts,
            "signature": signature
        }
    });

    // Симд‑JSON работает с mutable строками
    let mut place_req_str = simd_json::to_string(&mut place_req)?;
    println!("→ {}", place_req_str);

    // Проверяем что JSON валиден
    let _: Value = simd_json::to_borrowed_value(place_req_str.as_mut_str())?;

    // Отправляем сообщение
    ws.send(Message::Text(place_req_str.clone())).await?;
    println!("📤 Ордер отправлен…");

    // Ждём ответ
    while let Some(msg) = ws.next().await {
        let txt = match msg? {
            Message::Text(t) => t,
            _ => continue,
        };

        println!("← {}", txt);

        // simd‑json парсит &mut str, поэтому делаем copy в mutable
        let mut s = txt.clone();
        let v: Value = match simd_json::to_borrowed_value(s.as_mut_str()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let status = v["status"].as_u64().unwrap_or(0);
        if status == 200 {
            if let Some(res) = v.get("result") {
                println!("✅ Ордер размещён: {}", res);
            } else {
                println!("✅ Ордер размещён (без result?)");
            }
            break;
        } else if v.get("error").is_some() {
            bail!("❌ Ошибка размещения: {}", v["error"]);
        }
    }

    Ok(())
}