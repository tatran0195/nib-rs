//! Concrete invitation email transports.
//!
//! A successful return means only that the configured provider acknowledged the
//! request. SMTP remains at-least-once across an acknowledgement loss; callers
//! must persist the provider receipt and use delivery leases.

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart, SinglePart, header::ContentType},
    transport::smtp::response::Response as SmtpResponse,
};
use nibleaf_core::config::InvitationEmailConfig;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{InvitationEmailProvider, resolve_invitation_email_provider};

const POSTMARK_ENDPOINT: &str = "https://api.postmarkapp.com/email";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvitationEmail {
    pub recipient: String,
    pub subject: String,
    pub text_body: String,
    pub html_body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderReceipt {
    pub provider: InvitationEmailProvider,
    pub message_id: String,
}

#[derive(Debug, Error)]
pub enum InvitationEmailDeliveryError {
    #[error("no invitation email provider is configured")]
    Unconfigured,
    #[error("invitation email is invalid")]
    InvalidMessage,
    #[error("invitation email provider rejected the request")]
    Permanent,
    #[error("invitation email provider is temporarily unavailable")]
    Retryable,
}

/// Sends one fully rendered invitation through the legacy-compatible provider
/// selection rule. This function holds no database transaction or lease while
/// it waits on the network.
///
/// # Errors
/// Returns classified configuration, validation, permanent-provider, or
/// retryable-provider errors. Error bodies and addresses are deliberately not
/// returned because they may be sensitive and unbounded.
pub async fn send_invitation_email(
    http_client: &Client,
    config: &InvitationEmailConfig,
    email: &InvitationEmail,
) -> Result<ProviderReceipt, InvitationEmailDeliveryError> {
    validate_message(email)?;
    match resolve_invitation_email_provider(
        config.postmark_api_key.as_deref(),
        config.smtp_url.as_deref(),
    )
    .map_err(|_| InvitationEmailDeliveryError::Unconfigured)?
    {
        InvitationEmailProvider::Postmark => send_postmark(http_client, config, email).await,
        InvitationEmailProvider::Smtp => send_smtp(config, email).await,
    }
}

fn validate_message(email: &InvitationEmail) -> Result<(), InvitationEmailDeliveryError> {
    if email.recipient.trim().is_empty()
        || email.subject.trim().is_empty()
        || email.text_body.is_empty()
        || email.html_body.is_empty()
        || contains_line_break(&email.subject)
        || contains_line_break(&email.recipient)
    {
        return Err(InvitationEmailDeliveryError::InvalidMessage);
    }
    Ok(())
}

fn contains_line_break(value: &str) -> bool {
    value.contains(['\r', '\n'])
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PostmarkRequest<'a> {
    from: &'a str,
    to: &'a str,
    subject: &'a str,
    text_body: &'a str,
    html_body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_stream: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PostmarkResponse {
    #[serde(rename = "MessageID")]
    message_id: String,
}

async fn send_postmark(
    http_client: &Client,
    config: &InvitationEmailConfig,
    email: &InvitationEmail,
) -> Result<ProviderReceipt, InvitationEmailDeliveryError> {
    send_postmark_at(http_client, config, email, POSTMARK_ENDPOINT).await
}

async fn send_postmark_at(
    http_client: &Client,
    config: &InvitationEmailConfig,
    email: &InvitationEmail,
    endpoint: &str,
) -> Result<ProviderReceipt, InvitationEmailDeliveryError> {
    let Some(api_key) = config.postmark_api_key.as_deref() else {
        return Err(InvitationEmailDeliveryError::Unconfigured);
    };
    let response = http_client
        .post(endpoint)
        .header("X-Postmark-Server-Token", api_key)
        .json(&PostmarkRequest {
            from: &config.from,
            to: &email.recipient,
            subject: &email.subject,
            text_body: &email.text_body,
            html_body: &email.html_body,
            message_stream: config.postmark_message_stream.as_deref(),
        })
        .send()
        .await
        .map_err(|_| InvitationEmailDeliveryError::Retryable)?;
    let status = response.status();
    if !status.is_success() {
        return Err(classify_http_status(status));
    }
    let receipt = response
        .json::<PostmarkResponse>()
        .await
        .map_err(|_| InvitationEmailDeliveryError::Retryable)?;
    if receipt.message_id.trim().is_empty() {
        return Err(InvitationEmailDeliveryError::Retryable);
    }
    Ok(ProviderReceipt {
        provider: InvitationEmailProvider::Postmark,
        message_id: receipt.message_id,
    })
}

fn classify_http_status(status: StatusCode) -> InvitationEmailDeliveryError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        InvitationEmailDeliveryError::Retryable
    } else {
        InvitationEmailDeliveryError::Permanent
    }
}

async fn send_smtp(
    config: &InvitationEmailConfig,
    email: &InvitationEmail,
) -> Result<ProviderReceipt, InvitationEmailDeliveryError> {
    let Some(smtp_url) = config.smtp_url.as_deref() else {
        return Err(InvitationEmailDeliveryError::Unconfigured);
    };
    let from: Mailbox = config
        .from
        .parse()
        .map_err(|_| InvitationEmailDeliveryError::InvalidMessage)?;
    let to: Mailbox = email
        .recipient
        .parse()
        .map_err(|_| InvitationEmailDeliveryError::InvalidMessage)?;
    let message = Message::builder()
        .from(from)
        .to(to)
        .subject(&email.subject)
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(email.text_body.clone()))
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(email.html_body.clone()),
                ),
        )
        .map_err(|_| InvitationEmailDeliveryError::InvalidMessage)?;
    let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
        .map_err(|_| InvitationEmailDeliveryError::InvalidMessage)?
        .build();
    let receipt = transport
        .send(message)
        .await
        .map_err(|_| InvitationEmailDeliveryError::Retryable)?;
    Ok(ProviderReceipt {
        provider: InvitationEmailProvider::Smtp,
        message_id: smtp_receipt_id(&receipt),
    })
}

fn smtp_receipt_id(receipt: &SmtpResponse) -> String {
    receipt.message().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
    };
    use nibleaf_core::config::InvitationEmailConfig;
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
        sync::Mutex,
    };

    use super::{
        InvitationEmail, InvitationEmailDeliveryError, InvitationEmailProvider, send_postmark_at,
        send_smtp, validate_message,
    };

    fn email_config(
        postmark_api_key: Option<&str>,
        smtp_url: Option<String>,
    ) -> InvitationEmailConfig {
        InvitationEmailConfig {
            application_url: "https://app.example.test".to_owned(),
            from: "sender@example.test".to_owned(),
            postmark_api_key: postmark_api_key.map(str::to_owned),
            postmark_message_stream: Some("transactional".to_owned()),
            smtp_url,
        }
    }

    fn sample_email() -> InvitationEmail {
        InvitationEmail {
            recipient: "recipient@example.test".to_owned(),
            subject: "Invitation".to_owned(),
            text_body: "Accept it".to_owned(),
            html_body: "<p>Accept it</p>".to_owned(),
        }
    }

    #[test]
    fn rejects_header_injection_before_any_provider_call() {
        let email = InvitationEmail {
            recipient: "recipient@example.test".to_owned(),
            subject: "hello\r\nBcc: attacker@example.test".to_owned(),
            text_body: "text".to_owned(),
            html_body: "<p>text</p>".to_owned(),
        };
        assert!(matches!(
            validate_message(&email),
            Err(InvitationEmailDeliveryError::InvalidMessage)
        ));
    }

    #[tokio::test]
    async fn postmark_transport_sends_legacy_stream_and_persists_receipt_shape()
    -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone)]
        struct Captured(Arc<Mutex<Option<(String, String, Value)>>>);
        async fn fake_postmark(
            State(captured): State<Captured>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            let token = headers
                .get("x-postmark-server-token")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let stream = body["MessageStream"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            *captured.0.lock().await = Some((token, stream, body));
            (
                StatusCode::OK,
                Json(json!({"MessageID":"postmark-receipt-1"})),
            )
        }

        let captured = Captured(Arc::new(Mutex::new(None)));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let server_state = captured.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/", post(fake_postmark))
                    .with_state(server_state),
            )
            .await
        });
        let receipt = send_postmark_at(
            &reqwest::Client::new(),
            &email_config(Some("postmark-test-token"), None),
            &sample_email(),
            &endpoint,
        )
        .await?;
        server.abort();
        assert_eq!(receipt.provider, InvitationEmailProvider::Postmark);
        assert_eq!(receipt.message_id, "postmark-receipt-1");
        let (token, stream, body) = captured.0.lock().await.take().ok_or_else(|| {
            std::io::Error::other("fake Postmark server did not receive a request")
        })?;
        assert_eq!(token, "postmark-test-token");
        assert_eq!(stream, "transactional");
        assert_eq!(body["To"], "recipient@example.test");
        assert_eq!(body["From"], "sender@example.test");
        Ok(())
    }

    #[tokio::test]
    async fn smtp_transport_uses_a_real_smtp_exchange_and_records_provider_reply()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            writer.write_all(b"220 fake-smtp ready\r\n").await?;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).await? == 0 {
                    break;
                }
                let command = line.to_ascii_uppercase();
                if command.starts_with("EHLO") || command.starts_with("HELO") {
                    writer.write_all(b"250 fake-smtp\r\n").await?;
                } else if command.starts_with("MAIL FROM") || command.starts_with("RCPT TO") {
                    writer.write_all(b"250 accepted\r\n").await?;
                } else if command.starts_with("DATA") {
                    writer.write_all(b"354 continue\r\n").await?;
                    let mut message = String::new();
                    loop {
                        line.clear();
                        reader.read_line(&mut line).await?;
                        if line == ".\r\n" {
                            break;
                        }
                        message.push_str(&line);
                    }
                    if !message.contains("Invitation")
                        || !message.contains("recipient@example.test")
                    {
                        return Err(std::io::Error::other("unexpected SMTP message"));
                    }
                    writer.write_all(b"250 fake-smtp-receipt-1\r\n").await?;
                } else if command.starts_with("QUIT") {
                    writer.write_all(b"221 bye\r\n").await?;
                    break;
                } else {
                    return Err(std::io::Error::other(format!(
                        "unexpected SMTP command: {line}"
                    )));
                }
            }
            Ok::<(), std::io::Error>(())
        });
        let config = email_config(None, Some(format!("smtp://{address}")));
        let receipt = send_smtp(&config, &sample_email()).await?;
        server.await??;
        assert_eq!(receipt.provider, InvitationEmailProvider::Smtp);
        assert_eq!(receipt.message_id, "fake-smtp-receipt-1");
        Ok(())
    }
}
