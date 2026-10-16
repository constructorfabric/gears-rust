# Deploy

Building and shipping CF/Gears as docker images and Helm charts:

## Layout

```
deploy/
  components.yaml             # release manifest — what gets built into images/charts
  docker/                     # dockerfiles (flight-control + the shared gear image)
  helm/
    toolkit-common/           # library chart (shared templates, renders nothing itself)
    toolkit-platform/         # umbrella chart (which gears + environment)

<gear-path>/deploy/helm/      # individual gear charts

tools/scripts/
  update-helm-deps.sh         # vendor toolkit-common into every chart
  resolve_release.py          # map a release to its image/chart build outputs

.github/workflows/
  publish-artifacts.yml       # the image + chart publish pipeline
```

## Helm charts

| Chart | Type | Role |
|---|---|---|
| `deploy/helm/toolkit-common/` | library | Shared named templates (`deployment`, `service`, `hpa`, …). Renders nothing itself; not installable. |
| `deploy/helm/toolkit-platform/` | umbrella | Conditional deps on gear charts + `values*.yaml` presets. Installs the whole platform. |
| `<gear-path>/deploy/helm/<gear>` | application | A gear's `values.yaml` + thin wrappers that `include` the library. Installable standalone. |


```
toolkit-platform (umbrella)
  ├─ flight-control ─┐   (enabled by default)
  ├─ <gear-a> ───────┤──> toolkit-common (library, via file://)
  └─ <gear-n> ───────┘
```

Each gear chart vendors `toolkit-common` locally and carries the full value set — the
library holds the logic, the gear supplies the data.

### How templates see values

A gear's wrapper is just:

```yaml
# <gear>/templates/deployment.yaml
{{- include "toolkit-common.deployment" . }}
```

Rendered, `.` is the gear subchart's context, so inside the library template `.Values`
is that gear's merged values. That's why every gear's `values.yaml` carries the full set.

### Value precedence (lowest → highest)

For a gear installed **via the umbrella**, layers deep-merge:

1. the gear chart's own `values.yaml` (defaults),
2. the umbrella's `values.yaml`, under the gear's key (`<gear>:` block),
3. the umbrella preset passed with `-f` (e.g. `values-dev.yaml`),
4. `--set` on the command line.

### The `global:` key

Anything under `global:` in the umbrella values reaches **every** subchart as
`.Values.global` — set once, apply to all gears:

```yaml
# toolkit-platform/values.yaml
global:
  imageRegistry: ""              # image prefix for every gear
  opentelemetry:
    enabled: false
    endpoint: "http://otel-collector:4317"
```

### Capability templates (opt-in)

`toolkit-common` also defines optional resources — Secrets, HPA, PDB, NetworkPolicy,
Ingress — rendered only when a gear opts in with a one-line wrapper plus matching values:

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

### Common commands

```bash
# Vendor toolkit-common into every chart (auto-discovers; no list to maintain).
bash tools/scripts/update-helm-deps.sh

# Lint / render the platform with the dev preset.
helm lint deploy/helm/toolkit-platform
helm template demo deploy/helm/toolkit-platform -f deploy/helm/toolkit-platform/values-dev.yaml
```

Notes:
- `Chart.lock` / `charts/*.tgz` are build outputs (gitignored); regenerate with
`update-helm-deps.sh`.
- `config.data` is rendered through Helm `tpl`, so literal`{{`/`}}` in config content must be escaped.

## Releasing

Images and charts publish to `ghcr.io/constructorfabric` via
[`publish-artifacts.yml`](../.github/workflows/publish-artifacts.yml). Two independent
kinds of release:

- **A gear** — its image + chart, published automatically by release-plz.
- **The platform** — the umbrella, pinned to chosen gear versions, published manually.

### Gears (automatic)

When a release PR merges, [`release-plz.yml`](../.github/workflows/release-plz.yml)
releases each gear and, in the **same run**, publishes its image + chart: a
`plan-publish` job turns release-plz's `releases` output into a matrix (via
[`resolve_release.py`](../tools/scripts/resolve_release.py) +
[`components.yaml`](./components.yaml)), and a `publish` matrix runs the pipeline per gear.

> It keys off release-plz's **output**, not git tags — tags made with `GITHUB_TOKEN`
> can't trigger a workflow, and pushing >3 at once is dropped by GitHub.

How a gear reaches that output:

- **Regular gears** (`publish = true`) release to crates.io normally.
- **flight-control and the examples** (`publish = false`) use a `git_only` entry in
  [`release-plz.toml`](../release-plz.toml) so release-plz tags them without crates.io.

Each gear gets an **image** (`:<version>` + `:latest`, with OCI labels, SBOM, SLSA
provenance, and a keyless cosign signature) and a **chart** (packaged at that version,
`image.tag` stamped to match, cosign-signed by digest).

### Platform (manual)

The umbrella's bill-of-materials is one `image.tag` per gear in
[`toolkit-platform/values.yaml`](./helm/toolkit-platform/values.yaml). Point each at an
already-released gear version, then **Actions → Publish Images & Charts → Run workflow**
with component `toolkit-platform` and the new version:

```yaml
# toolkit-platform/values.yaml
my-gear:
  image:
    tag: "1.4.0"      # must already be released
```

Operators install or upgrade with:

```bash
helm upgrade --install my-platform \
  oci://ghcr.io/constructorfabric/charts/toolkit-platform --version 0.5.0
```

The same **Run workflow** (any `component` + `version`, optional `latest`) re-publishes
a gear by hand. `toolkit-common` is vendored via `file://`, so it is never published on
its own.

### Adding a gear

One entry in [`components.yaml`](./components.yaml) — no workflow edit:

```yaml
components:
  my-gear:                               # → ghcr.io/constructorfabric/my-gear (image + chart)
    crate: cf-gears-my-gear              # its release drives publishing
    chart: gears/my-gear/deploy/helm/my-gear
    image:
      dockerfile: deploy/docker/oop-gear.Dockerfile   # OoP gears share this one
      buildArgs:
        - GEAR_PACKAGE=cf-gears-my-gear
        - GEAR_BIN=my-gear-oop
        - GEAR_FEATURES=oop_module k8s-auth otel
        - GEAR_CONFIG=config/oop-my-gear.yaml
        - GEAR_PORT=9091
```

Then, in the new gear chart's `values.yaml`, set `image.tag` to the `"0.0.0"`
placeholder — the pipeline stamps it with the real version at publish time, but only if
it is there to stamp:

```yaml
# <gear-path>/deploy/helm/<gear>/values.yaml
image:
  tag: "0.0.0"   # placeholder; the release pipeline stamps the real version
```

### Verifying signatures

Signatures are keyless, so verify against the signer identity + OIDC issuer (no key to
distribute):

```bash
# Image: ghcr.io/constructorfabric/<component>:<version>
cosign verify ghcr.io/constructorfabric/hello:0.2.0 \
  --certificate-identity-regexp '^https://github.com/constructorfabric/gears-rust/\.github/workflows/publish-artifacts\.yml@' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Charts verify the same way, but live under the `charts/` path
(`ghcr.io/constructorfabric/charts/<component>`):

```bash
cosign verify ghcr.io/constructorfabric/charts/hello:0.2.0 \
  --certificate-identity-regexp '^https://github.com/constructorfabric/gears-rust/\.github/workflows/publish-artifacts\.yml@' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Inspect an image's SBOM and provenance with `docker buildx imagetools inspect <image>`.

### Troubleshooting

- **A gear released but nothing published** — check the `plan-publish` log for
  `will publish: …`; a gear missing from `components.yaml` (or not `git_only` when
  `publish = false`) won't appear.
- **Dry-run the resolver locally** — the default mode resolves one component (a gear tag,
  or explicit inputs for a manual/dispatch publish):
  ```bash
  GITHUB_REF_NAME=hello-v0.2.0 python3 tools/scripts/resolve_release.py          # a gear tag
  INPUT_COMPONENT=toolkit-platform INPUT_VERSION=0.5.0 python3 tools/scripts/resolve_release.py
  ```
- **Dry-run the publish matrix** — `plan` mode maps release-plz's `releases` output to the
  `{component, version}` matrix the automatic path publishes (non-publishable crates drop
  out):
  ```bash
  RELEASES='[{"package_name":"hello","tag":"hello-v0.2.0","version":"0.2.0"}]' \
    python3 tools/scripts/resolve_release.py plan
  ```
