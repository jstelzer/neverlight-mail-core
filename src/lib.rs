pub mod config;
pub mod imap;
pub mod keyring;
pub mod mime;
pub mod models;
pub mod setup;
pub mod smtp;
pub mod store;

// Re-export melib types used by consumers
pub use melib::backends::{BackendEvent, FlagOp, RefreshEventKind};
pub use melib::email::Flag;
pub use melib::{EnvelopeHash, MailboxHash};
