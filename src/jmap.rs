use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use indexmap::IndexMap;
use tokio::sync::Mutex;

use melib::backends::{
    BackendEventConsumer, EnvelopeHashBatch, FlagOp, IsSubscribedFn, MailBackend,
};
use melib::conf::AccountSettings;
use melib::jmap::JmapType;
use melib::{AccountHash, EnvelopeHash, Mail, MailboxHash};

use crate::config::AccountConfig;
use crate::envelope::{envelope_to_summary, extract_body};
use crate::models::{AttachmentData, Folder, MessageSummary};

/// A live JMAP session backed by melib.
///
/// Mirrors `ImapSession` in structure: wraps melib's backend behind
/// `Arc<Mutex<...>>` and exposes the same set of async operations.
pub struct JmapSession {
    backend: Arc<Mutex<Box<JmapType>>>,
    mailbox_paths: Mutex<HashMap<MailboxHash, String>>,
}

fn map_mailbox_counts(counts: (usize, usize)) -> (u32, u32) {
    let (unseen, total) = counts;
    (unseen as u32, total as u32)
}

impl std::fmt::Debug for JmapSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JmapSession").finish_non_exhaustive()
    }
}

impl JmapSession {
    /// Connect to a JMAP server using the given account config.
    ///
    /// The `session_url` is the JMAP session resource URL, typically
    /// discovered via `https://{domain}/.well-known/jmap`.
    pub async fn connect(
        config: &AccountConfig,
        session_url: &str,
    ) -> Result<Arc<Self>, String> {
        let mut extra = IndexMap::new();
        extra.insert("server_url".into(), session_url.to_string());
        extra.insert("server_username".into(), config.username.clone());
        extra.insert("server_password".into(), config.password.clone());
        extra.insert("use_token".into(), "true".into());
        extra.insert("danger_accept_invalid_certs".into(), "false".into());

        let account_settings = AccountSettings {
            name: config.username.clone(),
            root_mailbox: "INBOX".into(),
            format: "jmap".into(),
            identity: config
                .email_addresses
                .first()
                .cloned()
                .unwrap_or_else(|| config.username.clone()),
            extra,
            ..Default::default()
        };

        let is_subscribed: IsSubscribedFn =
            (Arc::new(|_: &str| true) as Arc<dyn Fn(&str) -> bool + Send + Sync>).into();

        let event_consumer = BackendEventConsumer::new(Arc::new(
            |_account_hash: AccountHash, event: melib::backends::BackendEvent| {
                log::debug!("JMAP backend event: {:?}", event);
            },
        ));

        let backend = JmapType::new(&account_settings, is_subscribed, event_consumer)
            .map_err(|e| format!("Failed to create JMAP backend: {}", e))?;

        let session = JmapSession {
            backend: Arc::new(Mutex::new(backend)),
            mailbox_paths: Mutex::new(HashMap::new()),
        };

        // Verify connectivity (fetches JMAP session object)
        {
            let backend = session.backend.lock().await;
            let online_future = backend
                .is_online()
                .map_err(|e| format!("JMAP is_online failed: {}", e))?;
            online_future
                .await
                .map_err(|e| format!("JMAP connection failed: {}", e))?;
        }

        log::info!("JMAP session established for {}", config.username);
        Ok(Arc::new(session))
    }

    /// Fetch the list of folders (mailboxes) from the server.
    pub async fn fetch_folders(self: &Arc<Self>) -> Result<Vec<Folder>, String> {
        let future = {
            let backend = self.backend.lock().await;
            backend
                .mailboxes()
                .map_err(|e| format!("Failed to request mailboxes: {}", e))?
        };

        let mailboxes = future
            .await
            .map_err(|e| format!("Failed to fetch mailboxes: {}", e))?;

        let mut folders: Vec<Folder> = Vec::with_capacity(mailboxes.len());
        let mut path_map = HashMap::new();

        for (hash, mailbox) in &mailboxes {
            let counts = mailbox
                .count()
                .map_err(|e| format!("Failed to get mailbox count: {}", e))?;
            let (unseen, total) = map_mailbox_counts(counts);

            path_map.insert(*hash, mailbox.path().to_string());

            folders.push(Folder {
                name: mailbox.name().to_string(),
                path: mailbox.path().to_string(),
                unread_count: unseen,
                total_count: total,
                mailbox_hash: hash.0,
            });
        }

        // Sort: INBOX first, then alphabetical
        folders.sort_by(|a, b| {
            if a.path == "INBOX" {
                std::cmp::Ordering::Less
            } else if b.path == "INBOX" {
                std::cmp::Ordering::Greater
            } else {
                a.path.cmp(&b.path)
            }
        });

        *self.mailbox_paths.lock().await = path_map;

        log::info!("JMAP: fetched {} mailboxes", folders.len());
        Ok(folders)
    }

    /// Fetch message summaries (envelopes) for a mailbox.
    pub async fn fetch_messages(
        self: &Arc<Self>,
        mailbox_hash: MailboxHash,
    ) -> Result<Vec<MessageSummary>, String> {
        let stream = {
            let mut backend = self.backend.lock().await;
            backend
                .fetch(mailbox_hash)
                .map_err(|e| format!("Failed to start fetch: {}", e))?
        };

        let mut stream = std::pin::pin!(stream);
        let mut messages = Vec::new();

        while let Some(batch_result) = stream.next().await {
            let envelopes = batch_result.map_err(|e| format!("Error fetching envelopes: {}", e))?;

            for envelope in envelopes {
                messages.push(envelope_to_summary(&envelope, mailbox_hash));
            }
        }

        Ok(messages)
    }

    /// Set or unset flags on a single message.
    pub async fn set_flags(
        self: &Arc<Self>,
        envelope_hash: EnvelopeHash,
        mailbox_hash: MailboxHash,
        flags: Vec<FlagOp>,
    ) -> Result<(), String> {
        let future = {
            let mut backend = self.backend.lock().await;
            backend
                .set_flags(EnvelopeHashBatch::from(envelope_hash), mailbox_hash, flags)
                .map_err(|e| format!("Failed to request set_flags: {}", e))?
        };

        future
            .await
            .map_err(|e| format!("Failed to set flags: {}", e))?;
        Ok(())
    }

    /// Move a message from one mailbox to another.
    ///
    /// JMAP note: this uses `Email/set` to patch `mailboxIds`, which is the
    /// correct way to move messages over JMAP. The `Flag::TRASHED` -> `$junk`
    /// mapping in melib is wrong for trash operations, so callers must always
    /// use this method (not flag-based trash) for JMAP accounts.
    pub async fn move_messages(
        self: &Arc<Self>,
        envelope_hash: EnvelopeHash,
        source_mailbox_hash: MailboxHash,
        destination_mailbox_hash: MailboxHash,
    ) -> Result<(), String> {
        let future = {
            let mut backend = self.backend.lock().await;
            backend
                .copy_messages(
                    EnvelopeHashBatch::from(envelope_hash),
                    source_mailbox_hash,
                    destination_mailbox_hash,
                    true, // move = true
                )
                .map_err(|e| format!("Failed to request move: {}", e))?
        };

        future
            .await
            .map_err(|e| format!("Failed to move message: {}", e))?;
        Ok(())
    }

    /// Permanently delete a message.
    ///
    /// For JMAP, this is `Email/set { destroy: [...] }`. Use `move_messages`
    /// to move to Trash instead; reserve this for "empty trash" operations.
    pub async fn delete_messages(
        self: &Arc<Self>,
        envelope_hash: EnvelopeHash,
        mailbox_hash: MailboxHash,
    ) -> Result<(), String> {
        let future = {
            let mut backend = self.backend.lock().await;
            backend
                .delete_messages(EnvelopeHashBatch::from(envelope_hash), mailbox_hash)
                .map_err(|e| format!("Failed to request delete: {}", e))?
        };

        future
            .await
            .map_err(|e| format!("Failed to delete message: {}", e))?;
        Ok(())
    }

    /// Fetch and render the body of a single message, extracting attachments.
    /// Returns (markdown_body, plain_body, attachments).
    pub async fn fetch_body(
        self: &Arc<Self>,
        envelope_hash: EnvelopeHash,
    ) -> Result<(String, String, Vec<AttachmentData>), String> {
        let future = {
            let backend = self.backend.lock().await;
            backend
                .envelope_bytes_by_hash(envelope_hash)
                .map_err(|e| format!("Failed to request message bytes: {}", e))?
        };

        let bytes = future
            .await
            .map_err(|e| format!("Failed to fetch message bytes: {}", e))?;

        let mail = Mail::new(bytes, None).map_err(|e| format!("Failed to parse message: {}", e))?;

        let body_attachment = mail.body();
        let (text_plain, text_html, attachments) = extract_body(&body_attachment);

        let plain_rendered = crate::mime::render_body(text_plain.as_deref(), text_html.as_deref());
        let markdown_rendered =
            crate::mime::render_body_markdown(text_plain.as_deref(), text_html.as_deref());

        Ok((markdown_rendered, plain_rendered, attachments))
    }

    /// Start watching for changes (JMAP poll loop — 60s intervals).
    /// Returns a `'static` stream — safe to hold after releasing the lock.
    pub async fn watch(
        self: &Arc<Self>,
    ) -> Result<
        impl futures::Stream<Item = melib::error::Result<melib::backends::BackendEvent>>,
        String,
    > {
        let stream = {
            let backend = self.backend.lock().await;
            backend
                .watch()
                .map_err(|e| format!("Failed to start watch: {}", e))?
        };
        log::info!("JMAP watch stream started (60s poll interval)");
        Ok(stream)
    }
}
