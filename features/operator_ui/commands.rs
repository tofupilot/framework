//! UI response submission commands.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use specta::Type;
use super::channels::UI_RESPONSE_CHANNELS;

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct UiResponseValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub string_val: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_val: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bool_val: Option<bool>,
}

#[tauri::command]
#[specta::specta]
pub async fn submit_native_ui_response(
    request_id: String,
    values: HashMap<String, UiResponseValue>,
    bound_measurements: Option<HashMap<String, UiResponseValue>>,
) -> Result<(), String> {
    log::debug!(
        "🎯 submit_native_ui_response: request_id={}, bound_measurements={:?}",
        request_id, bound_measurements
    );

    let mut channels = UI_RESPONSE_CHANNELS.lock().await;

    if let Some(tx) = channels.remove(&request_id) {
        let response: HashMap<String, String> = values
            .into_iter()
            .map(|(k, v)| {
                let string_value = if let Some(s) = v.string_val {
                    s
                } else if let Some(n) = v.number_val {
                    n.to_string()
                } else if let Some(b) = v.bool_val {
                    b.to_string()
                } else {
                    String::new()
                };
                (k, string_value)
            })
            .collect();

        let mut final_response = response;
        if let Some(measurements) = bound_measurements {
            let measurements_map: HashMap<String, serde_json::Value> = measurements
                .into_iter()
                .map(|(k, v)| {
                    let value = if let Some(s) = v.string_val {
                        serde_json::Value::String(s)
                    } else if let Some(n) = v.number_val {
                        serde_json::Value::Number(serde_json::Number::from_f64(n).unwrap_or(serde_json::Number::from(0)))
                    } else if let Some(b) = v.bool_val {
                        serde_json::Value::Bool(b)
                    } else {
                        serde_json::Value::Null
                    };
                    (k, value)
                })
                .collect();
            final_response.insert(
                "__bound_measurements__".to_string(),
                serde_json::to_string(&measurements_map).unwrap_or_default(),
            );
        }

        tx.send(final_response)
            .map_err(|_| "Failed to send response".to_string())?;
        Ok(())
    } else {
        log::debug!(
            "📝 UI submission for Python phase (no channel): {}",
            request_id
        );
        Ok(())
    }
}
