-- Fresh isolated PostgreSQL regression for m041 transaction/functional immutability.
CREATE FUNCTION pg_temp.expect_locked(statement text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
  BEGIN EXECUTE statement;
  EXCEPTION WHEN raise_exception THEN
    IF SQLERRM LIKE 'currency scale locked:%' THEN RETURN; END IF;
    RAISE;
  END;
  RAISE EXCEPTION 'expected currency scale locked: %', statement;
END $$;
BEGIN;
INSERT INTO bss.ledger_journal_entry(entry_id,tenant_id,legal_entity_id,period_id,entry_currency,source_doc_type,source_business_id,effective_at,origin,posted_by_actor_id,correlation_id)
VALUES('00000000-0000-0000-0000-000000000009','00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000003','202610','EUR','INVOICE_POST','r4',CURRENT_DATE,'SYSTEM','00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000009');
INSERT INTO bss.ledger_journal_line(line_id,entry_id,tenant_id,period_id,payer_tenant_id,account_id,account_class,side,amount,currency,currency_scale,mapping_status,functional_amount,functional_currency,functional_currency_scale)
SELECT gen_random_uuid(),'00000000-0000-0000-0000-000000000009','00000000-0000-0000-0000-000000000001','202610','00000000-0000-0000-0000-000000000005','00000000-0000-0000-0000-000000000002','AR',s,'1','EUR',2,'RESOLVED','1.001','USD',3
FROM unnest(ARRAY['DR','CR']) s;
COMMIT;
SELECT pg_temp.expect_locked('INSERT INTO bss.ledger_currency_scale_registry VALUES (''00000000-0000-0000-0000-000000000001'',''EUR'',3,''test'')');
SELECT pg_temp.expect_locked('INSERT INTO bss.ledger_currency_scale_registry VALUES (''00000000-0000-0000-0000-000000000001'',''USD'',2,''test'')');
INSERT INTO bss.ledger_currency_scale_registry VALUES ('00000000-0000-0000-0000-000000000001','EUR',2,'test'), ('00000000-0000-0000-0000-000000000001','USD',3,'test');
SELECT pg_temp.expect_locked('UPDATE bss.ledger_currency_scale_registry SET currency_scale=3 WHERE currency=''EUR''');
SELECT pg_temp.expect_locked('UPDATE bss.ledger_currency_scale_registry SET currency_scale=2 WHERE currency=''USD''');
UPDATE bss.ledger_currency_scale_registry SET currency_scale=currency_scale;
INSERT INTO bss.ledger_currency_scale_registry VALUES ('00000000-0000-0000-0000-000000000001','JPY',0,'test');
UPDATE bss.ledger_currency_scale_registry SET currency_scale=28 WHERE currency='JPY';
INSERT INTO bss.ledger_currency_scale_registry VALUES ('00000000-0000-0000-0000-000000000007','EUR',3,'test');
SELECT 'R4 transaction/functional first-insert and update scale locks passed' AS result;
