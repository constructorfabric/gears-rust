# Settings Demo Gear

A demo contributor for the Settings Service. At init it resolves
`SettingsContributionClient` from `ClientHub` and registers a sample catalogue
of sixteen declarations — one per value type the Settings Service SDK ships, plus a
second of a few types —
so a running example server has settings to browse, read and set.

It is example code, enabled in the example server by the `settings-demo`
feature. It ships no REST surface and no storage of its own; everything it
contributes lives in the Settings Service.

## What it contributes

Keys are `gts.cf.core.settings.setting_type.v1~cf.settings_demo.<category>.<name>.v1~`
across four categories, created on first boot: `network`, `notifications`,
`limits`, `security`. Re-running the server converges the same set and changes
nothing; editing a declaration's description here updates it in place, while a
change of value type, Schema Default or scope class is refused and logged — those
are a new major, not an edit.
