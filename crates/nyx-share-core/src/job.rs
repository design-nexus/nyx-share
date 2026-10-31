use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn finished(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub destination: String,
    pub destination_name: String,
    pub state: JobState,
    pub items: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// What happened, in a sentence: "Copied to the clipboard", or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// What the destination made, such as the paths of saved files.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Unix seconds.
    pub created: i64,
    pub updated: i64,
}

impl Job {
    /// Progress from 0 to 1, if the size is known.
    pub fn fraction(&self) -> Option<f64> {
        (self.bytes_total > 0).then(|| (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0))
    }
}
