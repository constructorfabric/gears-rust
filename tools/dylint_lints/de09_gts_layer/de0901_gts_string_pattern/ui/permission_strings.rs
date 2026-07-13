//! Test file for permission strings with colon-separated parts
//! Security claims can contain any GTS format: type schemas, instance segments, or wildcards
//! These should NOT trigger the lint because they are in permission string context

fn main() {
    // Test 1: Permission string with GTS type schema (ending with ~)
    // Should NOT trigger DE0901 - type schemas are allowed in permission strings
    let _perm1 = "550e8400-e29b-41d4-a716-446655440000:gts.cf.core.events.topic.v1~:*:publish";

    // Test 2: Permission string with GTS instance segment (not ending with ~)
    // Should NOT trigger DE0901 - instance segments are allowed in permission strings
    let _perm2 = "550e8400-e29b-41d4-a716-446655440000:gts.cf.core.events.tenant.v1~cf.core.example.tenant.v1:660e8400-e29b-41d4-a716-446655440002:edit";

    // Test 3: Permission string with GTS wildcard pattern
    // Should trigger DE0901 - invalid GTS
    let _perm3 = "resource-id:gts.cf.*.events.*.v1~:action:scope";

    // Test 4: Permission string with GTS wildcard pattern
    // Should trigger DE0901 - invalid GTS format
    let _perm3 = "resource-id:gts.cf.core.events.event.v1~a.b.c~:action:scope";

    // Test 5: Invalid GTS instance segment
    // Should trigger DE0901 - invalid GTS
    let _perm2 = "550e8400-e29b-41d4-a716-446655440000:gts.cf.events.tenant.v1:660e8400-e29b-41d4-a716-446655440002:edit";

    // Test 6: Invalid GTS identifier (not leading type segment)
    // Should trigger DE0901 - invalid GTS
    let _perm5 = "uuid:gts.vendor.pkg.ns.type.v1:action";

    // Test 7: Disallowed vendor in permission string
    // Should trigger DE0901 - invalid GTS vendor
    let _perm6 = "uuid:gts.badvendor.pkg.ns.type.v1~cf.pkg.ns.derived.v1~:action";

    let _perm7 = MockPermissionBuilder::default()
        // Should NOT trigger DE0901 - wildcards are allowed in resource_pattern() calls
        .resource_pattern("gts.cf.core.events.type.v*")
        .build();

    // Additional valid cases

    // Should NOT trigger DE0901 - typical permission string
    let _perm4 = "uuid:gts.cf.pkg.ns.type.v1~:action";

    // Should NOT trigger DE0901 - wildcards are allowed in resource_pattern() calls
    let _perm = MockPermissionBuilder::default()
        .resource_pattern("gts.cf.core.events.topic.v1~example.*")
        .build();

    // Should NOT trigger DE0901 - wildcards are allowed in resource_pattern() calls
    let _perm8 = MockPermissionBuilder::default()
        .resource_pattern("gts.cf.core.events.type.v1~*")
        .build();

    // Should NOT trigger DE0901 - wildcards are allowed in resolve_to_uuids() calls
    let _resolver = MockResolver;
    _resolver.resolve_to_uuids(&["gts.example.core.events.*".to_owned()]);
}

#[derive(Default)]
struct MockResolver;

impl MockResolver {
    fn resolve_to_uuids(&self, _patterns: &[String]) -> Vec<String> {
        vec![]
    }
}

#[derive(Default)]
struct MockPermissionBuilder {
    resource_pattern: Option<String>,
}

impl MockPermissionBuilder {
    fn resource_pattern(mut self, pattern: &str) -> Self {
        self.resource_pattern = Some(pattern.to_owned());
        self
    }

    fn build(self) -> String {
        self.resource_pattern.unwrap_or_default()
    }
}
