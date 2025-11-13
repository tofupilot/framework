use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct VenvInfo {
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_version: Option<String>,
    pub packages: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, TS)]
#[ts(export)]
pub struct VenvPackage {
    pub name: String,
    pub version: String,
}
