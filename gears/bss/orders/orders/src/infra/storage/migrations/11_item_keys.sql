-- Forward correction of the shipped foundation (D-246): a database `main` migrated never
-- re-applies an edited 01, so the committed product identity and the pinned-line rule without a
-- scalar scope key arrive here. Complete new evidence is authoritative; historical scalar
-- evidence stays untouched.
ALTER TABLE bss_orders__order_line ADD COLUMN catalog_subscription_product_key text;
ALTER TABLE bss_orders__order_line ADD COLUMN overlap_item_keys jsonb;
ALTER TABLE bss_orders__order_line DROP CONSTRAINT ck_bss_orders__order_line__2;
ALTER TABLE bss_orders__order_line ADD CONSTRAINT ck_bss_orders__order_line__2 CHECK (commercial_attempt_id IS NOT NULL AND pricing_acceptance_id IS NOT NULL AND pricing_request_digest IS NOT NULL AND pricing_terms_digest IS NOT NULL AND order_pin IS NOT NULL);
ALTER TABLE bss_orders__order_line ADD CONSTRAINT ck_bss_orders__order_line__item_keys CHECK (
    overlap_item_keys IS NULL OR (
        jsonb_typeof(overlap_item_keys) = 'array'
        AND overlap_scope_key IS NULL
        AND catalog_subscription_product_key IS NULL
    )
);
