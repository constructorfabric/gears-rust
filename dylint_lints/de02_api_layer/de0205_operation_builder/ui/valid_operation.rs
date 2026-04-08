// simulated_dir=modules/simple-resource-registry/simple-resource-registry/src/api/rest

use modkit::api::OperationBuilder;

const VALID_TAG: &str = "Simple Resource Registry";
const ANOTHER_VALID_TAG: &str = "User Management";

fn valid_operations() {
    // Should not trigger DE0205 - Operation builder with tag and summary
    let router1: OperationBuilder<_, _, ()> = OperationBuilder::post("/resources")
        .operation_id("create_resource")
        .summary("Create a new resource")
        .tag("Simple Resource Registry");  // proper format

    // Should not trigger DE0205 - Operation builder with tag and summary
    let router2: OperationBuilder<_, _, ()> = OperationBuilder::get("/resources/{id}")
        .operation_id("get_resource")
        .summary("Get resource by ID")
        .tag("Registry");  // single capital word

    // Should not trigger DE0205 - Operation builder with tag and summary
    let router3: OperationBuilder<_, _, ()> = OperationBuilder::put("/resources/{id}")
        .operation_id("update_resource")
        .summary("Update an existing resource")
        .tag("User Management System");  // multiple capital words

    // Should not trigger DE0205 - Operation builder with tag and summary
    let router4: OperationBuilder<_, _, ()> = OperationBuilder::delete("/resources/{id}")
        .operation_id("delete_resource")
        .summary("Delete a resource")
        .tag("API V1 Resources");  // capital with numbers

    // Should not trigger DE0205 - Operation builder with const tag and summary
    let router5: OperationBuilder<_, _, ()> = OperationBuilder::patch("/resources/{id}")
        .operation_id("patch_resource")
        .summary("Partially update a resource")
        .tag(VALID_TAG);  // const with valid format

    // Should not trigger DE0205 - Operation builder with const tag and summary
    let router6: OperationBuilder<_, _, ()> = OperationBuilder::get("/resources/all")
        .operation_id("list_resources")
        .summary("List all resources")
        .tag(ANOTHER_VALID_TAG);  // const with valid format

    _ = router1;
    _ = router2;
    _ = router3;
    _ = router4;
    _ = router5;
    _ = router6;
}

fn main() {
    valid_operations();
}
