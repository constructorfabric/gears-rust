# Code Review Prompt

Review the following PR code changes.

Focus on:
- Correctness, edge cases and error handling
- Code style and idiomatic patterns
- Performance implications
- Test coverage
- Security vulnerabilities
- Mistakes and potential misbehaviors

Use `{checklist}` as the structured review guide when available.
Refer to `{coding_guidelines}` for programming language-specific conventions.
Refer to `{security_guidelines}` for security requirements.
Refer to `{gts_guidelines}` when the PR introduces new or changed GTS identifiers, type schemas, well-known instances, discriminator/const-enum-like values, `x-gts-traits` / `x-gts-traits-schema`, type-driven authorization, or plugin/extension-point contracts that should be modeled with GTS.
