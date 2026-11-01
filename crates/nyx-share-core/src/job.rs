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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Send,
    /// Files arriving from another device; `destination_name` is the sender.
    Receive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub destination: String,
    pub destination_name: String,
    #[serde(default)]
    pub direction: Direction,
    pub state: JobState,
    pub items: u32,
    /// What is being moved, for lists: "report.pdf and 2 more".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// What a running job is doing before bytes move: "Connecting to NAS Documents…".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
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

/// "a.pdf", "a.pdf and b.jpg", "a.pdf, b.jpg and 3 more".
pub fn summarize<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let names: Vec<&str> = names.into_iter().collect();
    match names.as_slice() {
        [] => None,
        [a] => Some(a.to_string()),
        [a, b] => Some(format!("{a} and {b}")),
        [a, b, rest @ ..] => Some(format!("{a}, {b} and {} more", rest.len())),
    }
}

impl Job {
    /// The step in a word or two for tight spaces: "Connecting".
    pub fn short_step(&self) -> Option<String> {
        let step = self.step.as_deref()?;
        Some(step.split_whitespace().next().unwrap_or(step).trim_end_matches('…').to_string())
    }

    /// Progress from 0 to 1, if the size is known.
    pub fn fraction(&self) -> Option<f64> {
        (self.bytes_total > 0).then(|| (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0))
    }
}
