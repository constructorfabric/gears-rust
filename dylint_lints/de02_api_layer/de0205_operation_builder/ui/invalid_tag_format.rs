// simulated_dir=modules/simple-resource-registry/simple-resource-registry/src/api/rest

use modkit::api::OperationBuilder;

const INVALID_TAG: &str = "simple resource registry";

fn invalid_tag_formats() {
    let _router1: OperationBuilder<_, _, ()> = OperationBuilder::post("/resources")
        .operation_id("create_resource")
        // Should trigger DE0205 - Operation builder tag
        .tag("simple resource registry")  // lowercase words
        .summary("Create a resource");

    let _router2: OperationBuilder<_, _, ()> = OperationBuilder::get("/resources/{id}")
        .operation_id("get_resource")
        // Should trigger DE0205 - Operation builder tag
        .tag("Simple resource registry")  // mixed case
        .summary("Get a resource");

    let _router3: OperationBuilder<_, _, ()> = OperationBuilder::put("/resources/{id}")
        .operation_id("update_resource")
        // Should trigger DE0205 - Operation builder tag
        .tag("registry")  // single lowercase word
        .summary("Update a resource");

    let _router4: OperationBuilder<_, _, ()> = OperationBuilder::delete("/resources/{id}")
        .operation_id("delete_resource")
        // Should trigger DE0205 - Operation builder tag
        .tag("")  // empty string
        .summary("Delete a resource");

    let tag_name = "Dynamic Tag";
    let _router5: OperationBuilder<_, _, ()> = OperationBuilder::get("/resources")
        .operation_id("list_resources")
        // Should trigger DE0205 - Operation builder tag
        .tag(tag_name)  // variable, not string literal or const
        .summary("List resources");

    let _router6: OperationBuilder<_, _, ()> = OperationBuilder::patch("/resources/{id}")
        .operation_id("patch_resource")
        // Should trigger DE0205 - Operation builder tag
        .tag(INVALID_TAG)  // const with invalid format
        .summary("Patch a resource");
}

fn main() {
    invalid_tag_formats();
}
