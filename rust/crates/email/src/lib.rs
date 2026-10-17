#![forbid(unsafe_code)]

//! Shared transactional email transport for feature-owned rendering workflows.
//! Provider choice is Postmark-first and SMTP fallback, matching the verified
//! legacy behavior. A successful return means provider acknowledgement only.

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart, SinglePart, header::ContentType},
};
use nibleaf_core::config::InvitationEmailConfig;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const POSTMARK_ENDPOINT: &str = "https://api.postmarkapp.com/email";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmailProvider {
    Postmark,
    Smtp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionalEmail {
    pub recipient: String,
    pub subject: String,
    pub text_body: String,
    pub html_body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderReceipt {
    pub provider: EmailProvider,
    pub message_id: String,
}

#[derive(Debug, Error)]
pub enum EmailDeliveryError {
    #[error("no email provider is configured")]
    Unconfigured,
    #[error("email message is invalid")]
    InvalidMessage,
    #[error("email provider rejected the request")]
    Permanent,
    #[error("email provider is temporarily unavailable")]
    Retryable,
}

/// Resolves the configured provider using the verified legacy precedence.
///
/// # Errors
/// Returns `Unconfigured` when neither provider has usable configuration.
pub fn configured_provider(
    config: &InvitationEmailConfig,
) -> Result<EmailProvider, EmailDeliveryError> {
    if config
        .postmark_api_key
        .as_deref()
        .is_some_and(|key| !key.trim().is_empty())
    {
        return Ok(EmailProvider::Postmark);
    }
    if config
        .smtp_url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
    {
        return Ok(EmailProvider::Smtp);
    }
    Err(EmailDeliveryError::Unconfigured)
}

/// Sends a rendered transactional email through Postmark or SMTP.
///
/// # Errors
/// Returns classified configuration, message, permanent-provider, or temporary
/// provider failures; no recipient or token is included in the error.
pub async fn send_transactional_email(
    client: &Client,
    config: &InvitationEmailConfig,
    message: &TransactionalEmail,
) -> Result<ProviderReceipt, EmailDeliveryError> {
    validate(message)?;
    match configured_provider(config)? {
        EmailProvider::Postmark => send_postmark(client, config, message).await,
        EmailProvider::Smtp => send_smtp(config, message).await,
    }
}

fn validate(message: &TransactionalEmail) -> Result<(), EmailDeliveryError> {
    if message.recipient.trim().is_empty()
        || message.subject.trim().is_empty()
        || message.text_body.is_empty()
        || message.html_body.is_empty()
        || message.recipient.contains(['\r', '\n'])
        || message.subject.contains(['\r', '\n'])
    {
        return Err(EmailDeliveryError::InvalidMessage);
    }
    Ok(())
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
struct PostmarkResponse {
    #[serde(rename = "MessageID")]
    message_id: String,
}

async fn send_postmark(
    client: &Client,
    config: &InvitationEmailConfig,
    message: &TransactionalEmail,
) -> Result<ProviderReceipt, EmailDeliveryError> {
    send_postmark_at(client, config, message, POSTMARK_ENDPOINT).await
}

async fn send_postmark_at(
    client: &Client,
    config: &InvitationEmailConfig,
    message: &TransactionalEmail,
    endpoint: &str,
) -> Result<ProviderReceipt, EmailDeliveryError> {
    let key = config
        .postmark_api_key
        .as_deref()
        .ok_or(EmailDeliveryError::Unconfigured)?;
    let response = client
        .post(endpoint)
        .header("X-Postmark-Server-Token", key)
        .json(&PostmarkRequest {
            from: &config.from,
            to: &message.recipient,
            subject: &message.subject,
            text_body: &message.text_body,
            html_body: &message.html_body,
            message_stream: config.postmark_message_stream.as_deref(),
        })
        .send()
        .await
        .map_err(|_| EmailDeliveryError::Retryable)?;
    if !response.status().is_success() {
        return Err(
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status().is_server_error()
            {
                EmailDeliveryError::Retryable
            } else {
                EmailDeliveryError::Permanent
            },
        );
    }
    let receipt = response
        .json::<PostmarkResponse>()
        .await
        .map_err(|_| EmailDeliveryError::Retryable)?;
    if receipt.message_id.trim().is_empty() {
        return Err(EmailDeliveryError::Retryable);
    }
    Ok(ProviderReceipt {
        provider: EmailProvider::Postmark,
        message_id: receipt.message_id,
    })
}

async fn send_smtp(
    config: &InvitationEmailConfig,
    email: &TransactionalEmail,
) -> Result<ProviderReceipt, EmailDeliveryError> {
    let smtp_url = config
        .smtp_url
        .as_deref()
        .ok_or(EmailDeliveryError::Unconfigured)?;
    let from: Mailbox = config
        .from
        .parse()
        .map_err(|_| EmailDeliveryError::InvalidMessage)?;
    let to: Mailbox = email
        .recipient
        .parse()
        .map_err(|_| EmailDeliveryError::InvalidMessage)?;
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
        .map_err(|_| EmailDeliveryError::InvalidMessage)?;
    let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)
        .map_err(|_| EmailDeliveryError::InvalidMessage)?
        .build();
    let receipt = transport
        .send(message)
        .await
        .map_err(|_| EmailDeliveryError::Retryable)?;
    Ok(ProviderReceipt {
        provider: EmailProvider::Smtp,
        message_id: receipt.message().collect::<Vec<_>>().join(" "),
    })
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
        EmailDeliveryError, EmailProvider, TransactionalEmail, send_postmark_at, send_smtp,
        validate,
    };

    fn config(postmark_api_key: Option<&str>, smtp_url: Option<String>) -> InvitationEmailConfig {
        InvitationEmailConfig {
            application_url: "https://app.example.test".to_owned(),
            from: "sender@example.test".to_owned(),
            postmark_api_key: postmark_api_key.map(str::to_owned),
            postmark_message_stream: Some("transactional".to_owned()),
            smtp_url,
        }
    }

    fn email() -> TransactionalEmail {
        TransactionalEmail {
            recipient: "recipient@example.test".to_owned(),
            subject: "One-time code".to_owned(),
            text_body: "123456".to_owned(),
            html_body: "<p>123456</p>".to_owned(),
        }
    }

    #[test]
    fn rejects_header_injection_before_any_provider_call() {
        let invalid = TransactionalEmail {
            subject: "hello\r\nBcc: attacker@example.test".to_owned(),
            ..email()
        };
        assert!(matches!(
            validate(&invalid),
            Err(EmailDeliveryError::InvalidMessage)
        ));
    }

    #[tokio::test]
    async fn postmark_request_uses_provider_contract_and_receipt()
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
        let state = captured.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/", post(fake_postmark))
                    .with_state(state),
            )
            .await
        });
        let receipt = send_postmark_at(
            &reqwest::Client::new(),
            &config(Some("postmark-test-token"), None),
            &email(),
            &endpoint,
        )
        .await?;
        server.abort();
        assert_eq!(receipt.provider, EmailProvider::Postmark);
        assert_eq!(receipt.message_id, "postmark-receipt-1");
        let (token, stream, body) = captured
            .0
            .lock()
            .await
            .take()
            .ok_or_else(|| std::io::Error::other("fake Postmark did not receive a request"))?;
        assert_eq!(token, "postmark-test-token");
        assert_eq!(stream, "transactional");
        assert_eq!(body["To"], "recipient@example.test");
        assert_eq!(body["From"], "sender@example.test");
        Ok(())
    }

    #[tokio::test]
    async fn smtp_transport_exchanges_message_and_captures_receipt()
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
                    if !message.contains("One-time code")
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
        let receipt = send_smtp(&config(None, Some(format!("smtp://{address}"))), &email()).await?;
        server.await??;
        assert_eq!(receipt.provider, EmailProvider::Smtp);
        assert_eq!(receipt.message_id, "fake-smtp-receipt-1");
        Ok(())
    }
}
