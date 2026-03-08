//! RFC 5322 body/attachment extraction via mail-parser.
//!
//! Replaces the old `envelope.rs` which depended on melib.
//! Used when fetching full message bodies via blob download.
//! For list views, JMAP returns structured data directly — no parsing needed.

use mail_parser::MimeHeaders;

use crate::models::AttachmentData;

/// Parsed email body parts.
pub struct ParsedBody {
    pub text_plain: Option<String>,
    pub text_html: Option<String>,
    pub attachments: Vec<AttachmentData>,
}

/// Parse a raw RFC 5322 message and extract body parts.
pub fn parse_body(raw: &[u8]) -> ParsedBody {
    let message = match mail_parser::MessageParser::default().parse(raw) {
        Some(msg) => msg,
        None => {
            return ParsedBody {
                text_plain: None,
                text_html: None,
                attachments: Vec::new(),
            }
        }
    };

    let text_plain = message
        .body_text(0)
        .map(|s| s.to_string());

    let text_html = message
        .body_html(0)
        .map(|s| s.to_string());

    let mut attachments = Vec::new();
    for attachment in message.attachments() {
        let filename = attachment
            .attachment_name()
            .unwrap_or("unnamed")
            .to_string();
        let mime_type = attachment
            .content_type()
            .map(|ct: &mail_parser::ContentType| {
                if let Some(subtype) = ct.subtype() {
                    format!("{}/{}", ct.ctype(), subtype)
                } else {
                    ct.ctype().to_string()
                }
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        attachments.push(AttachmentData {
            filename,
            mime_type,
            data: attachment.contents().to_vec(),
        });
    }

    ParsedBody {
        text_plain,
        text_html,
        attachments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_plain_text() {
        let raw = b"From: test@example.com\r\n\
                    To: recipient@example.com\r\n\
                    Subject: Test\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Hello, world!";
        let body = parse_body(raw);
        assert_eq!(body.text_plain.as_deref(), Some("Hello, world!"));
        assert!(body.attachments.is_empty());
    }

    #[test]
    fn parse_multipart_alternative() {
        let raw = b"From: test@example.com\r\n\
                    To: recipient@example.com\r\n\
                    Subject: Test\r\n\
                    Content-Type: multipart/alternative; boundary=boundary42\r\n\
                    \r\n\
                    --boundary42\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Plain text body\r\n\
                    --boundary42\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <p>HTML body</p>\r\n\
                    --boundary42--";
        let body = parse_body(raw);
        assert_eq!(body.text_plain.as_deref(), Some("Plain text body"));
        assert_eq!(body.text_html.as_deref(), Some("<p>HTML body</p>"));
    }

    #[test]
    fn parse_garbage_returns_empty() {
        let body = parse_body(b"not an email at all");
        // mail-parser may or may not parse this — but it shouldn't panic
        // and we should get something back
        assert!(body.attachments.is_empty());
    }
}
