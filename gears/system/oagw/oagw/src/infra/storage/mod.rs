/// In-memory route repository.
pub(crate) mod route_repo;
/// In-memory upstream repository.
pub(crate) mod upstream_repo;

pub(crate) use route_repo::InMemoryRouteRepo;
pub(crate) use upstream_repo::InMemoryUpstreamRepo;
