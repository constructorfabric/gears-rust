# Design Review Prompt

Review the following PR changes against design and architecture best practices.

Focus on:
- Alignment with existing architecture (see `{architecture}`)
- Trade-off analysis
- API contract consistency
- Security considerations
- Compliance with `{template}` template
- Find antipatterns
- Compare proposed design with existing industry patterns
- Compare proposed design with the best IEEE, ISO, and other industry standards
- Criticize the design
- Split design review by topics and rate every 1-10

Use `{checklist}` as the structured review guide.
Pay attention to the "PR Review Focus (Design)" section at the end of the checklist.
Refer to `{gts_guidelines}` when the design introduces or changes GTS identifiers, type schemas, well-known instances, discriminator/const-enum-like values, `x-gts-traits` / `x-gts-traits-schema`, type registries, type-driven authorization, or plugin/extension-point contracts that should be modeled with GTS.
