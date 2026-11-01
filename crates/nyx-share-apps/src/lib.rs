//! Nyx apps as Share destinations, each through the interface the app already
//! offers: Notes, Mail and Pulse (Telegram) through their control tools, cloud accounts
//! through Nyx Cloud's service.

pub mod bluetooth;
pub mod cloud;
pub mod control;
pub mod mail;
pub mod notes;
pub mod pulse;

pub use bluetooth::Bluetooth;
pub use cloud::Cloud;
pub use mail::Mail;
pub use notes::Notes;
pub use pulse::Pulse;
