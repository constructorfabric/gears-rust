# Plane separation scenarios

End-to-end scenarios for the two route sets through the real api-gateway (D17, D20): the platform API under `/types-registry/v2/platform/` admits a validated `X-ToolKit-Internal-Token` only, and the tenant reads under `/types-registry/v2/` admit a validated bearer only. The gateway validates the token with its `internal_auth` shared secret from `config/e2e-local.yaml`. Every refusal is a canonical `401` Problem whose `context.reason` names it.

Every scenario has an e2e test in [test_planes.py](../test_planes.py). The same matrix also runs in Rust against the `OoP` listener stack (`types-registry/tests/platform_routes_auth_test.rs`, with either authenticator absent). Gateway plane enforcement, including refusal of platform requests without an internal authenticator, is covered by `api-gateway/tests/auth_middleware.rs` using test routes.

## Contents

- [Plane separation](#plane-separation)
  - [TR-PLANE-001 — A platform route serves the internal token and refuses a bearer](#tr-plane-001--a-platform-route-serves-the-internal-token-and-refuses-a-bearer)
  - [TR-PLANE-002 — Both credentials on one request are refused on either route set](#tr-plane-002--both-credentials-on-one-request-are-refused-on-either-route-set)
  - [TR-PLANE-003 — A tenant reads with its bearer and cannot mutate](#tr-plane-003--a-tenant-reads-with-its-bearer-and-cannot-mutate)
  - [TR-PLANE-004 — The full auth matrix on every route](#tr-plane-004--the-full-auth-matrix-on-every-route)

## Plane separation

### TR-PLANE-001 — A platform route serves the internal token and refuses a bearer

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** nothing is registered under the test's namespace.

**When:** dry-run `person_schema` through `POST /types-registry/v2/platform/entities`, once with no credential, once with the tenant bearer alone, once with a forged internal token and once with the configured internal token.

**Then:** the first three are `401` with reasons `MISSING_INTERNAL_TOKEN`, `MISSING_INTERNAL_TOKEN` (the bearer is dropped unvalidated) and `INTERNAL_AUTH_FAILED`; the last is a `202` receipt.

### TR-PLANE-002 — Both credentials on one request are refused on either route set

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** `person_schema` is registered.

**When:** read it through `GET /types-registry/v2/platform/entities/{gts_id}` and through `GET /types-registry/v2/entities/{gts_id}`, each request carrying both the tenant bearer and the internal token.

**Then:** both are `401` with reason `MIXED_CREDENTIALS`, refused before either credential is validated.

### TR-PLANE-003 — A tenant reads with its bearer and cannot mutate

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** `person_schema` is registered through the platform API.

**When:** the tenant reads it through `GET /types-registry/v2/entities/{gts_id}` with its bearer, then tries the same read with the internal token alone, then tries to register through `POST /types-registry/v2/entities` with its bearer.

**Then:** the bearer read is `200` with the entity and equals the platform read; the token-only tenant read is `401`; the tenant registration is not served (`404` or `405`), and the entity still has `resource_version` 1.

### TR-PLANE-004 — The full auth matrix on every route

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** `person_schema` is registered.

**When:** every route of both sets — the seven platform routes (dry-run submit and deletions, the operation read, the three reads) and the three tenant reads — is called with no credential, the valid credential of each plane, a forged credential of each plane, and both credentials together in all four combinations of validity.

**Then:** a platform route serves only the valid internal token; with none, or a bearer alone, it is `401` `MISSING_INTERNAL_TOKEN`, and with a forged token `INTERNAL_AUTH_FAILED`. A tenant read serves only the valid bearer; with none, or a token alone, it is `401` `MISSING_BEARER`, and with a forged bearer `AUTHN_FAILED`. Both credentials together are `401` `MIXED_CREDENTIALS` on every route, whatever their validity.
