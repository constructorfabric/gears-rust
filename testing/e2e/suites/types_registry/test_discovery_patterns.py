"""GTS §10 matching examples through HTTP discovery.

IDs below omit the first namespace; `{ns}` names it in later segments. Each test
gets a fresh namespace. Expected IDs are authored explicitly, never matched or
inferred by the helpers. Managed admission forbids UUID tails, instance minors,
instances of major-zero types, and mixed kind/shape within a version family.
"""

import pytest

from .helpers import assert_pages, mandatory, managed, paged, schema_with_id, walk

UUID = "7a1d2f34-5678-49ab-9012-abcdef123456"


class DiscoveryExample:
    """Register authored examples and compare complete discovery pages."""

    def __init__(self, template, register, client, api_path):
        self.template = template
        self.namespace = template["gts_id"].removeprefix("gts.").removesuffix(".person.v1~")
        self.register = register
        self.client = client
        self.api_path = api_path
        self.documents = {}

    def gts_id(self, identifier):
        identifier = identifier.format(ns=self.namespace)
        if identifier.startswith("gts."):
            return identifier
        return f"gts.{self.namespace}.{identifier}"

    async def given(self, *, schemas, instances=()):
        documents = []
        for identifier in schemas:
            gts_id = self.gts_id(identifier)
            document = schema_with_id(self.template, gts_id)
            # A derived type's immediate parent is its preceding type chain.
            parent = gts_id.removesuffix("~").rpartition("~")[0]
            if parent:
                document["content"]["allOf"] = [{"$ref": f"gts://{parent}~"}]
            documents.append(document)
        for identifier in instances:
            documents.append({
                "gts_id": self.gts_id(identifier),
                "content": {"name": "Alice", "payload": {}},
            })
        await self.register(*documents)
        self.documents.update((document["gts_id"], document) for document in documents)

    async def expect(self, *, pattern, expected):
        expected_ids = sorted(self.gts_id(identifier) for identifier in expected)
        expected_entities = [
            {**mandatory(self.documents[gts_id]), "origin": managed(1)}
            for gts_id in expected_ids
        ]
        pages = await walk(
            self.client, self.api_path, {"pattern": self.gts_id(pattern), "limit": 2}
        )
        assert_pages(pages, paged(expected_entities, 2))


@pytest.fixture
def registry(discovery_fixture, given_registered, registry_http, registry_api_path):
    return DiscoveryExample(
        discovery_fixture("person_schema"), given_registered, registry_http, registry_api_path
    )


@pytest.fixture
async def version_examples(registry):
    """Root versions plus a direct instance, derived type and longer chain."""
    await registry.given(
        schemas=[
            "person.v0.0~", "person.v0.1~",
            "person.v1.0~", "person.v1.1~", "person.v1.2~",
            "person.v2~", "person.v10~", "person_extra.v1~",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v2~{ns}.employee.v1.0~",
        ],
        instances=[
            "person.v1.0~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v2~{ns}.alice.v1",
            "person.v10~{ns}.alice.v1",
        ],
    )
    return registry


@pytest.mark.scenario("TR-DISC-104")
@pytest.mark.parametrize("pattern", ["person.v1~", "person.v1~*", "person.v1.*"])
async def test_type_scope_includes_minor_versions_and_descendants(version_examples, pattern):
    """Major 1 includes explicit minors and descendants, excluding major 10 and person_extra."""
    await version_examples.expect(
        pattern=pattern,
        expected=[
            "person.v1.0~", "person.v1.1~", "person.v1.2~",
            "person.v1.0~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-104")
@pytest.mark.parametrize("pattern", ["person.*", "person.v*"])
async def test_family_scope_includes_all_majors_but_not_another_type(version_examples, pattern):
    await version_examples.expect(
        pattern=pattern,
        expected=[
            "person.v0.0~", "person.v0.1~",
            "person.v1.0~", "person.v1.1~", "person.v1.2~", "person.v2~", "person.v10~",
            "person.v1.0~{ns}.alice.v1", "person.v2~{ns}.alice.v1",
            "person.v10~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v2~{ns}.employee.v1.0~",
        ],
    )


@pytest.mark.scenario("TR-DISC-104")
@pytest.mark.parametrize(("pattern", "expected"), [
    pytest.param("person.v2.*", [], id="later-minor-does-not-satisfy-base-minor"),
    pytest.param(
        "person.v2~*",
        ["person.v2~", "person.v2~{ns}.alice.v1", "person.v2~{ns}.employee.v1.0~"],
        id="chain-wildcard-accepts-major-only-base",
    ),
    pytest.param(
        "person.v2~{ns}.*",
        ["person.v2~{ns}.alice.v1", "person.v2~{ns}.employee.v1.0~"],
        id="later-namespace-excludes-base-and-other-majors",
    ),
])
async def test_base_minor_and_chain_wildcards_select_different_scopes(
    version_examples, pattern, expected
):
    """person.v2 has no minor, even though its employee.v1.0 descendant does."""
    await version_examples.expect(pattern=pattern, expected=expected)


@pytest.mark.scenario("TR-DISC-105")
@pytest.mark.parametrize("pattern", ["person.v1.0~", "person.v1.0~*"])
async def test_pinned_base_minor_keeps_descendants_with_other_versions(version_examples, pattern):
    await version_examples.expect(
        pattern=pattern,
        expected=[
            "person.v1.0~",
            "person.v1.0~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-105")
@pytest.mark.parametrize(("pattern", "expected"), [
    pytest.param("person.v0~*", ["person.v0.0~", "person.v0.1~"], id="flexible-minor"),
    pytest.param("person.v0.*", ["person.v0.0~", "person.v0.1~"], id="explicit-minor"),
    pytest.param("person.v0.1~*", ["person.v0.1~"], id="pinned-minor"),
])
async def test_major_zero_uses_the_same_minor_matching_rules(version_examples, pattern, expected):
    await version_examples.expect(pattern=pattern, expected=expected)


@pytest.fixture
async def instance_examples(registry):
    """Named instances, same-name type controls and their longer chains."""
    await registry.given(
        schemas=[
            "person.v1.0~", "person.v1.1~", "person.v1.2~", "person.v2~", "person.v10~",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.2~{ns}.employee.v1~",
        ],
        instances=[
            "person.v1.0~{ns}.alice.v1",
            "person.v1.1~{ns}.alice.v1",
            "person.v1.2~{ns}.alice.v1",
            "person.v1.0~{ns}.alice.v10",
            "person.v1.0~{ns}.alice_extra.v1",
            "person.v2~{ns}.alice.v1",
            "person.v10~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.2~{ns}.employee.v1~{ns}.item.v2",
        ],
    )
    return registry


@pytest.mark.scenario("TR-DISC-106")
@pytest.mark.parametrize(("pattern", "expected"), [
    pytest.param(
        "person.v1~{ns}.alice.v1",
        [
            "person.v1.0~{ns}.alice.v1",
            "person.v1.1~{ns}.alice.v1",
            "person.v1.2~{ns}.alice.v1",
        ],
        id="omitted-base-minor-selects-three-instances",
    ),
    pytest.param("person.v1~{ns}.employee.v1", [], id="instance-excludes-types-and-longer-chains"),
    pytest.param("person.v1~{ns}.employee.v1.0", [], id="pinned-instance-excludes-type"),
    pytest.param(
        "person.v1.0~{ns}.alice.v1", ["person.v1.0~{ns}.alice.v1"],
        id="pinned-base-selects-one-instance",
    ),
    pytest.param("person.v1~{ns}.alice.v1.0", [], id="instance-minor-excludes-major-only-tail"),
    pytest.param("person.v1.0~{ns}.alice.v1.0", [], id="both-minors-pinned-exclude-major-only-tail"),
])
async def test_concrete_instance_selectors_preserve_kind_names_and_chain_length(
    instance_examples, pattern, expected
):
    await instance_examples.expect(pattern=pattern, expected=expected)


@pytest.fixture
async def employee_branches(registry):
    """Two base minors × two derived minors, plus an omitted-derived-minor branch."""
    await registry.given(
        schemas=[
            "person.v1.0~", "person.v1.1~", "person.v1.2~",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.1~{ns}.employee.v1.0~",
            "person.v1.1~{ns}.employee.v1.1~",
            "person.v1.2~{ns}.employee.v1~",
        ],
        instances=[
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.2~{ns}.employee.v1~{ns}.item.v2",
            "person.v1.0~{ns}.alice.v1",
        ],
    )
    return registry


@pytest.mark.scenario("TR-DISC-107")
async def test_later_minor_wildcard_excludes_the_omitted_minor_branch(employee_branches):
    await employee_branches.expect(
        pattern="person.v1~{ns}.employee.v1.*",
        expected=[
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.1~{ns}.employee.v1.0~",
            "person.v1.1~{ns}.employee.v1.1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.1~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-107")
@pytest.mark.parametrize("pattern", ["person.v1~{ns}.employee.v1~*", "person.v1~{ns}.employee.v1~"])
async def test_derived_type_scope_accepts_omitted_and_explicit_minors(employee_branches, pattern):
    await employee_branches.expect(
        pattern=pattern,
        expected=[
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.1~{ns}.employee.v1.0~",
            "person.v1.1~{ns}.employee.v1.1~",
            "person.v1.2~{ns}.employee.v1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.2~{ns}.employee.v1~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-107")
async def test_pinned_derived_minor_keeps_the_base_minor_flexible(employee_branches):
    await employee_branches.expect(
        pattern="person.v1~{ns}.employee.v1.0~",
        expected=[
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.1~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-107")
async def test_later_namespace_selects_direct_instances_and_derived_types(version_examples):
    await version_examples.expect(
        pattern="person.v1~{ns}.*",
        expected=[
            "person.v1.0~{ns}.alice.v1",
            "person.v1.0~{ns}.employee.v1.0~",
            "person.v1.0~{ns}.employee.v1.1~",
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
        ],
    )


@pytest.mark.scenario("TR-DISC-107")
async def test_root_selector_does_not_start_matching_at_a_later_segment(employee_branches):
    await employee_branches.expect(pattern="gts.{ns}.employee.v1~", expected=[])


@pytest.mark.scenario("TR-DISC-108")
@pytest.mark.parametrize(("pattern", "expected"), [
    pytest.param(
        "person.v1~{ns}.employee.v1~{ns}.item.v2",
        [
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.1~{ns}.item.v2",
            "person.v1.2~{ns}.employee.v1~{ns}.item.v2",
        ],
        id="all-type-minors-flexible",
    ),
    pytest.param(
        "person.v1~{ns}.employee.v1.0~{ns}.item.v2",
        [
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.1~{ns}.employee.v1.0~{ns}.item.v2",
        ],
        id="only-derived-minor-pinned",
    ),
    pytest.param(
        "person.v1.0~{ns}.employee.v1~{ns}.item.v2",
        [
            "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
            "person.v1.0~{ns}.employee.v1.1~{ns}.item.v2",
        ],
        id="only-base-minor-pinned",
    ),
    pytest.param(
        "person.v1~{ns}.employee.v1~{ns}.item.v2.0", [],
        id="pinned-instance-minor-excludes-major-only-tail",
    ),
    pytest.param(
        "person.v1.0~{ns}.employee.v1.0~{ns}.item.v2",
        ["person.v1.0~{ns}.employee.v1.0~{ns}.item.v2"],
        id="base-and-derived-minors-pinned",
    ),
])
async def test_three_segment_instance_pins_each_type_minor_independently(
    employee_branches, pattern, expected
):
    await employee_branches.expect(pattern=pattern, expected=expected)


@pytest.mark.scenario("TR-DISC-109")
@pytest.mark.parametrize("pattern", [
    f"person.v1~{UUID}", f"person.v1.0~{UUID}", f"person.v2~{UUID}",
])
async def test_valid_uuid_selector_excludes_named_instances(instance_examples, pattern):
    """Positive UUID-tail matches cannot be set up through managed admission."""
    await instance_examples.expect(pattern=pattern, expected=[])
