-- Complete new evidence is authoritative; historical scalar evidence stays untouched.
ALTER TABLE bss_orders__order_line ADD COLUMN overlap_item_keys jsonb;
ALTER TABLE bss_orders__order_line ADD CONSTRAINT ck_bss_orders__order_line__item_keys CHECK (
    overlap_item_keys IS NULL OR (
        jsonb_typeof(overlap_item_keys) = 'array'
        AND overlap_scope_key IS NULL
        AND catalog_subscription_product_key IS NULL
    )
);
