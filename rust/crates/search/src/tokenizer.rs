//! Programmatic Lindera analysis without the wrapper's YAML/configuration path.
//!
//! Tantivy's `Tokenizer` trait cannot return an error, whereas Lindera can. A
//! shared, first-error latch therefore follows every tokenizer clone. Builders
//! MUST check it after the writer's synchronous commit barrier and before a
//! manifest is written; queries MUST check it after parsing and before search.
//! An empty internal stream on failure is never a successful partial index or
//! a successful empty query. The original error is retained, not unwrapped.

use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
    vec::IntoIter,
};

use lindera::{dictionary::load_dictionary, error::LinderaError, mode::Mode, segmenter::Segmenter};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

use crate::SearchError;

#[derive(Clone, Default)]
pub(crate) struct TokenizerHealth(Arc<OnceLock<Arc<LinderaError>>>);

impl TokenizerHealth {
    pub(crate) fn check(&self) -> Result<(), SearchError> {
        if let Some(error) = self.0.get() {
            return Err(SearchError::Tokenizer(Arc::clone(error)));
        }
        Ok(())
    }

    fn record_failure(&self, error: LinderaError) {
        // Keep the first source error with bounded memory, even when indexing
        // threads or multiple analyzed fields fail in the same operation.
        self.0.get_or_init(|| Arc::new(error));
    }
}

#[derive(Clone)]
pub(crate) struct LinderaTokenizer {
    segmenter: Segmenter,
    health: TokenizerHealth,
}

impl LinderaTokenizer {
    pub(crate) fn load(uri: &str, health: TokenizerHealth) -> Result<Self, SearchError> {
        let dictionary =
            load_dictionary(uri).map_err(|error| SearchError::Tokenizer(Arc::new(error)))?;
        let mut segmenter = Segmenter::new(Mode::Normal, dictionary, None);
        // Keep the pre-v6 unknown-word grouping policy explicit. This does not
        // assert identical historical dictionaries/postings; manifest runtime
        // identity still mandates a new generation for the upgraded analyzer.
        segmenter.unknown_word_ladder = false;
        Ok(Self { segmenter, health })
    }
}

impl Tokenizer for LinderaTokenizer {
    type TokenStream<'a> = LinderaTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        LinderaTokenStream::from_segmentation(
            self.segmenter.segment(Cow::Borrowed(text)),
            &self.health,
        )
    }
}

pub(crate) struct LinderaTokenStream {
    tokens: IntoIter<Token>,
    current: Token,
}

impl LinderaTokenStream {
    fn from_segmentation(
        result: lindera::LinderaResult<Vec<lindera::token::Token<'_>>>,
        health: &TokenizerHealth,
    ) -> Self {
        let tokens = match result {
            Ok(tokens) => tokens
                .into_iter()
                .map(|token| Token {
                    offset_from: token.byte_start,
                    offset_to: token.byte_end,
                    position: token.position,
                    text: token.surface.into_owned(),
                    position_length: token.position_length,
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                health.record_failure(error);
                Vec::new()
            }
        };
        Self {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

impl TokenStream for LinderaTokenStream {
    fn advance(&mut self) -> bool {
        let Some(token) = self.tokens.next() else {
            return false;
        };
        self.current = token;
        true
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use lindera::error::LinderaErrorKind;

    use super::*;

    #[derive(Clone)]
    struct FailingTokenizer(TokenizerHealth);

    impl Tokenizer for FailingTokenizer {
        type TokenStream<'a> = LinderaTokenStream;

        fn token_stream<'a>(&'a mut self, _text: &'a str) -> Self::TokenStream<'a> {
            LinderaTokenStream::from_segmentation(
                Err(LinderaErrorKind::Parse
                    .with_error(std::io::Error::other("injected segmentation failure"))),
                &self.0,
            )
        }
    }

    pub(crate) fn install_failing_tokenizers(index: &tantivy::Index) -> TokenizerHealth {
        let health = TokenizerHealth::default();
        for name in [
            crate::JAPANESE_TOKENIZER_NAME,
            crate::CHINESE_TOKENIZER_NAME,
        ] {
            index
                .tokenizers()
                .register(name, FailingTokenizer(health.clone()));
        }
        health
    }

    fn tokens(tokenizer: &mut LinderaTokenizer, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokenizer
            .token_stream(text)
            .process(&mut |token| tokens.push(token.clone()));
        tokens
    }

    #[test]
    fn japanese_offsets_positions_and_stream_reset_are_preserved() -> Result<(), SearchError> {
        let health = TokenizerHealth::default();
        let mut tokenizer = LinderaTokenizer::load("embedded://ipadic", health.clone())?;
        let text = "東京 国際空港\nRust 文書";
        let first = tokens(&mut tokenizer, text);
        assert!(first.iter().any(|token| token.text == "東京"));
        for (position, token) in first.iter().enumerate() {
            assert_eq!(token.position, position);
            assert_eq!(token.position_length, 1);
            assert_eq!(
                text.get(token.offset_from..token.offset_to),
                Some(token.text.as_str())
            );
        }
        let second = tokens(&mut tokenizer, "空港");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].position, 0);
        assert_eq!((second[0].offset_from, second[0].offset_to), (0, 6));
        assert_eq!(tokens(&mut tokenizer, "").len(), 0);
        health.check()
    }

    #[test]
    fn chinese_tokens_have_source_byte_offsets_not_character_offsets() -> Result<(), SearchError> {
        let health = TokenizerHealth::default();
        let mut tokenizer = LinderaTokenizer::load("embedded://cc-cedict", health.clone())?;
        let text = "搜索引擎支持中文检索";
        let tokens = tokens(&mut tokenizer, text);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            ["搜索引擎", "支持", "中文", "检索"],
        );
        for token in tokens {
            assert_eq!(
                text.get(token.offset_from..token.offset_to),
                Some(token.text.as_str())
            );
        }
        health.check()
    }

    #[test]
    fn segmentation_failure_is_latched_across_clones_and_cannot_be_reset() {
        let health = TokenizerHealth::default();
        let clone = health.clone();
        let error = LinderaErrorKind::Parse
            .with_error(std::io::Error::other("injected segmentation failure"));
        let mut failed = LinderaTokenStream::from_segmentation(Err(error), &health);
        assert!(!failed.advance());
        assert!(matches!(clone.check(), Err(SearchError::Tokenizer(_))));
        let mut empty_success = LinderaTokenStream::from_segmentation(Ok(Vec::new()), &clone);
        assert!(!empty_success.advance());
        assert!(matches!(health.check(), Err(SearchError::Tokenizer(_))));
    }

    #[test]
    fn dictionary_load_failure_returns_a_typed_error_without_panicking() {
        assert!(matches!(
            LinderaTokenizer::load("embedded://not-a-dictionary", TokenizerHealth::default()),
            Err(SearchError::Tokenizer(_))
        ));
    }
}
