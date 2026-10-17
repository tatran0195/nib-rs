//! Durable worker consumer for invitation email delivery.
//!
//! The outbox lease fences workers at event level and this receipt table stores
//! provider evidence separately. A provider acknowledgement can be lost after
//! the send, so retries are deliberately at-least-once.

use nibleaf_core::Config;
use nibleaf_outbox::{ClaimedEvent, Failure};
use sqlx::{FromRow, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    InvitationEmail, InvitationEmailDeliveryError, InvitationTokenClaims, send_invitation_email,
    sign_invitation_token,
};

#[derive(FromRow)]
struct DeliveryCandidate {
    recipient_email: String,
    expires_at: OffsetDateTime,
}

/// Claims a delivery receipt, calls the selected provider outside a database
/// transaction, and writes provider acknowledgement or retry/dead-letter state.
/// The caller remains responsible for marking the *outbox* event according to
/// the returned `Failure`.
///
/// # Errors
///
/// Returns a permanent or retryable failure after persisting the corresponding
/// delivery-receipt state whenever that persistence was possible.
pub async fn deliver_invitation_email_event(
    pool: &PgPool,
    config: &Config,
    event: &ClaimedEvent,
) -> Result<(), Failure> {
    let invitation_id = Uuid::parse_str(&event.aggregate_id).map_err(|_| Failure::Permanent {
        message: "invalid invitation aggregate identifier".to_owned(),
    })?;
    let candidate = sqlx::query_as::<_, DeliveryCandidate>(
        r"UPDATE member_invitation_delivery AS delivery
           SET state = 'processing', attempts = delivery.attempts + 1,
               leased_until = $2, lease_token = $3, last_error = NULL, updated_at = NOW()
           FROM member_invitation AS invitation
           WHERE delivery.invitation_id = invitation.id
             AND delivery.invitation_id = $1
             AND invitation.status = 'pending'
             AND invitation.delivery_mode = 'email'
             AND invitation.expires_at > NOW()
             AND (delivery.state = 'pending' OR (delivery.state = 'processing' AND delivery.leased_until < NOW()))
           RETURNING invitation.recipient_email, invitation.expires_at",
    )
    .bind(invitation_id)
    .bind(event.leased_until)
    .bind(event.lease_token)
    .fetch_optional(pool)
    .await
    .map_err(database_retryable)?;
    let Some(candidate) = candidate else {
        // Cancellation, acceptance, expiry, or a newer fenced worker makes this
        // event obsolete. It is safe to acknowledge the outbox event.
        return Ok(());
    };

    let token = sign_invitation_token(
        &config.invitation_signing_key,
        InvitationTokenClaims {
            invitation_id,
            expires_at: candidate.expires_at,
        },
    )
    .map_err(|_| Failure::Permanent {
        message: "unable to mint invitation acceptance token".to_owned(),
    })?;
    let acceptance_url = format!(
        "{}/invitations/{}",
        config
            .invitation_email
            .application_url
            .trim_end_matches('/'),
        token
    );
    let email = InvitationEmail {
        recipient: candidate.recipient_email,
        subject: "You have been invited to Nibleaf".to_owned(),
        text_body: format!(
            "You have been invited to Nibleaf. Accept the invitation: {acceptance_url}"
        ),
        html_body: format!(
            "<p>You have been invited to Nibleaf.</p><p><a href=\"{acceptance_url}\">Accept invitation</a></p>"
        ),
    };
    let result =
        send_invitation_email(&reqwest::Client::new(), &config.invitation_email, &email).await;
    match result {
        Ok(receipt) => {
            let saved = sqlx::query(
                "UPDATE member_invitation_delivery SET state='sent', provider=$2, provider_message_id=$3, sent_at=NOW(), leased_until=NULL, lease_token=NULL, updated_at=NOW() WHERE invitation_id=$1 AND state='processing' AND lease_token=$4",
            )
            .bind(invitation_id)
            .bind(match receipt.provider { crate::InvitationEmailProvider::Postmark => "postmark", crate::InvitationEmailProvider::Smtp => "smtp" })
            .bind(receipt.message_id)
            .bind(event.lease_token)
            .execute(pool)
            .await
            .map_err(database_retryable)?;
            if saved.rows_affected() != 1 {
                return Err(Failure::Retryable {
                    message: "invitation delivery receipt lease was lost".to_owned(),
                });
            }
            Ok(())
        }
        Err(error) => {
            let failure = classify_delivery_error(&error);
            let dead_letter = matches!(failure, Failure::Permanent { .. });
            sqlx::query(
                "UPDATE member_invitation_delivery SET state=CASE WHEN $2 THEN 'dead_letter' ELSE 'pending' END, last_error=$3, leased_until=NULL, lease_token=NULL, updated_at=NOW() WHERE invitation_id=$1 AND state='processing' AND lease_token=$4",
            )
            .bind(invitation_id)
            .bind(dead_letter)
            .bind(match &failure { Failure::Retryable { message } | Failure::Permanent { message } => message })
            .bind(event.lease_token)
            .execute(pool)
            .await
            .map_err(database_retryable)?;
            Err(failure)
        }
    }
}

fn database_retryable(_: sqlx::Error) -> Failure {
    Failure::Retryable {
        message: "invitation delivery receipt persistence failed".to_owned(),
    }
}

fn classify_delivery_error(error: &InvitationEmailDeliveryError) -> Failure {
    match error {
        InvitationEmailDeliveryError::Retryable => Failure::Retryable {
            message: "invitation email provider temporarily unavailable".to_owned(),
        },
        InvitationEmailDeliveryError::Unconfigured => Failure::Permanent {
            message: "no invitation email provider is configured".to_owned(),
        },
        InvitationEmailDeliveryError::InvalidMessage => Failure::Permanent {
            message: "invitation email rendering is invalid".to_owned(),
        },
        InvitationEmailDeliveryError::Permanent => Failure::Permanent {
            message: "invitation email provider rejected the request".to_owned(),
        },
    }
}
