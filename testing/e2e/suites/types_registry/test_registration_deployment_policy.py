"""Vendor regions and the deployment compatibility-force switch."""

from copy import deepcopy
import os

import pytest

from .helpers import (
    TRACE_ID,
    assert_absent,
    assert_bad_request,
    assert_exact,
    assert_json,
    get_entity,
    invalid_argument,
    mandatory,
    outcome,
    post_registration,
    provenance,
    read_created,
    register_and_assert,
    schema_entity,
)


# The shared e2e config enables force; only the force-disabled profile sets "0".
FORCE_ENABLED = os.getenv("TYPES_REGISTRY_FORCE_ENABLED_E2E") != "0"


def _acme_schema(template, package):
    """Keep the test namespace while placing acme inside or outside its open region."""
    schema = deepcopy(template)
    assert schema["gts_id"].startswith("gts.cf.e2e.")
    schema["gts_id"] = schema["gts_id"].replace(
        "gts.cf.e2e.", f"gts.acme.{package}.", 1
    )
    schema["content"]["$id"] = f"gts://{schema['gts_id']}"
    return schema


@pytest.mark.scenario("TR-REG-901")
async def test_default_policy_refuses_non_platform_vendor(
    registry_http, registry_api_path, registration_fixture
):
    """The default stays closed for acme outside its configured e2e region."""
    schema = _acme_schema(registration_fixture("person_schema"), "outside")
    assert_bad_request(
        await post_registration(registry_http, registry_api_path, [schema]),
        {
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 400,
            "detail": "Operation precondition not met",
            "instance": "<request_path>",
            "trace_id": TRACE_ID,
            "context": {
                "violations": [
                    {
                        "type": "REGISTRATION_POLICY_ALLOWED_VENDORS",
                        "subject": "<default>",
                        "description": (
                            f"registration policy refuses '{schema['gts_id']}': vendor 'acme' "
                            "is not admitted; registration policy is closed by default and only "
                            "global 'cf' is implicit (parameter 'allowed_vendors', no region "
                            "provides it)"
                        ),
                    }
                ],
                "resource_type": "gts.cf.core.types_registry.entity.v1~",
                "resource_name": schema["gts_id"],
            },
        },
    )
    await assert_absent(registry_http, registry_api_path, schema)


@pytest.mark.skipif(FORCE_ENABLED, reason="TR-REG-902 requires the force-disabled profile")
@pytest.mark.scenario("TR-REG-902")
async def test_disabled_force_rejects_commit_and_dry_run(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A dry run cannot bypass a deployment's disabled force gate."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    await given_registered(first)
    second["force"] = True
    for dry_run in (False, True):
        assert_bad_request(
            await post_registration(
                registry_http, registry_api_path, [second], dry_run=dry_run
            ),
            invalid_argument(
                "force",
                "VALIDATION_FAILED",
                f"force is not permitted on '{second['gts_id']}' in this deployment",
                resource_name=second["gts_id"],
            ),
        )
    await assert_absent(registry_http, registry_api_path, second)
    first_read = await get_entity(
        registry_http, registry_api_path, first["gts_id"], select="content,provenance"
    )
    assert_exact(
        first_read,
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(first),
                "content": first["content"],
                "provenance": provenance(False),
            },
        },
    )


@pytest.mark.skipif(
    not FORCE_ENABLED, reason="TR-REG-903 requires the default force-enabled launcher"
)
@pytest.mark.scenario("TR-REG-903")
async def test_enabled_force_records_waiver_on_only_the_new_minor(
    registry_http, registry_api_path, registration_fixture, given_registered
):
    """A force-enabled minor records a waiver without revising its predecessor."""
    first = registration_fixture("person_minor_0_schema")
    second = registration_fixture("person_minor_1_schema")
    await given_registered(first)
    before = await get_entity(
        registry_http, registry_api_path, first["gts_id"], select="content,provenance"
    )
    assert_exact(
        before,
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(first),
                "content": first["content"],
                "provenance": provenance(False),
            },
        },
    )

    # A typed property at the open `payload` level is incompatible with v1.0,
    # so only the waiver lets v1.1 through.
    second["content"]["properties"]["payload"]["properties"] = {
        "employee_id": {"type": "string"}
    }
    second["force"] = True
    await register_and_assert(
        registry_http, registry_api_path, [second], outcome(second, "succeeded", 1)
    )
    second_read = await get_entity(
        registry_http, registry_api_path, second["gts_id"], select="content,provenance"
    )
    assert_exact(
        second_read,
        {
            "status": 200,
            "etag": "<etag>",
            "body": {
                **mandatory(second),
                "content": second["content"],
                "provenance": provenance(True),
            },
        },
    )
    after = await get_entity(
        registry_http, registry_api_path, first["gts_id"], select="content,provenance"
    )
    assert_exact(
        after,
        {
            "status": 200,
            "etag": before.headers["etag"],
            "body": {
                **mandatory(first),
                "content": first["content"],
                "provenance": provenance(False),
            },
        },
    )
    assert_json(after.json(), before.json())


@pytest.mark.scenario("TR-REG-904")
async def test_configured_region_admits_acme_schema(
    registry_http, registry_api_path, registration_fixture
):
    """The configured acme region admits a schema that reads back unchanged."""
    schema = _acme_schema(registration_fixture("person_schema"), "e2e")
    operation = await register_and_assert(
        registry_http, registry_api_path, [schema], outcome(schema, "succeeded", 1)
    )
    await read_created(registry_http, registry_api_path, schema_entity(schema, 1), operation)
