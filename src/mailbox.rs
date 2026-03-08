//! JMAP Mailbox methods (RFC 8621 §2).
//!
//! Mailbox/get, Mailbox/changes, Mailbox/set.

use serde_json::Value;

use crate::client::{JmapClient, JmapError};
use crate::models::Folder;

/// Properties requested from Mailbox/get (RFC 8621 §2.1).
const MAILBOX_PROPERTIES: &[&str] = &[
    "id",
    "name",
    "parentId",
    "role",
    "sortOrder",
    "totalEmails",
    "unreadEmails",
    "myRights",
];

/// Fetch all mailboxes for the account.
///
/// Sends `Mailbox/get` and maps the response to `Vec<Folder>`.
/// Builds hierarchical paths from parentId relationships.
pub async fn fetch_all(client: &JmapClient) -> Result<Vec<Folder>, JmapError> {
    let call = client.method(
        "Mailbox/get",
        serde_json::json!({
            "properties": MAILBOX_PROPERTIES,
        }),
        "m0",
    );

    let resp = client.call(vec![call]).await?;

    let list = resp
        .method_responses
        .first()
        .and_then(|mc| mc.1.get("list"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            JmapError::RequestError("Missing list in Mailbox/get response".into())
        })?;

    parse_mailboxes(list)
}

/// Parse the Mailbox/get `list` array into `Vec<Folder>`.
///
/// Builds hierarchical path names from `parentId` references.
fn parse_mailboxes(list: &[Value]) -> Result<Vec<Folder>, JmapError> {
    // First pass: collect raw mailbox data
    let mut raw: Vec<RawMailbox> = Vec::with_capacity(list.len());
    for item in list {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let parent_id = item
            .get("parentId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let role = item
            .get("role")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let sort_order = item
            .get("sortOrder")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let total_emails = item
            .get("totalEmails")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let unread_emails = item
            .get("unreadEmails")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        raw.push(RawMailbox {
            id,
            name,
            parent_id,
            role,
            sort_order,
            total_emails,
            unread_emails,
        });
    }

    // Build path by walking up parentId chain
    let folders: Vec<Folder> = raw
        .iter()
        .map(|mb| {
            let path = build_path(mb, &raw);
            Folder {
                name: mb.name.clone(),
                path,
                mailbox_id: mb.id.clone(),
                role: mb.role.clone(),
                sort_order: mb.sort_order,
                total_count: mb.total_emails,
                unread_count: mb.unread_emails,
            }
        })
        .collect();

    Ok(sort_folders(folders))
}

/// Sort folders: inbox first, then by role priority, then sort_order, then name.
fn sort_folders(mut folders: Vec<Folder>) -> Vec<Folder> {
    folders.sort_by(|a, b| {
        let a_priority = role_priority(a.role.as_deref());
        let b_priority = role_priority(b.role.as_deref());
        a_priority
            .cmp(&b_priority)
            .then(a.sort_order.cmp(&b.sort_order))
            .then(a.name.cmp(&b.name))
    });
    folders
}

/// Priority for sorting: lower = earlier. Inbox first, then standard roles, then custom.
fn role_priority(role: Option<&str>) -> u32 {
    match role {
        Some("inbox") => 0,
        Some("drafts") => 1,
        Some("sent") => 2,
        Some("archive") => 3,
        Some("trash") => 4,
        Some("junk") => 5,
        Some(_) => 6,
        None => 7,
    }
}

/// Build a hierarchical path like "Parent/Child/Grandchild".
fn build_path(mb: &RawMailbox, all: &[RawMailbox]) -> String {
    let mut parts = vec![mb.name.clone()];
    let mut current_parent = mb.parent_id.as_deref();
    let mut depth = 0;

    while let Some(pid) = current_parent {
        let Some(parent) = all.iter().find(|m| m.id == pid) else {
            break;
        };
        parts.push(parent.name.clone());
        current_parent = parent.parent_id.as_deref();
        depth += 1;
        if depth > 20 {
            break; // guard against cycles
        }
    }

    parts.reverse();
    parts.join("/")
}

struct RawMailbox {
    id: String,
    name: String,
    parent_id: Option<String>,
    role: Option<String>,
    sort_order: u32,
    total_emails: u32,
    unread_emails: u32,
}

/// Find the mailbox ID for a given role (e.g. "inbox", "drafts", "trash").
pub fn find_by_role(folders: &[Folder], role: &str) -> Option<String> {
    folders
        .iter()
        .find(|f| f.role.as_deref() == Some(role))
        .map(|f| f.mailbox_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_mailbox_list() -> Vec<Value> {
        serde_json::from_value(serde_json::json!([
            {
                "id": "mb-inbox",
                "name": "Inbox",
                "parentId": null,
                "role": "inbox",
                "sortOrder": 1,
                "totalEmails": 42,
                "unreadEmails": 5,
                "myRights": {"mayReadItems": true}
            },
            {
                "id": "mb-drafts",
                "name": "Drafts",
                "parentId": null,
                "role": "drafts",
                "sortOrder": 2,
                "totalEmails": 3,
                "unreadEmails": 0,
                "myRights": {"mayReadItems": true}
            },
            {
                "id": "mb-sent",
                "name": "Sent",
                "parentId": null,
                "role": "sent",
                "sortOrder": 3,
                "totalEmails": 100,
                "unreadEmails": 0,
                "myRights": {"mayReadItems": true}
            },
            {
                "id": "mb-trash",
                "name": "Trash",
                "parentId": null,
                "role": "trash",
                "sortOrder": 4,
                "totalEmails": 10,
                "unreadEmails": 0,
                "myRights": {"mayReadItems": true}
            },
            {
                "id": "mb-projects",
                "name": "Projects",
                "parentId": null,
                "role": null,
                "sortOrder": 10,
                "totalEmails": 50,
                "unreadEmails": 2,
                "myRights": {"mayReadItems": true}
            },
            {
                "id": "mb-proj-alpha",
                "name": "Alpha",
                "parentId": "mb-projects",
                "role": null,
                "sortOrder": 1,
                "totalEmails": 20,
                "unreadEmails": 1,
                "myRights": {"mayReadItems": true}
            }
        ]))
        .unwrap()
    }

    #[test]
    fn parses_mailbox_list() {
        let list = sample_mailbox_list();
        let folders = parse_mailboxes(&list).unwrap();

        assert_eq!(folders.len(), 6);

        let inbox = folders.iter().find(|f| f.role.as_deref() == Some("inbox")).unwrap();
        assert_eq!(inbox.name, "Inbox");
        assert_eq!(inbox.mailbox_id, "mb-inbox");
        assert_eq!(inbox.total_count, 42);
        assert_eq!(inbox.unread_count, 5);
    }

    #[test]
    fn sorts_by_role_priority() {
        let list = sample_mailbox_list();
        let folders = parse_mailboxes(&list).unwrap();

        // Inbox should be first, then drafts, sent, trash, then custom
        assert_eq!(folders[0].role.as_deref(), Some("inbox"));
        assert_eq!(folders[1].role.as_deref(), Some("drafts"));
        assert_eq!(folders[2].role.as_deref(), Some("sent"));
        assert_eq!(folders[3].role.as_deref(), Some("trash"));
    }

    #[test]
    fn builds_hierarchical_paths() {
        let list = sample_mailbox_list();
        let folders = parse_mailboxes(&list).unwrap();

        let alpha = folders.iter().find(|f| f.mailbox_id == "mb-proj-alpha").unwrap();
        assert_eq!(alpha.path, "Projects/Alpha");
        assert_eq!(alpha.name, "Alpha");

        let projects = folders.iter().find(|f| f.mailbox_id == "mb-projects").unwrap();
        assert_eq!(projects.path, "Projects");
    }

    #[test]
    fn find_by_role_works() {
        let list = sample_mailbox_list();
        let folders = parse_mailboxes(&list).unwrap();

        assert_eq!(find_by_role(&folders, "inbox"), Some("mb-inbox".to_string()));
        assert_eq!(find_by_role(&folders, "trash"), Some("mb-trash".to_string()));
        assert_eq!(find_by_role(&folders, "nonexistent"), None);
    }

    #[test]
    fn handles_empty_list() {
        let folders = parse_mailboxes(&[]).unwrap();
        assert!(folders.is_empty());
    }

    #[test]
    fn handles_missing_optional_fields() {
        let list: Vec<Value> = serde_json::from_value(serde_json::json!([
            {
                "id": "mb1",
                "name": "Test"
            }
        ]))
        .unwrap();

        let folders = parse_mailboxes(&list).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Test");
        assert!(folders[0].role.is_none());
        assert_eq!(folders[0].sort_order, 0);
        assert_eq!(folders[0].total_count, 0);
    }
}
