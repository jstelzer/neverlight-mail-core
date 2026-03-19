use rusqlite::Connection;
use tokio::sync::mpsc;

use super::backfill_queries;
use super::body_queries;
use super::commands::CacheCmd;
use super::flag_queries;
use super::folder_queries;
use super::message_queries;
use super::search_queries;

pub(super) fn run_loop(conn: Connection, mut rx: mpsc::UnboundedReceiver<CacheCmd>) {
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            CacheCmd::SaveFolders {
                account_id,
                folders,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_save_folders(
                    &conn,
                    &account_id,
                    &folders,
                ));
            }
            CacheCmd::LoadFolders { account_id, reply } => {
                let _ = reply.send(folder_queries::do_load_folders(&conn, &account_id));
            }
            CacheCmd::SaveMessages {
                account_id,
                mailbox_id,
                messages,
                reply,
            } => {
                let _ = reply.send(message_queries::do_save_messages(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    &messages,
                ));
            }
            CacheCmd::LoadMessages {
                account_id,
                mailbox_id,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(message_queries::do_load_messages(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    limit,
                    offset,
                ));
            }
            CacheCmd::LoadBody {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(body_queries::do_load_body(&conn, &account_id, &email_id));
            }
            CacheCmd::SaveBody {
                account_id,
                email_id,
                body_markdown,
                body_plain,
                attachments,
                reply,
            } => {
                let _ = reply.send(body_queries::do_save_body(
                    &conn,
                    &account_id,
                    &email_id,
                    &body_markdown,
                    &body_plain,
                    &attachments,
                ));
            }
            CacheCmd::UpdateFlags {
                account_id,
                email_id,
                flags_local,
                pending_op,
                reply,
            } => {
                let _ = reply.send(flag_queries::do_update_flags(
                    &conn,
                    &account_id,
                    &email_id,
                    flags_local,
                    &pending_op,
                ));
            }
            CacheCmd::ClearPendingOp {
                account_id,
                email_id,
                flags_server,
                reply,
            } => {
                let _ = reply.send(flag_queries::do_clear_pending_op(
                    &conn,
                    &account_id,
                    &email_id,
                    flags_server,
                ));
            }
            CacheCmd::RevertPendingOp {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(flag_queries::do_revert_pending_op(
                    &conn,
                    &account_id,
                    &email_id,
                ));
            }
            CacheCmd::RemoveMessage {
                account_id,
                email_id,
                reply,
            } => {
                let _ = reply.send(message_queries::do_remove_message(
                    &conn,
                    &account_id,
                    &email_id,
                ));
            }
            CacheCmd::PruneMailbox {
                account_id,
                mailbox_id,
                live_email_ids,
                reply,
            } => {
                let _ = reply.send(message_queries::do_prune_mailbox(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    &live_email_ids,
                ));
            }
            CacheCmd::Search {
                account_id,
                query,
                reply,
            } => {
                let _ = reply.send(search_queries::do_search(&conn, &account_id, &query));
            }
            CacheCmd::LoadThread {
                account_id,
                thread_id,
                mailbox_ids,
                reply,
            } => {
                let _ = reply.send(search_queries::do_load_thread(
                    &conn,
                    &account_id,
                    &thread_id,
                    &mailbox_ids,
                ));
            }
            CacheCmd::UpsertFolders {
                account_id,
                folders,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_upsert_folders(
                    &conn,
                    &account_id,
                    &folders,
                ));
            }
            CacheCmd::RemoveFolders {
                account_id,
                mailbox_ids,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_remove_folders_by_id(
                    &conn,
                    &account_id,
                    &mailbox_ids,
                ));
            }
            CacheCmd::RemoveAccount { account_id, reply } => {
                let _ = reply.send(folder_queries::do_remove_account(&conn, &account_id));
            }
            CacheCmd::GetState {
                account_id,
                resource,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_get_state(&conn, &account_id, &resource));
            }
            CacheCmd::SetState {
                account_id,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_set_state(
                    &conn,
                    &account_id,
                    &resource,
                    &state,
                ));
            }
            CacheCmd::GetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            } => {
                let _ = reply.send(backfill_queries::do_get_backfill_progress(
                    &conn,
                    &account_id,
                    &mailbox_id,
                ));
            }
            CacheCmd::SetBackfillProgress {
                account_id,
                mailbox_id,
                position,
                total,
                completed,
                reply,
            } => {
                let _ = reply.send(backfill_queries::do_set_backfill_progress(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    position,
                    total,
                    completed,
                ));
            }
            CacheCmd::ListBackfillProgress { account_id, reply } => {
                let _ = reply.send(backfill_queries::do_list_backfill_progress(
                    &conn,
                    &account_id,
                ));
            }
            CacheCmd::ResetBackfillProgress {
                account_id,
                mailbox_id,
                reply,
            } => {
                let _ = reply.send(backfill_queries::do_reset_backfill_progress(
                    &conn,
                    &account_id,
                    &mailbox_id,
                ));
            }
            CacheCmd::SaveFoldersAndSetState {
                account_id,
                folders,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_save_folders_and_set_state(
                    &conn,
                    &account_id,
                    &folders,
                    &resource,
                    &state,
                ));
            }
            CacheCmd::DeltaFoldersAndSetState {
                account_id,
                upsert,
                remove_ids,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(folder_queries::do_delta_folders_and_set_state(
                    &conn,
                    &account_id,
                    &upsert,
                    &remove_ids,
                    &resource,
                    &state,
                ));
            }
            CacheCmd::SaveMessagesAndSetState {
                account_id,
                mailbox_id,
                messages,
                resource,
                state,
                populated_mailbox_id,
                reply,
            } => {
                let _ = reply.send(message_queries::do_save_messages_and_set_state(
                    &conn,
                    &account_id,
                    &mailbox_id,
                    &messages,
                    &resource,
                    &state,
                    &populated_mailbox_id,
                ));
            }
            CacheCmd::DeltaEmailBatch {
                account_id,
                remove_ids,
                save_groups,
                resource,
                state,
                reply,
            } => {
                let _ = reply.send(message_queries::do_delta_email_batch(
                    &conn,
                    &account_id,
                    &remove_ids,
                    &save_groups,
                    &resource,
                    &state,
                ));
            }
            CacheCmd::ExpirePendingOps {
                account_id,
                max_age_secs,
                reply,
            } => {
                let _ = reply.send(flag_queries::do_expire_pending_ops(
                    &conn,
                    &account_id,
                    max_age_secs,
                ));
            }
        }
    }
    log::debug!("Cache thread exiting");
}
