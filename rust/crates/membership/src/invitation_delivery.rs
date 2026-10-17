//! Provider-selection rule shared by the future invitation email worker.
//!
//! This is deliberately separate from transport code: it preserves the
//! verified legacy precedence without pretending that configuration means an
//! email was sent.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvitationEmailProvider {
    Postmark,
    Smtp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvitationEmailProviderError {
    Unconfigured,
}

/// Selects the provider ordering used by the legacy worker: a non-blank
/// Postmark credential wins; SMTP is the self-hosted fallback.
///
/// # Errors
///
/// Returns `Unconfigured` when neither provider has usable configuration.
pub fn resolve_invitation_email_provider(
    postmark_api_key: Option<&str>,
    smtp_url: Option<&str>,
) -> Result<InvitationEmailProvider, InvitationEmailProviderError> {
    if postmark_api_key.is_some_and(|value| !value.trim().is_empty()) {
        return Ok(InvitationEmailProvider::Postmark);
    }
    if smtp_url.is_some_and(|value| !value.trim().is_empty()) {
        return Ok(InvitationEmailProvider::Smtp);
    }
    Err(InvitationEmailProviderError::Unconfigured)
}

#[cfg(test)]
mod tests {
    use super::{
        InvitationEmailProvider, InvitationEmailProviderError, resolve_invitation_email_provider,
    };

    #[test]
    fn postmark_has_legacy_precedence_over_smtp() {
        assert_eq!(
            resolve_invitation_email_provider(Some("postmark-key"), Some("smtp://host")),
            Ok(InvitationEmailProvider::Postmark)
        );
    }

    #[test]
    fn smtp_is_the_fallback_and_blank_values_do_not_configure_a_provider() {
        assert_eq!(
            resolve_invitation_email_provider(Some(" "), Some("smtp://host")),
            Ok(InvitationEmailProvider::Smtp)
        );
        assert_eq!(
            resolve_invitation_email_provider(None, Some("\t")),
            Err(InvitationEmailProviderError::Unconfigured)
        );
    }
}
