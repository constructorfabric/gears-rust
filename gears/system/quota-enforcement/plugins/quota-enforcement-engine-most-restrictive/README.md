# Most-restrictive-wins engine

Statically linked Quota Enforcement engine. Selects one satisfiable quota by subject tier (any subject before the tenant), boundedness, remaining capacity and quota ID. Does not read the metadata of the request. It does not read Quota metadata: Quota Enforcement applies a Quota's conditions before the engine runs, so the engine sees only the Quotas that apply.
