# Helm Charts

How the CF/Gears Helm charts fit together, and how values flow.

## The pieces

| Piece | Type | Owns | Installable? |
|---|---|---|---|
| `toolkit-common/` | library | **How** resources look — shared named templates (`toolkit-common.deployment`, `.service`, `.secrets`, `.hpa`, …). Renders nothing by itself. | No |
| `<gear-path>/deploy/helm/<gear>` | application | **What** a gear is — its `values.yaml` (image, ports, `config.data`, probes, resources) + thin wrapper templates that `include` the library. | Yes (standalone) |
| `toolkit-platform/` | application (umbrella) | **Which** gears + environment — conditional dependencies on gear charts plus `values*.yaml` presets. No templates of its own. | Yes (whole platform) |

## Dependency graph

```
toolkit-platform (umbrella)
  ├─ flight-control ─┐   (fixed control-plane chart)
  ├─ <gear-a> ───────┤──> toolkit-common (library)
  ├─ <gear-b> ───────┤
  └─ <gear-n> ───────┘
```

- Each gear chart depends on `toolkit-common` via a relative `file://` path.
- The umbrella depends on each gear chart with `condition: <gear>.enabled`.
- `toolkit-common` is a leaf: it contributes templates only, never values.

Concrete charts exist under `<gear-path>/deploy/helm/<gear>`

## How templates see values

A gear's wrapper is just:

```yaml
# <gear>/templates/deployment.yaml
{{- include "toolkit-common.deployment" . }}
```

When rendered, `.` is the gear subchart's context, so inside the library
template `.Values` is that gear's merged values. The library defines the logic;
each gear supplies the data — which is why every gear's `values.yaml` carries the
full set (`image`, `service.port`, `probes`, …).

## Value precedence (lowest → highest)

For a gear installed **via the umbrella**:

1. The gear chart's own `values.yaml` (defaults).
2. The umbrella's `values.yaml`, under the gear's key (e.g. a `<gear>:` block).
3. The umbrella preset passed with `-f` (e.g. `values-dev.yaml`).
4. `--set` on the command line.

Each later layer deep-merges over the earlier one.

### Example — a gear's image tag

```yaml
# 1. <gear>/values.yaml (gear default)
image: { repository: <registry>/<gear>, tag: "1.0.0" }
```
```yaml
# 2. toolkit-platform/values.yaml (keyed by subchart name)
<gear>:
  image: { tag: "1.1.0" }
```
```bash
# 4. install-time override wins
helm install demo deploy/helm/toolkit-platform --set <gear>.image.tag=1.2.3
```

Result: `<gear>` renders with tag `1.2.3`.

## The `global:` key

Anything under `global:` in the umbrella values is shared with **every** subchart
as `.Values.global` — set a value once, reach all gears:

```yaml
# toolkit-platform/values.yaml
global:
  imageRegistry: ""          # image prefix for every gear
  opentelemetry:
    enabled: false           # APP__OPENTELEMETRY__* env for every gear
    endpoint: "http://otel-collector:4317"
```

## Capability templates (opt-in)

`toolkit-common` defines optional resources — Secrets, HPA, PDB, NetworkPolicy,
Ingress. They are **not** rendered unless a gear opts in by adding a one-line
wrapper under its `templates/` and the matching values. Example:

```yaml
# <gear>/templates/hpa.yaml
{{- include "toolkit-common.hpa" . }}
```
```yaml
# <gear>/values.yaml
autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 5
```

## Common commands

```bash
# Resolve the toolkit-common dependency for every gear chart + the umbrella.
# Auto-discovers charts — no list to maintain.
bash deploy/helm/update-helm-deps.sh

# Lint the library, umbrella, and a gear chart.
helm lint deploy/helm/toolkit-common deploy/helm/toolkit-platform \
  <gear-path>/deploy/helm/<gear>

# Render the whole platform with the dev preset.
helm template demo deploy/helm/toolkit-platform \
  -f deploy/helm/toolkit-platform/values-dev.yaml

# Install a single gear standalone (directoryEndpoint points at the control plane).
helm install <gear> <gear-path>/deploy/helm/<gear> \
  --set directoryEndpoint=dns:///flight-control.<namespace>.svc:50051
```

## Notes

- Chart artifacts (`Chart.lock`, `charts/*.tgz`) are build outputs and gitignored;
  regenerate them with `update-helm-deps.sh`.
- `config.data` in a gear's values is rendered through Helm `tpl`, so literal
  `{{`/`}}` in config content must be escaped.
- See `docs/arch/toolkit-oop/DESIGN.md` § 3.9 and ADR-0004 for the authoritative
  layout and rationale.
