-- A project may publish navigation groups before it has a searchable PAGE.
-- Such a deployment must still promote a valid, intentionally empty lexical
-- generation; otherwise enabling native search would regress publication
-- availability for an existing user-visible project shape.

BEGIN;

ALTER TABLE rust_search_generation
    DROP CONSTRAINT rust_search_generation_document_count_check;

ALTER TABLE rust_search_generation
    ADD CONSTRAINT rust_search_generation_document_count_check
    CHECK (document_count IS NULL OR document_count >= 0);

COMMIT;
