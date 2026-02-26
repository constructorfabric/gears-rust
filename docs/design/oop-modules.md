# Out-of-Process (OoP) Modules Design Document

> **Status**: Draft  
> **Last Updated**: 2026-02-23  
> **Related ADR**: [ADR-0003: Universal Lazy Layer](../adrs/modkit/0003-modkit-universal-lazy-layer.md)

## Executive Summary

Out-of-Process (OoP) modules are ModKit modules that run as separate OS processes, communicating with the host and other modules via network protocols (REST by default, gRPC opt-in). This design enables:

- **Process isolation** — Crash in one module doesn't bring down others
- **Independent scaling** — Scale modules horizontally based on load
- **Language flexibility** — Non-Rust modules can implement the same API contracts
- **Resource isolation** — Memory/CPU limits per module
- **Independent deployment** — Update modules without full system restart

This document covers architecture, deployment models, fault tolerance strategies, and implementation patterns for OoP modules.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Module Packaging and Executables](#2-module-packaging-and-executables)
3. [Deployment Models](#3-deployment-models)
   - 3.4 [Hybrid and Multi-Environment Deployments](#34-hybrid-and-multi-environment-deployments)
4. [Service Discovery and Communication](#4-service-discovery-and-communication)
5. [Fault Tolerance](#5-fault-tolerance)
6. [SDK Pattern](#6-sdk-pattern)
7. [Configuration](#7-configuration)
8. [Lifecycle Management](#8-lifecycle-management)
9. [Migration Guide](#9-migration-guide)
10. [Quick Reference](#10-quick-reference)

---

## 1. Architecture Overview

### 1.1 System Topology

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              Host Process                                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌──────────────────┐    │
│  │ api-gateway │  │  Module A   │  │  Module B   │  │ DirectoryService │    │
│  │ (in-proc)   │  │ (in-proc)   │  │ (in-proc)   │  │    (gRPC hub)    │    │
│  └─────────────┘  └─────────────┘  └─────────────┘  └────────┬─────────┘    │
│         │                │                │                   │             │
│         └────────────────┴────────────────┴───────────────────┘             │
│                                   │ ClientHub                               │
└───────────────────────────────────┼─────────────────────────────────────────┘
                                    │
                    ┌───────────────┼───────────────┐
                    │               │               │
                    ▼               ▼               ▼
            ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
            │ Calculator  │ │ FileParser  │ │   LLM       │
            │   (OoP)     │ │   (OoP)     │ │  Gateway    │
            │  REST API   │ │  REST API   │ │   (OoP)     │
            └─────────────┘ └─────────────┘ └─────────────┘
```

### 1.2 Key Components

| Component | Role |
|-----------|------|
| **Host Process** | Runs in-process modules, DirectoryService, spawns OoP modules |
| **DirectoryService** | Service registry for discovery (gRPC-based) |
| **OoP Module** | Separate process with REST/gRPC API |
| **SDK Crate** | Shared contract (API trait, types, client) |
| **Lazy Client** | On-demand resolution and connection to OoP modules |

### 1.3 Communication Flow

```
Consumer Module                    DirectoryService                 OoP Module
      │                                  │                              │
      │  1. First API call               │                              │
      │─────────────────────────────────▶│                              │
      │                                  │  2. resolve_rest_service()   │
      │                                  │─────────────────────────────▶│
      │                                  │  3. Return endpoint          │
      │◀─────────────────────────────────│◀─────────────────────────────│
      │                                  │                              │
      │  4. HTTP request to cached endpoint                             │
      │────────────────────────────────────────────────────────────────▶│
      │                                  │                              │
      │  5. Response                                                    │
      │◀────────────────────────────────────────────────────────────────│
```

---

## 2. Module Packaging and Executables

### 2.1 Single Module per Executable (Recommended)

Each OoP module should be compilable as a standalone executable with its own `main.rs`:

```
modules/calculator/
├── calculator-sdk/           # Shared SDK crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── api.rs            # API trait + types
│       ├── client.rs         # Lazy REST client
│       └── descriptor.rs     # ClientDescriptor
└── calculator/               # Module implementation
    ├── Cargo.toml
    └── src/
        ├── lib.rs            # Module definition (for in-proc use)
        ├── module.rs         # Module struct + impl
        ├── main.rs           # OoP binary entry point
        └── api/
            └── rest/         # REST handlers
```

**`main.rs` for OoP module:**

```rust
use modkit::bootstrap::oop::{OopRunOptions, run_oop_with_options};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = OopRunOptions::from_env_and_args();
    run_oop_with_options(opts).await
}
```

### 2.2 Multiple Modules in Single Executable (Feature-Gated)

For scenarios where multiple related modules should be bundled together:

```toml
# Cargo.toml
[features]
default = ["calculator", "file-parser"]
calculator = []
file-parser = []
all-modules = ["calculator", "file-parser"]
```

```rust
// main.rs with feature-gated modules
use modkit::registry::ModuleRegistry;

fn main() -> anyhow::Result<()> {
    let mut registry = ModuleRegistry::new();
    
    #[cfg(feature = "calculator")]
    registry.register::<calculator::CalculatorModule>();
    
    #[cfg(feature = "file-parser")]
    registry.register::<file_parser::FileParserModule>();
    
    modkit::bootstrap::run_with_registry(registry)
}
```

**When to use multi-module executables:**
- Related modules that are always deployed together
- Reducing container image count in resource-constrained environments
- Development/testing convenience

**When to avoid:**
- Independent scaling requirements
- Different resource profiles
- Independent release cycles

### 2.3 Docker Image per Module

Every module should be compilable into a Docker image:

```dockerfile
# Dockerfile for calculator module
FROM rust:1.75-slim as builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin calculator-oop

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/calculator-oop /usr/local/bin/
EXPOSE 8080
ENTRYPOINT ["calculator-oop"]
```

**Image naming convention:**
```
{registry}/{project}/{module}-oop:{version}
# Example: ghcr.io/cyberfabric/calculator-oop:1.2.3
```

### 2.4 Building for Kubernetes: End-to-End Workflow

**"How do I compile all this for my K8s cluster and run it?"**

#### Step 1: Build Docker Images

```bash
# Build all OoP modules
for module in calculator file-parser llm-gateway; do
  docker build -t ghcr.io/cyberfabric/${module}-oop:latest \
    -f modules/${module}/Dockerfile .
done

# Or use a multi-stage build script
make docker-build-all
```

#### Step 2: Push to Registry

```bash
# Push to container registry (GitHub, ECR, GCR, etc.)
docker push ghcr.io/cyberfabric/calculator-oop:latest

# Or use CI/CD (GitHub Actions example)
# .github/workflows/build.yml handles this automatically
```

#### Step 3: Deploy to Kubernetes

```bash
# Option A: Helm chart (recommended)
helm install cyberfabric ./charts/cyberfabric \
  --set modules.calculator.enabled=true \
  --set modules.fileParser.enabled=true

# Option B: Kustomize
kubectl apply -k k8s/overlays/production

# Option C: Raw manifests
kubectl apply -f k8s/calculator-deployment.yaml
kubectl apply -f k8s/calculator-service.yaml
```

#### Step 4: Verify Deployment

```bash
# Check pods are running
kubectl get pods -l app.kubernetes.io/part-of=cyberfabric

# Check services are discoverable
kubectl get svc

# Test health endpoints
kubectl port-forward svc/calculator 8080:80
curl http://localhost:8080/health/ready
```

#### Minimal K8s Requirements

CyberFabric modules are standard containers. They need:

| Requirement | Provided By | CyberFabric Provides |
|-------------|-------------|---------------------|
| Container runtime | K8s (containerd/docker) | Docker images |
| Service discovery | K8s DNS or DirectoryService | Health endpoints |
| Load balancing | K8s Service | Readiness probes |
| Config management | K8s ConfigMap/Secret | Env var support |
| Scaling | K8s HPA | Stateless design |
| Logging | K8s logging (fluentd, etc.) | Structured JSON logs |
| Monitoring | Prometheus/Grafana | Metrics endpoint |

**We don't reinvent K8s primitives** — modules are standard containers that work with any K8s tooling.

### 2.5 Multi-Pod Patterns (Dispatcher/Worker)

Some modules require multiple pod types working together (e.g., dispatcher + workers, API + background processor).

#### 2.5.1 Pattern: Dispatcher + Worker

```
┌─────────────────────────────────────────────────────────────┐
│                     Kubernetes Cluster                      │
│                                                             │
│  ┌─────────────┐         ┌─────────────────────────────┐    │
│  │ Dispatcher  │         │        Worker Pool          │    │
│  │  (1 pod)    │────────▶│  ┌───────┐ ┌───────┐        │    │
│  │             │  Queue  │  │Worker1│ │Worker2│ ...    │    │
│  │ REST API    │◀────────│  └───────┘ └───────┘        │    │
│  └─────────────┘ Results │                             │    │
│                          └─────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

**Implementation:**

```yaml
# dispatcher-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: file-parser-dispatcher
spec:
  replicas: 1  # Single dispatcher
  template:
    spec:
      containers:
      - name: dispatcher
        image: ghcr.io/cyberfabric/file-parser-oop:latest
        args: ["--role", "dispatcher"]
        env:
        - name: WORKER_QUEUE_URL
          value: "redis://redis:6379"
---
# worker-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: file-parser-worker
spec:
  replicas: 5  # Scale workers independently
  template:
    spec:
      containers:
      - name: worker
        image: ghcr.io/cyberfabric/file-parser-oop:latest
        args: ["--role", "worker"]
        env:
        - name: WORKER_QUEUE_URL
          value: "redis://redis:6379"
```

**Module code:**

```rust
// Single binary, role selected at runtime
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let role = std::env::var("MODKIT_ROLE").unwrap_or_else(|_| "standalone".to_owned());
    
    match role.as_str() {
        "dispatcher" => run_dispatcher().await,
        "worker" => run_worker().await,
        "standalone" => run_standalone().await,  // Both in one (for local dev)
        _ => anyhow::bail!("Unknown role: {}", role),
    }
}
```

#### 2.5.2 Pattern: API + Background Processor

```yaml
# Same image, different entry points
apiVersion: apps/v1
kind: Deployment
metadata:
  name: notifications-api
spec:
  replicas: 3
  template:
    spec:
      containers:
      - name: api
        image: ghcr.io/cyberfabric/notifications-oop:latest
        args: ["--role", "api"]
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: notifications-processor
spec:
  replicas: 2
  template:
    spec:
      containers:
      - name: processor
        image: ghcr.io/cyberfabric/notifications-oop:latest
        args: ["--role", "processor"]
```

#### 2.5.3 When to Use Multi-Pod Patterns

| Pattern | Use When |
|---------|----------|
| **Single pod** | Simple request/response, stateless |
| **Dispatcher + Workers** | Long-running tasks, need queue-based scaling |
| **API + Processor** | Async processing, event-driven workflows |
| **Leader + Followers** | Coordination required, exactly-once semantics |

#### 2.5.4 Communication Between Pods

| Method | Use Case | K8s Primitive |
|--------|----------|---------------|
| **REST/gRPC** | Sync request/response | Service |
| **Message Queue** | Async tasks (Redis, RabbitMQ, SQS) | External or StatefulSet |
| **Shared DB** | State coordination | External or StatefulSet |
| **K8s Events** | Loose coupling | K8s API |

**We leverage K8s and external infrastructure** — no custom queue or coordination layer.

### 2.6 Compile-Time HTTP Client for Backward Compatibility

For modules that need to support both in-process and OoP deployment without runtime overhead:

```rust
// In SDK crate
#[cfg(feature = "in-process")]
pub use crate::local_client::LocalCalculatorClient as CalculatorClient;

#[cfg(not(feature = "in-process"))]
pub use crate::rest_client::LazyCalculatorClient as CalculatorClient;
```

This allows:
- **In-process builds**: Direct function calls, no HTTP overhead
- **OoP builds**: REST client with lazy resolution

**Cargo.toml:**
```toml
[features]
default = []
in-process = []  # Use local client (direct calls)
# Default (no feature) = REST client for OoP
```

---

## 3. Deployment Models

### 3.1 Local Development

```yaml
# config.yaml
modules:
  calculator:
    runtime:
      type: oop
      execution:
        executable_path: "target/debug/calculator-oop"
        environment:
          RUST_LOG: "debug"
```

**Characteristics:**
- Host spawns OoP modules as child processes
- DirectoryService runs in-process on host
- Modules connect via localhost
- Single machine, multiple processes

### 3.2 Kubernetes Deployment

```yaml
# k8s/calculator-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: calculator
spec:
  replicas: 3
  selector:
    matchLabels:
      app: calculator
  template:
    metadata:
      labels:
        app: calculator
    spec:
      containers:
      - name: calculator
        image: ghcr.io/cyberfabric/calculator-oop:1.2.3
        ports:
        - containerPort: 8080
        env:
        - name: MODKIT_DIRECTORY_ENDPOINT
          value: "http://directory-service:50051"
        resources:
          requests:
            memory: "128Mi"
            cpu: "100m"
          limits:
            memory: "512Mi"
            cpu: "500m"
        livenessProbe:
          httpGet:
            path: /health/live
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 10
        readinessProbe:
          httpGet:
            path: /health/ready
            port: 8080
          initialDelaySeconds: 5
          periodSeconds: 5
---
apiVersion: v1
kind: Service
metadata:
  name: calculator
spec:
  selector:
    app: calculator
  ports:
  - port: 80
    targetPort: 8080
```

**Characteristics:**
- Each module is a separate Deployment
- Kubernetes Service for load balancing
- DirectoryService as a separate Deployment (or use K8s DNS directly)
- Horizontal Pod Autoscaler for scaling
- Resource limits per module

### 3.3 On-Premises / VM Deployment

```yaml
# ansible/calculator.yaml
- name: Deploy Calculator Module
  hosts: calculator_servers
  tasks:
    - name: Copy binary
      copy:
        src: calculator-oop
        dest: /opt/cyberfabric/bin/calculator-oop
        mode: '0755'
    
    - name: Create systemd service
      template:
        src: calculator.service.j2
        dest: /etc/systemd/system/calculator.service
    
    - name: Start service
      systemd:
        name: calculator
        state: started
        enabled: yes
```

**systemd service template:**
```ini
[Unit]
Description=Calculator OoP Module
After=network.target

[Service]
Type=simple
User=cyberfabric
Environment=MODKIT_DIRECTORY_ENDPOINT=http://directory.internal:50051
Environment=RUST_LOG=info
ExecStart=/opt/cyberfabric/bin/calculator-oop
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

**Characteristics:**
- Binary distribution via package manager or direct copy
- systemd for process management
- Internal DNS or static IPs for service discovery
- Manual or Ansible-based scaling

### 3.4 Hybrid and Multi-Environment Deployments

Real-world deployments often involve hybrid environments where CyberFabric services coexist with vendor services, external databases, and legacy systems. This section covers discovery strategies for these scenarios.

#### 3.4.0 ModKit vs External: What's the Boundary?

**ModKit manages:**
- CyberFabric modules (in-process or OoP)
- Module lifecycle (init → migrate → start → stop)
- Inter-module communication (REST/gRPC via DirectoryService or K8s DNS)
- Module configuration and health

**External (not ModKit's concern):**
- Customer's existing services
- Vendor APIs and third-party integrations
- Databases (Postgres, Redis, etc.) — ModKit connects to them, doesn't manage them
- Message queues (RabbitMQ, Kafka, SQS) — ModKit uses them, doesn't manage them
- K8s infrastructure (networking, storage, ingress)
- Observability stack (Prometheus, Grafana, Jaeger)

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Customer's K8s Cluster                           │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    ModKit-Managed (CyberFabric)                 │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────────────┐   │    │
│  │  │Calculator│  │FileParser│  │LLM-GW    │  │DirectoryService│   │    │
│  │  │  (OoP)   │  │  (OoP)   │  │  (OoP)   │  │   (optional)   │   │    │
│  │  └──────────┘  └──────────┘  └──────────┘  └────────────────┘   │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                              │                                          │
│                              │ Standard K8s/HTTP/gRPC                   │
│                              ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    External (Customer-Managed)                  │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐     │    │
│  │  │ Postgres │  │  Redis   │  │ Vendor   │  │ Customer's   │     │    │
│  │  │   (DB)   │  │ (Cache)  │  │   API    │  │  Services    │     │    │
│  │  └──────────┘  └──────────┘  └──────────┘  └──────────────┘     │    │
│  └─────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────┘
```

**Key principle:** CyberFabric modules are **good K8s citizens** — they use standard protocols (HTTP, gRPC), standard config (env vars, ConfigMaps), and standard observability (health endpoints, structured logs, metrics). They don't require special infrastructure beyond what K8s already provides.

#### 3.4.1 Discovery Strategy Options

| Strategy | Description | Best For |
|----------|-------------|----------|
| **DirectoryService** | Central gRPC registry with heartbeats | Cross-environment, metadata-rich |
| **K8s DNS** | Native Kubernetes service discovery | Pure K8s, simple setups |
| **Static Config** | Hardcoded endpoints in config | External services, legacy systems |
| **Hybrid** | DirectoryService for CyberFabric + static for external | Mixed environments |

#### 3.4.2 K8s DNS vs DirectoryService

**When to use K8s DNS:**
- All services run in the same K8s cluster
- No need for instance-level metadata (version, tags)
- Simple deployments with K8s-native health checks
- Want zero additional infrastructure

**When to use DirectoryService:**
- Services span multiple clusters or environments
- Need client-side load balancing control (weighted, sticky)
- Require instance metadata for routing decisions
- Local development or on-prem without K8s
- Need visibility into which instance handled a request

**How communication changes:**

```
K8s DNS:
  Client → http://calculator:80 → K8s Service → Pod
  (K8s handles LB, health checks, retries)

DirectoryService:
  Client → DirectoryService.resolve("calculator") → http://10.0.1.42:8080 → Pod
  (Client handles LB, circuit breaker, retries)
```

#### 3.4.3 Hybrid Environment Patterns

**Pattern 1: CyberFabric in K8s + External Database**

```yaml
# CyberFabric modules use DirectoryService for inter-module communication
# External DB configured via static connection string
modules:
  calculator:
    runtime:
      type: oop
    database:
      server: external-postgres  # Static config, not discovered
      
database:
  servers:
    external-postgres:
      host: "db.vendor.example.com"  # External vendor DB
      port: 5432
```

**Pattern 2: CyberFabric + Vendor Services in Same Cluster**

```yaml
# CyberFabric modules: DirectoryService discovery
# Vendor services: K8s DNS or static endpoints
modules:
  calculator:
    runtime:
      type: oop
      discovery: directory  # Use DirectoryService
      
  vendor-api-gateway:
    runtime:
      type: external
      endpoint: "http://vendor-gateway.vendor-ns.svc.cluster.local"  # K8s DNS
```

**Pattern 3: Multi-Cluster Federation**

```
┌─────────────────────────────┐     ┌─────────────────────────────┐
│      Cluster A (US-East)    │     │     Cluster B (EU-West)     │
│  ┌─────────────────────┐    │     │    ┌─────────────────────┐  │
│  │  DirectoryService   │◄───┼─────┼───►│  DirectoryService   │  │
│  └─────────────────────┘    │     │    └─────────────────────┘  │
│           │                 │     │             │               │
│     ┌─────┴─────┐           │     │       ┌─────┴─────┐         │
│     ▼           ▼           │     │       ▼           ▼         │
│ Calculator   FileParser     │     │   Calculator   LLM-Gateway  │
└─────────────────────────────┘     └─────────────────────────────┘

Cross-cluster discovery via federated DirectoryService
```

#### 3.4.4 External Service Integration

For services outside CyberFabric's control (vendor APIs, legacy systems):

```rust
// Option 1: Static endpoint in ClientConfig
impl ClientDescriptor for VendorApiDescriptor {
    fn config() -> ClientConfig {
        ClientConfig {
            discovery: DiscoveryStrategy::Static {
                endpoint: std::env::var("VENDOR_API_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.vendor.com".to_owned()),
            },
            ..ClientConfig::rest()
        }
    }
}

// Option 2: K8s DNS for in-cluster vendor services
impl ClientDescriptor for VendorInClusterDescriptor {
    fn config() -> ClientConfig {
        ClientConfig {
            discovery: DiscoveryStrategy::KubernetesDns {
                service_name: "vendor-service",
                namespace: Some("vendor-ns".to_owned()),
                port: 8080,
            },
            ..ClientConfig::rest()
        }
    }
}
```

#### 3.4.5 Discovery Strategy Enum

```rust
/// How to discover the target service endpoint.
#[derive(Debug, Clone)]
pub enum DiscoveryStrategy {
    /// Use DirectoryService for dynamic discovery (default for CyberFabric modules).
    Directory,
    
    /// Use Kubernetes DNS (service-name.namespace.svc.cluster.local).
    KubernetesDns {
        service_name: String,
        namespace: Option<String>,
        port: u16,
    },
    
    /// Static endpoint (for external services, legacy systems).
    Static {
        endpoint: String,
    },
}

impl Default for DiscoveryStrategy {
    fn default() -> Self {
        Self::Directory
    }
}
```

### 3.5 Deployment Model Comparison

| Aspect | Local | Kubernetes | On-Prem | Hybrid |
|--------|-------|------------|---------|--------|
| **Scaling** | Manual | HPA/VPA | Manual/Scripts | Mixed |
| **Discovery** | In-process | K8s DNS or DirectoryService | DirectoryService | DirectoryService + Static |
| **Load Balancing** | Round-robin (client) | K8s Service or client | HAProxy/Nginx | Mixed |
| **Health Checks** | Heartbeat | Probes | Heartbeat | Mixed |
| **Restart Policy** | Host respawns | Pod restart | systemd | Mixed |
| **Resource Limits** | OS limits | Pod limits | cgroups | Mixed |
| **External Services** | Static config | K8s DNS or static | Static config | Static config |

---

## 4. Service Discovery and Communication

### 4.1 DirectoryService

The DirectoryService is the central registry for module instances:

```rust
#[async_trait]
pub trait DirectoryClient: Send + Sync {
    /// Resolve REST endpoint for a module (default for OoP).
    async fn resolve_rest_service(&self, module_name: &str) -> Result<RestEndpoint>;
    
    /// Resolve gRPC endpoint for a module (opt-in).
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint>;
    
    /// Register this instance with the directory.
    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()>;
    
    /// Deregister this instance.
    async fn deregister_instance(&self, module: &str, instance_id: &str) -> Result<()>;
    
    /// Send heartbeat to maintain healthy status.
    async fn send_heartbeat(&self, module: &str, instance_id: &str) -> Result<()>;
}
```

### 4.2 Instance Registration

OoP modules register on startup:

```rust
// During OoP bootstrap
let register_info = RegisterInstanceInfo {
    module: "calculator".to_owned(),
    instance_id: instance_id.to_string(),
    rest_endpoint: Some(RestEndpoint::http("0.0.0.0", 8080)),
    grpc_services: vec![],  // Optional gRPC services
    version: Some("1.2.3".to_owned()),
};

directory_client.register_instance(register_info).await?;
```

### 4.3 Health States

```rust
pub enum InstanceState {
    Registered,   // Just registered, not yet healthy
    Ready,        // Passed readiness check
    Healthy,      // Receiving heartbeats
    Quarantined,  // Failed health checks, excluded from routing
    Draining,     // Shutting down, no new requests
}
```

**State transitions:**
```
Registered ──[first heartbeat]──▶ Healthy
Healthy ──[missed heartbeats]──▶ Quarantined
Healthy ──[shutdown signal]──▶ Draining
Quarantined ──[heartbeat resumes]──▶ Healthy
```

### 4.4 Load Balancing

Client-side round-robin across healthy instances:

```rust
impl ModuleManager {
    pub fn pick_rest_endpoint_round_robin(
        &self,
        module_name: &str,
    ) -> Option<(String, Arc<ModuleInstance>, Endpoint)> {
        // Filter to healthy/ready instances with REST endpoints
        let candidates: Vec<_> = self.instances_of(module_name)
            .into_iter()
            .filter(|inst| {
                inst.rest_endpoint.is_some() 
                && matches!(inst.state(), InstanceState::Healthy | InstanceState::Ready)
            })
            .collect();
        
        // Round-robin selection
        let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % candidates.len();
        candidates.get(idx).cloned()
    }
}
```

### 4.5 Transport Selection

| Transport | Default | Use Case |
|-----------|---------|----------|
| **REST** | ✅ Yes | Standard request/response, broad compatibility |
| **gRPC** | Opt-in | Streaming, high-throughput, binary protocols |

**Selecting transport in ClientDescriptor:**
```rust
impl ClientDescriptor for CalculatorClientDescriptor {
    fn config() -> ClientConfig {
        ClientConfig::rest()  // Default
        // or: ClientConfig::grpc()  // Opt-in for streaming
    }
}
```

---

## 5. Fault Tolerance

### 5.1 Failure Modes and Mitigations

| Failure Mode | Detection | Mitigation |
|--------------|-----------|------------|
| **OoP module crash** | Missed heartbeats | Instance quarantined, requests routed to healthy instances |
| **Network partition** | Connection timeout | Backoff + retry, circuit breaker opens |
| **Slow response** | Request timeout | Timeout error, retry with backoff |
| **Module overload** | 503 responses | Circuit breaker, shed load to other instances |
| **Directory unavailable** | Connection error | Cached endpoints, graceful degradation |
| **Invalid response** | Parse error | SDK error, HTTP 502 to caller |

### 5.2 Retry Strategy

**Endpoint Resolution Retries:**
```rust
// Exponential backoff for directory resolution
fn calculate_backoff(&self, failure_count: u32) -> Duration {
    let base = Duration::from_millis(100);
    let max = self.config.max_backoff;  // Default: 60s
    let backoff = base.saturating_mul(2u32.saturating_pow(failure_count.min(10)));
    backoff.min(max)
}
```

**HTTP Request Retries:**
```rust
pub struct RetryPolicy {
    pub max_retries: u32,                    // Default: 2
    pub retryable_status_codes: Vec<u16>,    // Default: [502, 503, 504]
    pub use_idempotency_keys: bool,          // Default: true
    pub retry_base_delay: Duration,          // Default: 100ms
}
```

### 5.3 Idempotency

For safe retries of non-idempotent operations:

| HTTP Method | Idempotent? | Retry Strategy |
|-------------|-------------|----------------|
| GET, HEAD, OPTIONS | ✅ Yes | Retry on 5xx/timeout |
| PUT, DELETE | ✅ Usually | Retry on 5xx/timeout |
| POST, PATCH | ❌ No | Retry only with `Idempotency-Key` header |

**Client-side idempotency key:**
```rust
// Generate key ONCE per logical operation
let idempotency_key = Uuid::new_v4().to_string();

// Same key used for all retries
client.post(&url)
    .header("Idempotency-Key", &idempotency_key)
    .json(&input)
    .send()
    .await
```

**Server-side contract:**
- Store `(idempotency_key, tenant_id) → response` with 24h TTL
- Return cached response if key seen before
- Process normally if key is new

### 5.4 Circuit Breaker

Prevents cascading failures by stopping requests to failing modules:

```
CLOSED (normal) ──[5 failures]──▶ OPEN (fail fast) ──[30s timeout]──▶ HALF-OPEN (probe)
    ▲                                    ▲                                   │
    │                                    └────────[probe fails]──────────────┤
    └─────────────────────────[2 successful probes]──────────────────────────┘
```

**Configuration:**
```rust
pub struct CircuitBreakerConfig {
    pub failure_threshold: u32,      // Default: 5
    pub reset_timeout: Duration,     // Default: 30s
    pub success_threshold: u32,      // Default: 2
    pub enabled: bool,               // Default: true
}
```

### 5.5 Fallback Strategies

When circuit breaker is open:

| Strategy | Behavior |
|----------|----------|
| `FailFast` (default) | Return error immediately (HTTP 424) |
| `CachedResponse { max_age }` | Return stale cached response |
| `StaticDefault` | SDK provides default value |
| `AlternativeService { module }` | Route to backup service |

### 5.6 Graceful Degradation

When an OoP dependency is unavailable:

1. **Per-operation failure** — Only affected endpoints return HTTP 424
2. **Module stays healthy** — Other endpoints continue working
3. **No startup blocking** — Module starts even if dependencies unavailable
4. **Automatic recovery** — Requests succeed once dependency is back

```rust
// Handler with graceful degradation
async fn calculate(
    State(calculator): State<Arc<dyn CalculatorClientV1>>,
    Json(input): Json<CalculateInput>,
) -> ApiResult<Json<CalculateOutput>> {
    calculator.add(&ctx, input.a, input.b)
        .await
        .map(|result| Json(CalculateOutput { result }))
        .map_err(|e| {
            // Maps to HTTP 424 Failed Dependency
            Problem::failed_dependency()
                .with_detail(format!("Calculator unavailable: {}", e))
        })
}
```

### 5.7 Health Endpoints

Every OoP module exposes standard health endpoints:

```
GET /health/live   → 200 OK (process is running)
GET /health/ready  → 200 OK (ready to accept traffic)
                   → 503 Service Unavailable (not ready)
```

**Readiness with required dependencies:**
```rust
impl ClientDescriptor for CriticalDependencyDescriptor {
    fn config() -> ClientConfig {
        ClientConfig {
            availability_policy: ClientAvailabilityPolicy::Required,
            ..ClientConfig::rest()
        }
    }
}
// Readiness probe fails until this dependency is resolvable
```

---

## 6. SDK Pattern

### 6.1 SDK Crate Structure

```
calculator-sdk/
├── Cargo.toml
└── src/
    ├── lib.rs              # Re-exports
    ├── api.rs              # API trait + types + errors
    ├── client.rs           # LazyCalculatorClient (REST)
    ├── descriptor.rs       # ClientDescriptor impl
    └── proto/              # Optional: gRPC definitions
        └── calculator.proto
```

### 6.2 API Trait

```rust
// calculator-sdk/src/api.rs
use async_trait::async_trait;
use modkit::security::SecurityContext;

#[async_trait]
pub trait CalculatorClientV1: Send + Sync {
    async fn add(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError>;
    async fn subtract(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CalculatorError {
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },
    
    #[error("calculator unavailable: {message}")]
    Unavailable { message: String },
    
    #[error("internal error: {message}")]
    Internal { message: String },
}
```

### 6.3 ClientDescriptor

```rust
// calculator-sdk/src/descriptor.rs
use modkit::clients::descriptor::{ClientDescriptor, ClientConfig};

pub struct CalculatorClientDescriptor;

impl ClientDescriptor for CalculatorClientDescriptor {
    type Api = dyn CalculatorClientV1;
    const MODULE_NAME: &'static str = "calculator";
    
    fn config() -> ClientConfig {
        ClientConfig::rest()
    }
}
```

### 6.4 Lazy REST Client

```rust
// calculator-sdk/src/client.rs
use crate::api::{CalculatorClientV1, CalculatorError};
use modkit::clients::rest_provider::RestClientProvider;

pub struct LazyCalculatorClient {
    provider: RestClientProvider,
}

impl LazyCalculatorClient {
    pub fn new(provider: RestClientProvider) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl CalculatorClientV1 for LazyCalculatorClient {
    async fn add(&self, ctx: &SecurityContext, a: i64, b: i64) -> Result<i64, CalculatorError> {
        let base_url = self.provider.get_base_url().await
            .map_err(|e| CalculatorError::Unavailable { message: e.to_string() })?;
        
        let url = format!("{}/api/v1/calculator/add", base_url);
        
        let response = self.provider.http_client()
            .post(&url)
            .header("x-tenant-id", ctx.tenant_id_str())
            .json(&serde_json::json!({ "a": a, "b": b }))
            .send()
            .await
            .map_err(|e| CalculatorError::Unavailable { message: e.to_string() })?;
        
        if !response.status().is_success() {
            return Err(map_http_error(response.status(), &response.text().await.unwrap_or_default()));
        }
        
        let result: AddResponse = response.json().await
            .map_err(|e| CalculatorError::Internal { message: e.to_string() })?;
        
        Ok(result.result)
    }
    
    // ... other methods
}
```

---

## 7. Configuration

### 7.1 Host Configuration (spawns OoP modules)

```yaml
# config.yaml
modules:
  calculator:
    runtime:
      type: oop
      execution:
        executable_path: "~/.hyperspot/bin/calculator-oop"
        args: ["--verbose", "2"]
        working_directory: null
        environment:
          RUST_LOG: "info"
          CUSTOM_VAR: "value"
    config:
      max_precision: 10
      cache_ttl_secs: 300
```

### 7.2 OoP Module Configuration

OoP modules receive configuration via:

1. **Environment variable** `MODKIT_MODULE_CONFIG` — JSON from host
2. **Config file** `--config path/to/config.yaml` — Local overrides
3. **CLI arguments** — Runtime overrides

**Merge priority (highest wins):**
```
CLI args > Local config file > MODKIT_MODULE_CONFIG from host
```

### 7.3 Environment Variables

| Variable | Description |
|----------|-------------|
| `MODKIT_DIRECTORY_ENDPOINT` | DirectoryService gRPC endpoint |
| `MODKIT_MODULE_CONFIG` | JSON config from host |
| `MODKIT_CONFIG_PATH` | Path to config file |
| `RUST_LOG` | Log level filter |

### 7.4 Runtime Configuration Updates

OoP modules may need configuration updates without restart. This section covers strategies for dynamic configuration.

#### 7.4.1 Configuration Update Strategies

| Strategy | Mechanism | Use Case | Downtime |
|----------|-----------|----------|----------|
| **Restart** | Kill + respawn with new config | Simple, stateless modules | Brief (seconds) |
| **Rolling Update** | K8s Deployment rollout | Stateless, multiple replicas | Zero |
| **Config Reload Signal** | SIGHUP triggers reload | Long-running, stateful | Zero |
| **Config Watch** | File/ConfigMap watch | Frequent changes | Zero |
| **Control Plane API** | gRPC/REST endpoint | Programmatic updates | Zero |

#### 7.4.2 Restart-Based Updates (Default)

For most OoP modules, restart is the simplest and safest approach:

```yaml
# Kubernetes: Update ConfigMap, then rollout restart
kubectl rollout restart deployment/calculator

# Local: Host respawns with new config
# 1. Update config.yaml
# 2. Send SIGTERM to OoP process
# 3. Host detects exit and respawns with new config
```

**Pros:** Simple, no special code needed, guaranteed clean state  
**Cons:** Brief downtime per instance, connection drain required

#### 7.4.3 Signal-Based Reload (SIGHUP)

For modules that need zero-downtime config updates:

```rust
// In OoP module bootstrap
use tokio::signal::unix::{signal, SignalKind};

let mut sighup = signal(SignalKind::hangup())?;
tokio::spawn(async move {
    loop {
        sighup.recv().await;
        tracing::info!("SIGHUP received, reloading configuration");
        if let Err(e) = config_provider.reload().await {
            tracing::error!(error = %e, "Config reload failed");
        }
    }
});
```

**What can be reloaded:**
- Feature flags
- Rate limits
- Cache TTLs
- Log levels

**What requires restart:**
- Database connections
- Listening ports
- TLS certificates
- Module dependencies

#### 7.4.4 Kubernetes ConfigMap Watch

For K8s deployments, modules can watch ConfigMap changes:

```yaml
# Mount ConfigMap as volume with auto-update
spec:
  containers:
  - name: calculator
    volumeMounts:
    - name: config
      mountPath: /etc/calculator
  volumes:
  - name: config
    configMap:
      name: calculator-config
```

```rust
// Watch for config file changes
use notify::{Watcher, RecursiveMode};

let mut watcher = notify::recommended_watcher(|res| {
    if let Ok(event) = res {
        if event.kind.is_modify() {
            config_provider.reload().await;
        }
    }
})?;
watcher.watch("/etc/calculator/config.yaml", RecursiveMode::NonRecursive)?;
```

#### 7.4.5 Control Plane API (Future)

For programmatic configuration updates, modules can expose a control endpoint:

```rust
// Control plane endpoint (internal only, not exposed via API gateway)
POST /internal/config/reload
POST /internal/config/update { "key": "value" }
GET  /internal/config/current
```

**Security considerations:**
- Control endpoints must be internal-only (not routed through API gateway)
- Require mTLS or internal network isolation
- Audit log all configuration changes

#### 7.4.6 Recommended Approach by Module Type

| Module Type | Recommended Strategy |
|-------------|---------------------|
| Stateless API handlers | Rolling restart (K8s) or restart (local) |
| Long-running workers | SIGHUP reload for safe params, restart for connections |
| Stateful services | SIGHUP reload + graceful drain for restarts |
| High-availability critical | Multiple replicas + rolling update |

---

## 8. Lifecycle Management

### 8.1 Startup Sequence

```
1. Parse CLI args and load config
2. Initialize logging (with OTEL if configured)
3. Connect to DirectoryService
4. Register instance with directory
5. Start heartbeat loop (background)
6. Run module lifecycle:
   a. pre_init() — Before any initialization
   b. init() — Register clients, load config
   c. migrate() — Database migrations
   d. start() — Start HTTP server, background tasks
7. Signal ready (readiness probe passes)
8. Accept traffic
```

### 8.2 Shutdown Sequence

```
1. Receive shutdown signal (SIGTERM/SIGINT)
2. Cancel root CancellationToken
3. Stop accepting new requests
4. Wait for in-flight requests (grace period)
5. Stop background tasks
6. Deregister from DirectoryService
7. Flush telemetry
8. Exit
```

### 8.3 Cancellation Token Pattern

```rust
// Root token created at bootstrap
let cancel = CancellationToken::new();

// Child tokens for background tasks
let heartbeat_cancel = cancel.child_token();
tokio::spawn(async move {
    loop {
        tokio::select! {
            () = heartbeat_cancel.cancelled() => break,
            () = sleep(interval) => send_heartbeat().await,
        }
    }
});

// Shutdown triggers cancellation
signal::ctrl_c().await?;
cancel.cancel();
```

---

## 9. Migration Guide

### 9.1 Converting In-Process to OoP

1. **Create SDK crate** with API trait, types, and ClientDescriptor
2. **Add `main.rs`** to module crate for OoP binary
3. **Implement REST handlers** for API operations
4. **Update host config** to set `runtime.type: oop`
5. **Test** with lazy client resolution

### 9.2 Backward Compatibility

Existing `wire_client()` code continues to work:

```rust
// Old pattern (still works)
calculator_sdk::wire_client(&hub, &directory).await?;

// New pattern (recommended)
#[modkit::module(
    clients = [CalculatorClientDescriptor],
)]
pub struct MyModule;
// Lazy client auto-registered
```

---

## 10. Quick Reference

### 10.1 Checklist: New OoP Module

- [ ] Create `{module}-sdk` crate with API trait, types, errors
- [ ] Implement `ClientDescriptor` in SDK
- [ ] Create `LazyClient` implementation (REST or gRPC)
- [ ] Add `main.rs` for OoP binary entry point
- [ ] Implement REST handlers for API operations
- [ ] Add health endpoints (`/health/live`, `/health/ready`)
- [ ] Create Dockerfile for container image
- [ ] Add to host config with `runtime.type: oop`
- [ ] Test fault tolerance (kill module, verify recovery)

### 10.2 Error Mapping

| SDK Error | HTTP Status | When |
|-----------|-------------|------|
| `Unavailable` | 424 Failed Dependency | Module not reachable |
| `InvalidArgument` | 400 Bad Request | Validation failed |
| `NotFound` | 404 Not Found | Resource doesn't exist |
| `Internal` | 500 Internal Server Error | Unexpected error |
| Circuit open | 424 Failed Dependency | Too many failures |
| Version mismatch | 424 Failed Dependency | API incompatible |

### 10.3 Default Timeouts

| Timeout | Default | Configurable |
|---------|---------|--------------|
| Connect | 5s | `ClientConfig.connect_timeout` |
| Request | 30s | `ClientConfig.request_timeout` |
| Max backoff | 60s | `ClientConfig.max_backoff` |
| Heartbeat interval | 5s | `OopRunOptions.heartbeat_interval_secs` |
| Circuit reset | 30s | `CircuitBreakerConfig.reset_timeout` |

### 10.4 Key Files

| File | Purpose |
|------|---------|
| `libs/modkit/src/bootstrap/oop.rs` | OoP bootstrap library |
| `libs/modkit/src/directory.rs` | DirectoryClient trait + LocalDirectoryClient |
| `libs/modkit/src/runtime/module_manager.rs` | Instance tracking, round-robin |
| `libs/modkit/src/clients/rest_provider.rs` | Lazy REST client infrastructure |
| `docs/adrs/modkit/0003-modkit-universal-lazy-layer.md` | ADR for lazy client design |

---

## Appendix A: Comparison with Alternatives

### A.1 OoP vs In-Process

| Aspect | In-Process | OoP |
|--------|------------|-----|
| Latency | Nanoseconds | Milliseconds |
| Isolation | Shared memory | Process boundary |
| Scaling | Vertical only | Horizontal |
| Deployment | Single binary | Multiple binaries/containers |
| Debugging | Single process | Distributed tracing |
| Resource limits | Shared | Per-module |

### A.2 REST vs gRPC for OoP

| Aspect | REST | gRPC |
|--------|------|------|
| Default | ✅ Yes | Opt-in |
| Streaming | ❌ No (use SSE) | ✅ Yes |
| Browser compat | ✅ Yes | ❌ No (needs proxy) |
| Schema | OpenAPI | Protobuf |
| Performance | Good | Better |
| Tooling | Ubiquitous | Specialized |

---

## Appendix B: Troubleshooting

### B.1 Module Not Discoverable

**Symptoms:** `resolve_rest_service()` returns "not found"

**Checks:**
1. Is module registered? Check DirectoryService logs
2. Is module healthy? Check heartbeat logs
3. Is REST endpoint registered? Check `RegisterInstanceInfo.rest_endpoint`
4. Is module name correct? Case-sensitive match

### B.2 Circuit Breaker Stuck Open

**Symptoms:** All requests fail immediately with "circuit open"

**Checks:**
1. Is target module actually healthy?
2. Has `reset_timeout` elapsed?
3. Are probe requests succeeding?

**Resolution:**
- Fix underlying issue in target module
- Wait for reset timeout
- Restart consumer module (resets circuit state)

### B.3 Idempotency Key Conflicts

**Symptoms:** Duplicate operations or missing responses

**Checks:**
1. Is key generated once per logical operation?
2. Is server storing key → response mapping?
3. Is TTL appropriate (24h recommended)?

---

*This document is the authoritative reference for OoP module design. For implementation details, see the linked ADR and code references.*
