# Gearbox

> Written 2026-09-14. The roadmap section below dates quickly; the rest does not.

You have started seeing `gear.gdl` files beside gear crates. This explains what reads them,
why they exist, and what is being asked of gear authors.

## What Gearbox does

Gearbox composes a deployable product out of gears. Given a set of gears and an operator's
intent, it:

1. **reads the catalogue**: every `gear.gdl` under the source roots, joined with what it
   projects out of the Rust attributes;
2. **resolves a product** for one deployment profile: which gears are in, which applications
   they land in, which contract bindings are local and which cross an application boundary;
3. **writes a lock**, `product.lock`, the resolution as a byte-stable document;
4. **generates the application crates**: `main.rs`, `Cargo.toml`, the registration list, the
   config files.

It decides. Nothing in a `gear.gdl` says *where* a gear runs or *what it binds to*; those are
outputs of resolution, not inputs. A descriptor states facts about one gear; the resolver puts
them together.

## Why the files exist

The Rust attributes already carry most of what Gearbox needs. `#[toolkit::gear]` gives it the
id, the runtime capabilities, the link-time dependencies and the lifecycle. Gearbox reads
those directly out of the source.

What it cannot read out of Rust is everything an operator needs to *choose* a gear: a display
name, a description, a category, whether the gear is internal or offered, which Cargo features
are worth putting in front of a human, and which contracts it intends to provide or consume as
a matter of design rather than of linkage. That is what a `gear.gdl` holds.

**One fact, one author.** A descriptor may not restate what the attributes already say.
Six fields are refused outright: `id`, `runtime_caps`, `colocated_deps`, `lifecycle`,
`client` and `cluster_providers`. Restating one is diagnostic `GBX0210`, which names the
attribute that owns it. Each descriptor's header comment records what its own crate projects.
From `gears/system/api-gateway/gear.gdl`:

```
# Under ADR cpt-gearbox-adr-macro-projected-catalogue the Rust attributes keep
# every fact they already express. Projected from #[toolkit::gear] in
# src/gear.rs, and therefore NOT written here:
#
#   id             <- name = "api-gateway"
#   runtime_caps   <- capabilities = [rest_host, rest, stateful]
#   colocated_deps <- deps = [grpc_hub, authn_resolver]   (link-time; uncuttable)
#   lifecycle      <- lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
#
# Declaring any of them here is GBX0210, which names the owning attribute.
```

## Four levels, and which one was renamed

Gearbox names four things. Only one of them changed name recently, and the change is
recent enough that both words are still in circulation.

| Level | What it is | Note |
|---|---|---|
| **gear** | one unit of code | a crate with `#[toolkit::gear]`, described by one `gear.gdl` |
| **application** | one binary, one Deployment, N replicas | the co-location closure of an anchor gear |
| **product** | a composition one team owns and ships | one `product.gdl`, one `product.lock`, one generated tree |
| **installation** | the running whole at one site | several products deploy into one |

**`process` became `application`.** The word is not banned: it still means an operating-system
process, and an application with three replicas is three of them. What it no longer means is the
deployable unit.

**`product` was not renamed**, and `product.gdl` keeps its name. The word is one level too high
for a shared cluster and that is known, but `product.gdl` and `product.lock` are on-disk names in
every description, document and test, so changing it is a migration rather than a rename. It
waits.

**`installation` is new and unbuilt.** It names a level that previously had no word at all. See
"Where this stands" below.

## What one looks like

The smallest descriptor in this repository, `gears/system/gear-orchestrator/gear.gdl`,
complete:

```python
# Gearbox product metadata for the gear-orchestrator gear.
#
# Projected from #[toolkit::gear]: id, runtime_caps (grpc, system, rest), and
# `client = cf_system_sdks::directory::DirectoryClient` -- the only gear in the
# slice declaring a client trait.

gear(
    name = "Gear Orchestrator",
    description = "DirectoryService server: instance registration, heartbeat, endpoint resolution.",
    category = "core-functionality",
    visibility = "internal",

    package = cargo(
        crate_name = "cf-gears-gear-orchestrator",
        lib = "gear_orchestrator",
        path = ".",
    ),
)
```

That is the whole file. Larger gears add contract declarations, endpoints, cluster
requirements and curated features. See [gdl.md](gdl.md) for the full vocabulary.

## Cargo features are curated, not dumped

Gearbox reads a crate's `[features]` table and, without curation, offers all of it to whoever
is composing a product. That means offering `integration` (wants a Docker daemon),
`e2e-diagnostics` (a test switch) and the literal `default`, which is not a choice anyone
makes.

`cargo_features` is where a gear author says which features belong in front of an integrator,
and where each one belongs. From `gears/system/api-gateway/gear.gdl`:

```python
    cargo_features = [
        feature("grpc"),
        feature("otel"),
        feature("embed_elements"),
        feature("k8s-auth", kinds = ["kubernetes"]),
    ],
```

`kinds` is the second judgement `Cargo.toml` cannot hold: `k8s-auth` reads a service-account
token from a path that exists only inside a pod, so it is what a Kubernetes deployment needs
and what a local one must not have. A product selecting it for a local profile is refused with
`GBX0316` rather than building a binary that compiles and then fails at startup.

The list is checked against the manifest it curates, so it cannot quietly drift from the
table: a name the crate does not declare is `GBX0213`.

**Absent and empty mean different things.** No `cargo_features` at all means nobody has
curated this gear, and the tool falls back to the whole table. `cargo_features = []` means
there is nothing here worth offering, which is the true answer for a crate whose only feature
gates its own tests.

## Where this stands

Fourteen crates carry a descriptor today, out of 46 that carry `#[toolkit::gear]`. Sixteen of
the thirty-two without one are system gears. The fourteen cover the slice Gearbox is currently
demonstrated on; the rest are invisible to composition until someone writes theirs.

Seven of the fourteen curate features; the other seven declare none and need none.

On the Gearbox side, the vertical slice works end to end: catalogue load, resolution across the
three deployment profiles, lock, generation of the application crates, and a Theia-based Studio
for browsing and composing, with a chat that answers from the resolver's own output. The engine
speaks a typed JSON-RPC API.

What is not built: registry-sourced gears (sources are local paths today), semantic-version
resolution of gear versions, roles and shards as resolved concepts, which are parsed and
refused with a diagnostic citing the runtime source that proves the gap; and cluster
providers beyond those registered in the runtime today.

There is also no migration tooling, deliberately: nothing will draft the declared half of a
`gear.gdl` for a gear that already exists. The thirty-two are written by hand.

The direction, and it is proposed rather than built: today a description generates the whole
closure, including gears another product already deployed. The intent is that it generates its
own part and **references** an installation it joins, keyed by that product's `product.lock` -
the artefact already exists, is canonical and records exactly what was resolved, so nothing new
has to be described, only pointed at. The first product builds the installation and later ones
join it. `Installation` exists in no type, no GDL construct and no lock field today.

## What is asked of gear authors

- **A new gear needs a descriptor.** Without one it is invisible to composition. The
  neighbouring files are the template; the smallest is 18 lines.
- **Curate the features that matter.** If your crate has features an integrator should choose
  between, list them. If it has none, write `cargo_features = []` and say so explicitly.
- **Do not restate what the attribute says.** If you find yourself typing an id or a
  capability list, the descriptor is the wrong place; the attribute already has it.
- **Diagnostics name their own remedy.** A `GBX…` code from a catalogue load points at the
  file, the line, and what to do. They are catalogued with the engine.

Questions about the language go to [gdl.md](gdl.md). Questions about what the resolver does
with a descriptor go to whoever is carrying Gearbox.
