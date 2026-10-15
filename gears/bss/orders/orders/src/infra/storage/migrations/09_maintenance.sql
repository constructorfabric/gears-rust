-- S2-11 maintenance access paths. The audit worker enumerates live orders and committed
-- evidence by immutable audit namespace (D-100 reconciliation); nothing else reads by it.
CREATE INDEX idx_bss_orders__order__11 ON bss_orders__order (audit_tenant_id,order_id);

CREATE INDEX idx_bss_orders__transition_audit__3 ON bss_orders__transition_audit (audit_tenant_id,order_id,sequence) WHERE outcome='committed';

-- D-100 item 1: members are streamed into the checkpoint transaction before the header, whose
-- digest covers them, is written last; the header/member link is checked at commit together
-- with the cardinality trigger, so header and members still publish atomically.
ALTER TABLE bss_orders__audit_checkpoint_member DROP CONSTRAINT fk_bss_orders__audit_checkpoint_member__0;
ALTER TABLE bss_orders__audit_checkpoint_member ADD CONSTRAINT fk_bss_orders__audit_checkpoint_member__header FOREIGN KEY (audit_tenant_id,checkpoint_sequence) REFERENCES bss_orders__audit_checkpoint(audit_tenant_id,checkpoint_sequence) DEFERRABLE INITIALLY DEFERRED;
