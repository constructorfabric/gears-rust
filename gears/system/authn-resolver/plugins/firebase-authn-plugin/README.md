# Firebase AuthN plugin

Verifies Firebase ID tokens using Google's securetoken JWK endpoint, RS256,
project audience/issuer, expiry and configured UID plus verified-email mappings.
Tenant and scopes come from deployment configuration, never from browser input.
Any number of mappings is supported; no project or account is built in.
