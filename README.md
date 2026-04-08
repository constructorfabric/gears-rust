# Cyber Fabric
![Badge](./.github/badgeHN.svg)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/cyberfabric/cyberfabric-core/badge)](https://scorecard.dev/viewer/?uri=github.com/cyberfabric/cyberfabric-core)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/12050/badge)](https://www.bestpractices.dev/projects/12050)

**Cyber Fabric** is a modular, high-performance platform for building modern enterprise-grade SaaS services in Rust. It provides a comprehensive framework for building scalable AI-powered applications with automatic REST API generation, comprehensive OpenAPI documentation, and a extremely flexible modular architecture.

**Key Philosophy:**
- **Modular by Design**: Everything is a Module - composable, independent units with gateway patterns for pluggable workers
- **Extensible at Every Level**: [GTS](https://github.com/globaltypesystem/gts-spec)-powered extension points for custom data types, business logic, and third-party integrations
- **SaaS Ready**: Multi-tenancy, granular access control, usage tracking, and tenant customization built-in
- **Cloud Operations Excellence**: Production-grade observability, database agnostic design, API best practices, and resilience patterns via ModKit
- **Spec-Driven Development**: [Industry-standard specification templates](docs/spec-templates/README.md) (PRD, Design, ADR, Feature, Upstream Reqs) define what gets built *before* code is written, ensuring traceability from requirements to implementation
- **Shift Left**: Catch issues at the earliest possible stage — custom [dylint](dylint_lints/) architectural lints enforce design rules at compile time, Clippy with denied warnings, integrated [E2E tests](#e2e-tests), fuzzing, and security audits run in CI before code reaches review
- **Quality First**: 90%+ test coverage target with unit, integration, E2E, performance, and security testing
- **Universal Deployment**: Single codebase runs on cloud, on-prem Windows/Linux workstation, or mobile
- **Developer Friendly**: AI-assisted code generation, automatic OpenAPI docs, DDD-light structure, and type-safe APIs
- **Written in Rust**: Optimize recurring engineering work with compile-time safety and deep static analysis (including project-specific lints) so more issues are prevented before review/runtime.
- **Keep Monorepo while possible**: Keep core modules and contracts in one place to enable atomic refactors, consistent tooling/CI, and realistic local build + end-to-end testing; split only when scale forces it.

See the full architecture [MANIFEST](docs/ARCHITECTURE_MANIFEST.md) for more details, including rationales behind Rust and Monorepo choice.

See also [REPO_PLAYBOOK](docs/REPO_PLAYBOOK.md) with the registry of repository-wide artifacts (guidelines, rules, conventions, etc).

## Quick Start

### Prerequisites

- Rust stable with Cargo ([Install via rustup](https://rustup.rs/))
- Protocol Buffers compiler (`protoc`):
  - macOS: `brew install protobuf`
  - Linux: `apt-get install protobuf-compiler`
  - Windows: Download from https://github.com/protocolbuffers/protobuf/releases
- MariaDB/PostgreSQL/SQLite or in-memory database

### CI/Development Commands

```bash
# Clone the repository
git clone --recurse-submodules <repository-url>
cd cyberfabric-core

make ci         # Run full CI pipeline
make fmt        # Check formatting (no changes). Use 'make dev-fmt' to auto-format
make clippy     # Lint (deny warnings). Use 'make dev-clippy' to attempt auto-fix
make test       # Run tests
make example    # Run modkit example module
make check      # Full check suite
make safety     # Extended safety checks (includes dylint/kani)
make deny       # License and dependency checks
```

### Running the Server

```bash
# Quick helper
make quickstart

# Option 1: Run with SQLite database (recommended for development)
cargo run --bin hyperspot-server -- --config config/quickstart.yaml run

# Option 2: Run without database (no-db mode)
cargo run --bin hyperspot-server -- --config config/no-db.yaml run

# Option 3: Run with mock in-memory database for testing
cargo run --bin hyperspot-server -- --config config/quickstart.yaml --mock run

# Check if server is ready (detailed JSON response)
curl http://127.0.0.1:8087/health

# Kubernetes-style liveness probe (simple "ok" response)
curl http://127.0.0.1:8087/healthz

# See API documentation:
# $ make quickstart
# visit: http://127.0.0.1:8087/docs
```

### Example Configuration (config/quickstart.yaml)

```yaml
# Cyber Fabric Configuration

# Core server configuration (global section)
server:
  home_dir: "~/.hyperspot"

# Database configuration (global section)
database:
  url: "sqlite://database/database.db"
  max_conns: 10
  busy_timeout_ms: 5000

# Logging configuration (global section)
logging:
  default:
    console_level: info
    file: "logs/hyperspot.log"
    file_level: warn
    max_age_days: 28
    max_backups: 3
    max_size_mb: 1000

# Per-module configurations moved under modules section
modules:
  api_gateway:
    bind_addr: "127.0.0.1:8087"
    enable_docs: true
    cors_enabled: false
```

### Creating Your First Module

See [MODKIT UNIFIED SYSTEM](docs/modkit_unified_system/README.md) and [MODKIT_PLUGINS.md](docs/MODKIT_PLUGINS.md) for details.

## Documentation

- **[Architecture manifest](docs/ARCHITECTURE_MANIFEST.md)** - High-level overview of the architecture
- **[Modules](docs/MODULES.md)** - List of all modules and their roles
- **[MODKIT UNIFIED SYSTEM](docs/modkit_unified_system/README.md) and [MODKIT_PLUGINS.md](docs/MODKIT_PLUGINS.md)** - how to add new modules.
- **[Contributing](CONTRIBUTING.md)** - Development workflow and coding standards

## Security

Cyber Fabric applies defense-in-depth security across the entire development lifecycle — from Rust's compile-time safety guarantees and custom architectural lints, through compile-time tenant isolation and PDP/PEP authorization enforcement, to continuous fuzzing, dependency auditing, and automated security scanning in CI.

See **[Security Overview](docs/security/SECURITY.md)** for the full breakdown, including: Secure ORM with compile-time tenant scoping, authentication/authorization architecture (NIST SP 800-162 PDP/PEP model), 90+ Clippy deny-level rules, custom dylint architectural lints, cargo-deny advisory checks, ClusterFuzzLite continuous fuzzing, CodeQL/Scorecard/Snyk/Aikido scanners, and AI-powered PR review bots.

## Specification Templates

Cyber Fabric uses industry-standard specification templates (IEEE, ISO, MADR) to drive development. Specs are written *before* implementation and live alongside the code in version control.

- **[Overview & Guide](docs/spec-templates/README.md)** — Template system overview, governance, FDD ID conventions, and document placement rules
- **[PRD.md](docs/spec-templates/PRD.md)** — Product Requirements Document: vision, actors, capabilities, use cases, FR/NFR
- **[DESIGN.md](docs/spec-templates/DESIGN.md)** — Technical Design: architecture, principles, constraints, domain model, API contracts
- **[ADR.md](docs/spec-templates/ADR.md)** — Architecture Decision Record: decisions, options, trade-offs, consequences
- **[FEATURE.md](docs/spec-templates/FEATURE.md)** — Feature Specification: flows, algorithms, states, requirements
- **[UPSTREAM_REQS.md](docs/spec-templates/UPSTREAM_REQS.md)** — Upstream Requirements: technical requirements from other modules to this module

## Configuration

### YAML Configuration Structure

```yaml
# config/server.yaml

# Global server configuration
server:
  home_dir: "~/.hyperspot"

# Database configuration
database:
  servers:
    sqlite_users:
      params:
        WAL: "true"
        synchronous: "NORMAL"
        busy_timeout: "5000"
      pool:
        max_conns: 5
        acquire_timeout: "30s"

# Logging configuration
logging:
  default:
    console_level: info
    file: "logs/hyperspot.log"
    file_level: warn
    max_age_days: 28
    max_backups: 3
    max_size_mb: 1000

# Per-module configuration
modules:
  api_gateway:
    config:
      bind_addr: "127.0.0.1:8087"
      enable_docs: true
      cors_enabled: true
  users_info:
    database:
      server: "sqlite_users"
      file: "users_info.db"
    config:
      default_page_size: 5
      max_page_size: 100
```

### Environment Variable Overrides

Configuration supports environment variable overrides with `HYPERSPOT_` prefix:

```bash
export HYPERSPOT_DATABASE_URL="postgres://user:pass@localhost/db"
export HYPERSPOT_MODULES_api_gateway_BIND_ADDR="0.0.0.0:8080"
export HYPERSPOT_LOGGING_DEFAULT_CONSOLE_LEVEL="debug"
```

## Testing

```bash
make check           # full quality gate (fmt + clippy + test + security)
```

Other tests:

```bash
make test            # unit tests (workspace)
make test-sqlite     # integration tests (SQLite, no external DB required)
make e2e-local       # end-to-end tests (builds + starts server automatically)
make e2e-docker      # end-to-end tests (builds + starts server in Docker)
make coverage-unit   # unit test code coverage
make fuzz            # fuzz smoke tests (30 s per target)
```

On **Windows** (no `make`), use the cross-platform CI script directly:

```bash
python scripts/ci.py check          # full CI suite
python scripts/ci.py e2e-local      # end-to-end tests
python scripts/ci.py fuzz --seconds 60  # fuzz smoke run
```

For the complete test strategy, coverage policy, CI pipeline details, and all
available commands see **[docs/TESTING.md](docs/TESTING.md)**.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines.

## License

This project is licensed under the Apache 2.0 License - see the [LICENSE](LICENSE) file for details.
