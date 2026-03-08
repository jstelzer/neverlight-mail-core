//! JMAP Email methods (RFC 8621 §4).
//!
//! Email/query, Email/get, Email/set, Email/changes.

mod body;
mod flags;
mod query;

pub use body::get_body;
pub use flags::{
    check_set_errors, destroy, move_to, search, set_flag, set_flags_batch, trash, SearchFilter,
};
pub use query::{get_summaries, query, query_and_get, QueryResult, DEFAULT_PAGE_SIZE};
