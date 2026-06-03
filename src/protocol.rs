use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    Write {
        data: String,
    },
    /// Simulated typing: write each chunk separately with a randomized
    /// delay (centered on `delay_ms`) between chunks. A chunk is one
    /// keystroke -- a single character, or a complete escape sequence
    /// that must not be split (e.g. an arrow key's `\x1b[A`).
    WriteChunks {
        chunks: Vec<String>,
        delay_ms: u64,
    },
    View {
        color: bool,
    },
    Output {
        tail: Option<usize>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Wait {
        settle_ms: Option<u64>,
        timeout_ms: Option<u64>,
    },
    Screenshot {
        path: String,
        scale: Option<u32>,
    },
    Status,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(data: Option<String>) -> Self {
        Self {
            success: true,
            data,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}
