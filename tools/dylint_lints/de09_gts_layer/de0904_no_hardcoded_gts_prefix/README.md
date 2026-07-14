# DE0904: No Hard-Coded GTS Prefix

Rejects Rust source that writes a GTS identifier with the `gts.` prefix.
Use `gts_id!("<suffix>")` so the final prefix comes from the consuming
crate's `GTS_ID_PREFIX` configuration.

The lint runs before macro expansion. This deliberately limits it to
user-authored source and makes it compatible with ToolKit wrappers such as
`gts_type_schema`, `gts_instance`, and `resource_error`, which generate
additional GTS values internally.

```rust,ignore
// Wrong
const TYPE_ID: &str = "gts.cf.core.users.user.v1~";

// Correct
const TYPE_ID: &str = gts_id!("cf.core.users.user.v1~");
```

The UI suite also exercises real `toolkit_gts::gts_type_schema`,
`gts_instance!`, `gts_instance_raw!`, and
`toolkit_canonical_errors::resource_error` invocations in both forms:
suffixes wrapped with `gts_id!` are accepted, while a user-written `gts.`
prefix is rejected.
