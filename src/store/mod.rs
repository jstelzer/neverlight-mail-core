mod backfill_queries;
mod body_queries;
mod commands;
mod dispatch;
mod flag_queries;
mod flags;
mod folder_queries;
mod handle;
mod message_queries;
mod schema;
mod search_queries;

pub use flags::{flags_from_u8, flags_to_u8};
pub use handle::CacheHandle;

/// Public constant for the default page size.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
