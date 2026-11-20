-- API Key Management (P1). PostgreSQL reference DDL for DESIGN.md §3.7.
-- Backend migrations preserve UUID equality, byte-exact MACs, case-sensitive
-- subject types and UTC timestamp precision on PostgreSQL, SQLite and MySQL.

-- Local serialization row for an owner. It survives deletion in Account
-- Management so administrators can revoke the remaining keys during offboarding.
CREATE TABLE api_key_management__api_key_owners (
    owner_tenant_id  uuid              NOT NULL,
    owner_id         uuid              NOT NULL,
    owner_type       text COLLATE "C"  NOT NULL,

    CONSTRAINT pk_akm_owners PRIMARY KEY (owner_tenant_id, owner_id, owner_type)
);

-- Verification reads by public id. The issuance MAC binds the presented
-- credential to its owner and lifetime. key_hint holds the prefix and suffix.
CREATE TABLE api_key_management__api_keys (
    id                 uuid              NOT NULL, -- gear issues UUIDv4 in P1; PK accepts any UUID version
    owner_tenant_id    uuid              NOT NULL,
    owner_id           uuid              NOT NULL,
    owner_type         text COLLATE "C"  NOT NULL,
    mac_key_id         text COLLATE "C"  NOT NULL, -- public ID of the provider key used at issuance
    issuance_mac       bytea,                      -- cleared atomically on revocation
    format_version     smallint          NOT NULL, -- version of the credential wire format
    key_hint           text              NOT NULL,
    name               text              NOT NULL,
    created_at         timestamptz       NOT NULL,
    expires_at         timestamptz       NOT NULL,
    revoked_at         timestamptz,
    revocation_reason  text,
    purge_after        timestamptz       NOT NULL,

    CONSTRAINT pk_akm_keys PRIMARY KEY (id),
    CONSTRAINT fk_akm_key_owner FOREIGN KEY (owner_tenant_id, owner_id, owner_type)
        REFERENCES api_key_management__api_key_owners (owner_tenant_id, owner_id, owner_type)
        ON DELETE RESTRICT,
    CONSTRAINT ck_akm_mac_key_id CHECK (mac_key_id <> ''),
    CONSTRAINT ck_akm_issuance_mac CHECK (
        (revoked_at IS NULL AND issuance_mac IS NOT NULL AND octet_length(issuance_mac) = 32)
        OR (revoked_at IS NOT NULL AND issuance_mac IS NULL)
    ),
    CONSTRAINT ck_akm_format_version CHECK (format_version > 0),
    CONSTRAINT ck_akm_lifetime CHECK (expires_at > created_at),
    CONSTRAINT ck_akm_revocation_pair CHECK ((revoked_at IS NULL) = (revocation_reason IS NULL)),
    CONSTRAINT ck_akm_revocation_time CHECK (revoked_at >= created_at AND revoked_at < expires_at),
    CONSTRAINT ck_akm_revocation_reason CHECK (revocation_reason IN (
        'key_compromise', 'superseded', 'no_longer_needed', 'privilege_withdrawn', 'owner_removed'
    )),
    -- Exactly 90 elapsed days, independent of the database session's time zone.
    CONSTRAINT ck_akm_retention CHECK (
        purge_after = COALESCE(revoked_at, expires_at) + INTERVAL '2160 hours'
    )
);

-- Owner and tenant-admin pages share the same (created_at, id) cursor order.
CREATE INDEX idx_akm_owner_page ON api_key_management__api_keys
    (owner_tenant_id, owner_id, owner_type, created_at DESC, id DESC);
CREATE INDEX idx_akm_tenant_page ON api_key_management__api_keys
    (owner_tenant_id, created_at DESC, id DESC);

-- Count or revoke keys still active at the cutoff under the owner lock.
CREATE INDEX idx_akm_owner_validity ON api_key_management__api_keys
    (owner_tenant_id, owner_id, owner_type, revoked_at, expires_at);

-- Seek the next bounded batch of due rows, including ties at the deadline.
CREATE INDEX idx_akm_purge ON api_key_management__api_keys (purge_after, id);
