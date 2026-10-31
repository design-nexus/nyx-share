//! LocalSend v2 messages (protocol v2.2, as LocalSend 1.18 sends them).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Announced over multicast, and sent to `register`: who a device is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub alias: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    pub fingerprint: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub download: bool,
    /// Only in multicast messages: true asks others to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<bool>,
}

fn default_port() -> u16 {
    crate::PORT
}

fn default_protocol() -> String {
    "https".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDto {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    #[serde(default = "octet_stream")]
    pub file_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// For text messages, the text itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

fn octet_stream() -> String {
    "application/octet-stream".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrepareUpload {
    pub info: DeviceInfo,
    pub files: HashMap<String, FileDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    /// File id to upload token, for the files the receiver accepted.
    pub files: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_spec_examples() {
        let announcement: DeviceInfo = serde_json::from_str(
            r#"{"alias":"Nice Orange","version":"2.0","deviceModel":"Samsung","deviceType":"mobile",
                "fingerprint":"random string","port":53317,"protocol":"https","download":true,"announce":true}"#,
        )
        .unwrap();
        assert_eq!(announcement.announce, Some(true));
        assert_eq!(announcement.device_type.as_deref(), Some("mobile"));
        let minimal: DeviceInfo = serde_json::from_str(r#"{"alias":"A","version":"2.1","fingerprint":"F"}"#).unwrap();
        assert_eq!((minimal.port, minimal.protocol.as_str()), (53317, "https"));
        let prepare: PrepareUpload = serde_json::from_str(
            r#"{"info":{"alias":"A","version":"2.0","fingerprint":"F","port":53317,"protocol":"https"},
                "files":{"x":{"id":"x","fileName":"my image.png","size":324242,"fileType":"image/jpeg","sha256":null,"preview":null}}}"#,
        )
        .unwrap();
        assert_eq!(prepare.files["x"].size, 324242);
        let response: PrepareUploadResponse = serde_json::from_str(r#"{"sessionId":"s","files":{"x":"t"}}"#).unwrap();
        assert_eq!(response.files["x"], "t");
    }
}
