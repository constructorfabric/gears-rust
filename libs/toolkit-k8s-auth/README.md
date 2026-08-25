# cf-gears-toolkit-k8s-auth

Kubernetes `TokenReview`-based platform-plane authenticator for ToolKit.

Provides `K8sTokenReviewAuthenticator`, a concrete
`toolkit_security::InternalAuthenticator` that validates an inbound
`X-ToolKit-Internal-Token` (a projected `ServiceAccount` JWT) via the Kubernetes
`TokenReview` API and resolves the caller to a
`PlatformIdentity::KubernetesServiceAccount`.

Kept in its own leaf crate so foundational crates stay free of the `kube` /
`k8s-openapi` dependency. Wire it into the OoP bootstrap via
`DynInternalAuthenticator` for Profile 3 (`InternalCredential::KubeServiceAccountToken`).
