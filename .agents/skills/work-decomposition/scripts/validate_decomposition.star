# Structural gate for a work-decomposition manifest.
#
# Checks (in order):
#   1. unique brief ids
#   2. depends_on references resolve (no dangling id)
#   3. dependency graph is acyclic (Kahn's topological sort)
#   4. every brief has non-empty acceptance criteria + a test_plan
#
# NOTE: JSON-schema shape validation and scope-path filesystem checks are
# handled by the Python companion (validate_decomposition.py), which requires
# jsonschema and filesystem access.  This script covers the pure-logic gates
# that run in host.starlark.run without any dependencies.

def main(ctx):
    manifest = ctx.inputs["manifest"]
    errors = []

    briefs = manifest.get("briefs") or []
    if type(briefs) != "list":
        return {"ok": False, "errors": ["manifest.briefs must be a list"]}

    # Build the id list and set.
    ids = []
    for b in briefs:
        bid = b.get("id") if b else None
        if bid:
            ids.append(bid)

    # 1. unique ids
    seen = {}
    for bid in ids:
        if bid in seen:
            errors.append("duplicate brief id: " + repr(bid))
        seen[bid] = True
    idset = seen  # used as a set: {id: True}

    # 2. dangling depends_on
    for b in briefs:
        bid = b.get("id") or "?"
        for dep in (b.get("depends_on") or []):
            if dep not in idset:
                errors.append("brief " + repr(bid) + " depends_on unknown id " + repr(dep))

    # 3. acyclic — Kahn's algorithm
    indeg = {}
    adj = {}
    for bid in idset:
        indeg[bid] = 0
        adj[bid] = []
    for b in briefs:
        bid = b.get("id")
        if bid not in idset:
            continue
        for dep in (b.get("depends_on") or []):
            if dep in idset:
                adj[dep].append(bid)
                indeg[bid] = indeg[bid] + 1

    queue = []
    for bid in indeg:
        if indeg[bid] == 0:
            queue.append(bid)

    visited = 0
    for _ in range(len(ids)):
        if not queue:
            break
        n = queue.pop()
        visited = visited + 1
        for m in adj[n]:
            indeg[m] = indeg[m] - 1
            if indeg[m] == 0:
                queue.append(m)

    if visited < len(ids):
        stuck = sorted([bid for bid in indeg if indeg[bid] > 0])
        errors.append("dependency cycle among briefs: " + ", ".join(stuck))

    # 4. acceptance + test_plan
    for b in briefs:
        bid = b.get("id") or "?"
        if not (b.get("acceptance") or []):
            errors.append("brief " + repr(bid) + " has no acceptance criteria")
        if not (b.get("test_plan") or "").strip():
            errors.append("brief " + repr(bid) + " has no test_plan")

    return {
        "ok": len(errors) == 0,
        "errors": errors,
    }
