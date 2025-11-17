// // use serde::{Deserialize, Deserializer};
// // use serde::de;
// // use std::fmt;

// /// Событие "trade" c уже вычисленным знаком размера.
// #[derive(Debug)]
// pub struct Trade {
//     pub time: i64,
//     pub symbol: String,
//     pub price: f64,
//     pub qty: f64, // со знаком: >0 buy, <0 sell
// }

// impl<'de> Deserialize<'de> for Trade {
//     fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
//     where
//         D: Deserializer<'de>,
//     {
//         #[derive(Deserialize)]
//         struct RawTrade {
//             #[serde(rename = "E")] E: i64,
//             #[serde(rename = "s")] s: String,
//             #[serde(rename = "p")] p: String,
//             #[serde(rename = "q")] q: String,
//             #[serde(rename = "m")] m: bool,
//         }

//         let raw = RawTrade::deserialize(deserializer)?;

//         let price = raw.p.parse::<f64>().map_err(de::Error::custom)?;
//         let mut qty = raw.q.parse::<f64>().map_err(de::Error::custom)?;
//         if raw.m { qty = -qty; }

//         Ok(Trade {
//             time: raw.E,
//             symbol: raw.s,
//             price,
//             qty,
//         })
//     }
// }

// fn main() -> Result<(), Box<dyn std::error::Error>> {
//     // пример JSON как присылает биржа
//     let json_text = r#"
//     {
//         "e": "trade",
//         "E": 1760869470671,
//         "T": 1760869470671,
//         "s": "SOLUSDT",
//         "t": 2811369788,
//         "p": "188.2600",
//         "q": "0.09",
//         "X": "MARKET",
//         "m": true
//     }"#;

//     // делаем String → &mut String (simd-json мутирует буфер)
//     let mut json = json_text.to_owned();

//     // SIMD-парсинг
//     let trade: Trade = simd_json::serde::from_str(&mut json)?;
//     println!("{trade:?}");
//     println!("directional size = {}", trade.qty);
//     Ok(())
// }