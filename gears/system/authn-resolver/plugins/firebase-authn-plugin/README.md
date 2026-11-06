# Firebase AuthN plugin

Verifies Firebase ID tokens using Google's securetoken JWK endpoint, RS256,
project audience/issuer, expiry and configured UID plus verified-email mappings.
Tenant and scopes come from deployment configuration, never from browser input.
Any number of mappings is supported; no project or account is built in.

The runtime publishes the configured plugin instance through
`PlatformTypesRegistryApi` before application initialization. The instance keeps
the fixed `cf.builtin.firebase_authn.plugin.v1` suffix and its configured vendor
and priority; identity mappings stay private to this plugin. After admission,
`init` binds its scoped client from the admitted manifest. A refused publication
prevents binding. The registry can be embedded with a database or accessed through
the SDK's resolving REST client; the plugin has no runtime dependency on its
implementation crate.
