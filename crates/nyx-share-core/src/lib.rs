//! What Nyx Share passes around: shared content, destinations and transfer jobs,
//! with the checks every request goes through. No D-Bus, GTK or I/O here.

pub mod content;
pub mod destination;
pub mod job;
pub mod mime;
pub mod names;
pub mod units;

pub use content::{ContentError, ContentKind, ItemInfo, ItemSpec, Limits, kinds_of, validate};
pub use destination::{Destination, Preferences, PreferencesUpdate, Section, arrange};
pub use job::{Direction, Job, JobState, summarize};

/// The version of the public `io.github.design_nexus.nyx.Share1` interface.
pub const API_VERSION: u32 = 1;
