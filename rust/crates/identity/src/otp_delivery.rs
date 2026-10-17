//! Worker-side email-OTP outbox consumer.

use nibleaf_core::config::InvitationEmailConfig;
use nibleaf_email::{
    EmailDeliveryError, EmailProvider, TransactionalEmail, send_transactional_email,
};
use nibleaf_outbox::{ClaimedEvent, Failure};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{EncryptionKey, decrypt_secret};

const OTP_DELIVERY_AAD: &[u8] = b"nibleaf.identity.otp.delivery.v1";

#[derive(FromRow)]
struct DeliveryRow {
    id: Uuid,
    ciphertext: Vec<u8>,
    state: String,
    normalized_email: String,
    purpose: String,
    expires_at: OffsetDateTime,
    challenge_state: String,
}

/// Delivers a claimed `identity.otp.delivery.requested` event. Provider calls
/// occur outside transactions. Provider acknowledgement is at-least-once: a
/// process crash after acknowledgement can result in a retry, which preserves
/// delivery rather than incorrectly claiming exactly-once behavior.
///
/// # Errors
/// Returns classified outbox failures. Provider or recipient details are never
/// included in their messages.
pub async fn deliver_otp_email_event(
    pool: &PgPool,
    config: &InvitationEmailConfig,
    keys: &[EncryptionKey],
    client: &reqwest::Client,
    event: &ClaimedEvent,
) -> Result<(), Failure> {
    if event.tenant_id.is_some() {
        return Err(permanent("identity OTP event must be platform-scoped"));
    }
    let delivery_id = event
        .payload
        .get("delivery_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| permanent("identity OTP event payload is invalid"))?;
    let row = sqlx::query_as::<_, DeliveryRow>(
        r"
        SELECT d.id, d.ciphertext, d.state, c.normalized_email, c.purpose,
               c.expires_at, c.state AS challenge_state
        FROM rust_identity_otp_delivery AS d
        INNER JOIN rust_identity_otp_challenge AS c ON c.id = d.challenge_id
        WHERE d.id = $1
        ",
    )
    .bind(delivery_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| retryable("database unavailable while loading OTP delivery"))?
    .ok_or_else(|| permanent("OTP delivery no longer exists"))?;
    if row.state == "delivered" || row.state == "cancelled" {
        return Ok(());
    }
    if row.state != "pending" {
        return Err(permanent("OTP delivery has an unknown state"));
    }
    if row.challenge_state != "pending" || row.expires_at <= OffsetDateTime::now_utc() {
        cancel_delivery(pool, row.id).await?;
        return Ok(());
    }
    let code = decrypt_secret(keys, &row.ciphertext, OTP_DELIVERY_AAD)
        .map_err(|_| permanent("OTP delivery secret is unavailable"))?;
    let code = String::from_utf8(code).map_err(|_| permanent("OTP delivery secret is invalid"))?;
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(permanent("OTP delivery secret is invalid"));
    }
    let (subject, text_body, html_body) =
        render(&row.purpose, &code).ok_or_else(|| permanent("OTP delivery purpose is invalid"))?;
    let receipt = send_transactional_email(
        client,
        config,
        &TransactionalEmail {
            recipient: row.normalized_email,
            subject,
            text_body,
            html_body,
        },
    )
    .await
    .map_err(|error| map_delivery_error(&error))?;
    let provider = match receipt.provider {
        EmailProvider::Postmark => "postmark",
        EmailProvider::Smtp => "smtp",
    };
    let result = sqlx::query(
        r"
        UPDATE rust_identity_otp_delivery
        SET state = 'delivered', ciphertext = '\x'::bytea, provider = $2,
            provider_message_id = $3, delivered_at = NOW(), updated_at = NOW()
        WHERE id = $1 AND state = 'pending'
        ",
    )
    .bind(row.id)
    .bind(provider)
    .bind(receipt.message_id)
    .execute(pool)
    .await
    .map_err(|_| retryable("database unavailable while recording OTP delivery"))?;
    if result.rows_affected() != 1 {
        return Err(retryable(
            "OTP delivery state changed while being delivered",
        ));
    }
    Ok(())
}

async fn cancel_delivery(pool: &PgPool, delivery_id: Uuid) -> Result<(), Failure> {
    sqlx::query(
        "UPDATE rust_identity_otp_delivery SET state='cancelled', ciphertext='\\x'::bytea, updated_at=NOW() WHERE id=$1 AND state='pending'",
    )
    .bind(delivery_id)
    .execute(pool)
    .await
    .map_err(|_| retryable("database unavailable while cancelling expired OTP delivery"))?;
    Ok(())
}

fn render(purpose: &str, code: &str) -> Option<(String, String, String)> {
    let action = match purpose {
        "sign_in" => "sign in to Nibleaf",
        "verify_email" => "verify your Nibleaf email address",
        "change_email" => "confirm your Nibleaf email change",
        _ => return None,
    };
    let subject = format!("Your Nibleaf verification code: {code}");
    let text = format!(
        "Use this code to {action}: {code}\n\nThis code expires in 10 minutes. If you did not request it, you can ignore this email."
    );
    let html = format!(
        "<p>Use this code to {action}:</p><p><strong>{code}</strong></p><p>This code expires in 10 minutes. If you did not request it, you can ignore this email.</p>"
    );
    Some((subject, text, html))
}

fn map_delivery_error(error: &EmailDeliveryError) -> Failure {
    match error {
        EmailDeliveryError::Retryable | EmailDeliveryError::Unconfigured => {
            retryable("OTP email provider is temporarily unavailable")
        }
        EmailDeliveryError::InvalidMessage | EmailDeliveryError::Permanent => {
            permanent("OTP email provider rejected delivery")
        }
    }
}

fn retryable(message: &str) -> Failure {
    Failure::Retryable {
        message: message.to_owned(),
    }
}
fn permanent(message: &str) -> Failure {
    Failure::Permanent {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn rendering_has_no_recipient_data_and_only_known_purposes() {
        match render("sign_in", "123456") {
            Some((subject, text, html)) => assert!(
                subject.contains("123456")
                    && text.contains("10 minutes")
                    && html.contains("123456")
            ),
            None => unreachable!("known purpose must render"),
        }
        assert!(render("unknown", "123456").is_none());
    }
}
