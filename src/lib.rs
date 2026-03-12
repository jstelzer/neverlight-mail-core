pub mod client;
pub mod config;
pub mod discovery;
pub mod email;
pub mod keyring;
pub mod mailbox;
pub mod mime;
pub mod models;
pub mod oauth;
pub mod parse;
pub mod push;
pub mod session;
pub mod setup;
pub mod store;
pub mod submit;
pub mod sync;
pub mod types;

// Re-export core types for consumers
pub use types::{
    BlobId, EmailId, FlagOp, Flags, IdentityId, MailboxId, MailboxRole, State, SyncEvent, ThreadId,
};
