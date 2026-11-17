# Hierarchical min() merge for descendant overrides

## Setup

Parent tenant upstream:

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "sustained": { "rate": 1000, "window": "minute" }
  }
}
```

Child tenant binding sets stricter limit:

```json
{
  "rate_limit": {
    "sustained": { "rate": 100, "window": "minute" }
  }
}
```

The child's limit is written before the parent switches to `enforce`: once an ancestor enforces its limit, a child's own limit is refused (`400`, `negative-9.9` Scenario D).

## Expected behavior

- Effective limit for child is `min(parent, child)`.
- After 100 requests/min, child gets `429`.
- The merged limit stays `enforce`, whichever level's value won: a tenant below the child cannot replace it with a `private` limit (see `negative-6.2` Scenario B).
