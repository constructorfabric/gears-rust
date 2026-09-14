# github-mirror SDK

Public in-process API for the `github-mirror` gear (`cpt-cf-github-mirror-interface-sdk`):
the `GithubMirrorClientV1` trait, its transport-agnostic models, and canonical error
semantics. Consumers resolve the client from `ClientHub`:

```rust,ignore
let mirror = ctx.client_hub().get::<dyn github_mirror_sdk::GithubMirrorClientV1>()?;
let status = mirror.status(&ctx_sec).await?;
```

The trait is `unstable (pre-1.0)`: it starts with the operations the gear can serve
today (`status`) plus the first read-slice contract (`list_repositories`, backed by
the mirrored store once the storage port lands), and grows method-by-method as
sync, entity retrieval, and write-back are ported from the `github-repotap`
prototype.
