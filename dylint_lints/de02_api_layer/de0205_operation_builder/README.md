# DE0205 – Operation builder must have tag and summary

## What it does

Ensures that all `OperationBuilder` instances call both `.tag(...)` and
`.summary(...)` with properly formatted values.

- **Tags** must contain whitespace-separated words where each word starts with
  a capital letter. Tags must be string literals or references to `const`
  string items.
- **Summaries** must be non-empty string literals or const strings.

## Why is this bad?

Operation builders without tags or summaries, or with improperly formatted
tags, make it difficult to organize and categorize API endpoints in OpenAPI
documentation and UI. Proper documentation is essential for API usability.

## Example

```rust
// Bad – missing summary and incorrect tag casing
OperationBuilder::post("/users")
    .operation_id("create_user")
    .tag("simple resource registry");
```

Use instead:

```rust
// Good – properly formatted tag and summary
OperationBuilder::post("/users")
    .operation_id("create_user")
    .tag("User Management")
    .summary("Create a new user");
```
