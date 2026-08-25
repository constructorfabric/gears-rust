"""What the deployed bss-pricing gear can honestly be asked.

These run against a real binary, behind a real gateway, with a real authn
resolver and a real PDP. That is the half no in-process test can reach: the
routes are mounted, the header contracts hold on the wire, and the PEP is
genuinely in the request path.

# The publication path, and exactly how far it reaches here

Slice 5 mounted the entrance, so the two-person publish workflow is now
reachable end to end and :func:`test_a_material_change_blocks_until_a_second
_principal_has_seen_it` walks it: a material publish opens a pinned approval
unit and answers 202, the submitter's own approve is refused 403
``SELF_APPROVAL_FORBIDDEN`` **and lands a `deny` record on the plan's audit
segment**, an independent principal in the same tenant approves, and the second
publish takes the **commit** arm.

**The commit then stops, and it stops for a reason outside this gear.** There is
no `CatalogVersion` registry gear in this repository, so the module boots with
``UnconfiguredCatalogVersionRegistryV1`` — logged at start-up as *"no
CatalogVersionRegistryV1 registered; publish will fail closed until the registry
gear is wired"* — and ``request_version`` refuses. The commit's whole
transaction rolls back and the caller gets **503**. So the last inch of the walk
below asserts a 503 rather than a receipt, and it is written to prove the arm
was **reached**: a route that had not found the approval would have answered
another 202 and opened a second unit, and a route that had frozen the revision
would have left the plan out of ``draft``. Neither happened. When the registry
gear lands, that one assertion becomes a 200 and a receipt, and nothing else in
this module changes.

The other two blockers this module used to list are gone: the publish route
exists (Slice 5), and the joint conformance fixture registry in this repository
carries ``publish = true`` for every model kind, so the gate is **open** here —
which is why the pre-check below passes rather than refusing ``FIXTURE_MISSING``.

``tests/sqlite_read_model.rs`` still owns the publish → sweep → pin-eligible
projection in process, against a registry double. Asserting a weaker version of
it over HTTP would prove nothing new.
"""

import uuid

import pytest

from .conftest import (
    REVIEWER_PRINCIPAL_ID,
    SUBMITTER_PRINCIPAL_ID,
    grant_window_coverage,
)


def _problem_codes(payload: dict) -> set:
    """Every machine-readable code in an RFC 9457 problem document.

    The code rides either the ``reason`` of an aborted/denied error or a
    precondition violation's ``type``; both spellings are the platform's, so both
    are read rather than one being assumed. §3.3 makes the **code** the
    discriminator a consumer matches on, not the status, which is why every
    refusal below is asserted by code.
    """
    found = set()

    def walk(node):
        if isinstance(node, dict):
            for key in ("reason", "type", "code"):
                value = node.get(key)
                if isinstance(value, str) and value.isupper() and len(value) > 3:
                    found.add(value)
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(payload)
    return found


def _assert_no_effective_policy(client, headers, api_base):
    """The tenant has no threshold policy in force.

    A precondition, not an assertion about the surface. Every case that expects
    `noConfiguredThreshold` depends on it, and it is **not** guaranteed by anything
    the test framework enforces: the one case that configures a policy is last in
    source order and the store is wiped between runs by the run protocol. If either
    changes, this fails where the dependency is, with the version that is in force.
    """
    policy = client.get(f"{api_base}/config/approval-threshold-policy", headers=headers)
    assert policy.status_code == 200, policy.text
    assert policy.json()["effective"] is None, (
        "a threshold policy is in force, so materiality will not answer "
        "`noConfiguredThreshold` here. Either a case that configures one ran before "
        "this one, or a previous run's store survived - the protocol's "
        "`rm -rf ~/.cf-gears/bss-pricing` is what clears it: "
        f"{policy.text}"
    )


def _create_plan(client, headers, key, tier="gold"):
    return client.post(
        "/bss-pricing/v1/plans",
        headers={**headers, "Idempotency-Key": key},
        json={"plan_tier": tier, "billing_cycle": "recurring"},
    )


# ---------------------------------------------------------------------------
# Mounted, and the frontier's empty reading on the wire.
# ---------------------------------------------------------------------------


def test_the_frontier_reads_empty_and_says_so_explicitly(client, auth_headers, api_base):
    """200 with ``pin_eligible: false`` and its two explicit nulls.

    The 404-vs-200 discrimination, through the whole middleware stack. A consumer
    must be able to tell "no publish has ever completed" from "the frontier
    stands at version 0", and this gear's 404 deliberately conflates absent with
    out-of-scope — so the empty reading has to be a 200 carrying its own
    discriminator.
    """
    response = client.get(f"{api_base}/catalog-version/frontier", headers=auth_headers)

    assert response.status_code == 200, response.text
    body = response.json()
    assert body["pin_eligible"] is False
    assert body["catalog_version"] is None
    assert body["advanced_at"] is None


@pytest.mark.parametrize(
    "method,path",
    [
        ("GET", "/bss-pricing/v1/catalog-version/frontier"),
        ("POST", "/bss-pricing/v1/plans"),
    ],
)
def test_an_unauthenticated_request_is_401_on_a_read_and_on_a_write(client, method, path):
    response = client.request(method, path, json={})

    assert response.status_code == 401, response.text


def test_the_nil_tenant_token_is_refused_before_it_reaches_the_pdp(
    client, auth_headers_nil_tenant, api_base
):
    """401, not 403 — and the difference is a property of this gear.

    ``static-authz`` denies the nil-UUID tenant, so on most gears this token is
    the standard way to prove the PEP is in the deployed request path. Here it
    cannot be: ``api::rest::auth_context::require_authenticated`` refuses a
    context whose ``subject_tenant_id`` is nil **before** the gate, because every
    surface of this gear is tenant-scoped and an all-zero tenant is not an
    identity the catalog can serve. So the only token this deployment has that
    the PDP would deny never reaches the PDP.

    That the PEP really is in the path is established instead by
    ``test_tenant_b_cannot_see_tenant_as_plan_…``: tenant B's 404 for a plan
    tenant A can read is only possible if the PDP-compiled ``owner_tenant_id``
    constraint became the SQL filter.
    """
    response = client.get(
        f"{api_base}/catalog-version/frontier", headers=auth_headers_nil_tenant
    )

    assert response.status_code == 401, response.text


# ---------------------------------------------------------------------------
# The authoring round-trip, as one narrative.
# ---------------------------------------------------------------------------


def test_the_authoring_round_trip_holds_on_the_wire(client, auth_headers, idempotency_key):
    """create → read → stale patch → patch → add a price → list → delete → abandon.

    One test rather than eight, because the value is the **sequence**: every step
    consumes a header the previous one emitted, and a broken `ETag` or `Location`
    shows up as the next step failing rather than as an assertion about a string.
    """
    created = _create_plan(client, auth_headers, idempotency_key)
    assert created.status_code == 201, created.text
    plan = created.json()
    plan_id = plan["plan_id"]
    assert created.headers["location"] == f"/bss-pricing/v1/plans/{plan_id}"
    assert created.headers["etag"] == '"0-0"'
    assert plan["lifecycle_state"] == "draft"
    assert plan["revision"] == 0

    # Read it back: the same tag, and the three child sets a PATCH can replace.
    read = client.get(f"/bss-pricing/v1/plans/{plan_id}", headers=auth_headers)
    assert read.status_code == 200, read.text
    assert read.headers["etag"] == '"0-0"'
    # **One phase, not none.** `inst-ph-default` (D-19) mints a terminal evergreen
    # phase inside the creating transaction, and the reason is in `plans.rs`: the
    # `phase` axis of the canonical scope key is always a `phase_id` whose default
    # is the plan's terminal phase, so a plan without one cannot be priced at all -
    # the add-row surface would have nothing to key on. This asserted `[]` until
    # 2026-08-19, which was a state the gear had stopped being able to produce.
    phases = read.json()["phases"]
    assert len(phases) == 1, phases
    assert phases[0]["kind"] == "evergreen"
    assert phases[0]["ordinal"] == 0
    assert phases[0]["converts_to_phase_id"] is None
    # Terminal, so it converts to nothing and has no duration of its own.
    assert phases[0]["phase_duration_days"] is None
    assert read.json()["descriptor_set"] is None

    # A stale precondition is refused by its code, and the row does not move.
    # The tag names its revision as well as its version: `/plans/{planId}` names
    # no revision, so a version alone would be applied to whichever revision the
    # PATCH resolves.
    stale = client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**auth_headers, "If-Match": '"0-9"'},
        json={"shape": {"plan_tier": "platinum"}},
    )
    assert stale.status_code == 409, stale.text
    assert "STALE_VERSION" in _problem_codes(stale.json())

    # The right one advances the tag.
    patched = client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**auth_headers, "If-Match": '"0-0"'},
        json={"shape": {"plan_tier": "platinum"}},
    )
    assert patched.status_code == 200, patched.text
    assert patched.headers["etag"] == '"0-1"'
    assert patched.json()["plan_tier"] == "platinum"

    # A phase, so the price row below has a `phase` axis to sit on.
    phase_id = str(uuid.uuid4())
    phased = client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**auth_headers, "If-Match": '"0-1"'},
        json={
            "phases": [
                {
                    "phase_id": phase_id,
                    "kind": "evergreen",
                    "ordinal": 0,
                    "converts_to_phase_id": None,
                    "phase_duration_days": None,
                    "display_trial_days": None,
                }
            ]
        },
    )
    assert phased.status_code == 200, phased.text
    plan_tag = phased.headers["etag"]

    # A price row on the plan.
    price = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/prices",
        headers={**auth_headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "scope_key": {
                "currency": "USD",
                "region": "EU",
                "phase": phase_id,
                "price_eligibility": "all_subscriptions",
                "charge_kind": "recurring",
                "cohort": None,
            },
            "content": {"model_kind": "flat", "amount_minor": 1500},
        },
    )
    assert price.status_code == 201, price.text
    price_id = price.json()["price_id"]
    assert (
        price.headers["location"]
        == f"/bss-pricing/v1/plans/{plan_id}/prices/{price_id}"
    )
    assert price.headers["etag"] == '"0"'

    # The list, in the platform's `Page` envelope.
    listed = client.get(
        f"/bss-pricing/v1/plans/{plan_id}/prices", headers=auth_headers
    )
    assert listed.status_code == 200, listed.text
    page = listed.json()
    assert [row["price_id"] for row in page["items"]] == [price_id]
    assert page["page_info"]["next_cursor"] is None
    assert page["page_info"]["prev_cursor"] is None
    assert page["page_info"]["limit"] == 100

    # Delete under its own tag - the price row's, never the plan's (D-141).
    deleted = client.delete(
        f"/bss-pricing/v1/plans/{plan_id}/prices/{price_id}",
        headers={**auth_headers, "If-Match": '"0"'},
    )
    assert deleted.status_code == 204, deleted.text

    # Abandon the plan's draft: it flips, it is never deleted.
    abandoned = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/abandon",
        headers={**auth_headers, "If-Match": plan_tag},
    )
    assert abandoned.status_code == 200, abandoned.text
    assert abandoned.json()["lifecycle_state"] == "abandoned"


def test_a_blind_delete_is_refused_and_the_row_survives(client, auth_headers):
    """D-141's defect, on the wire.

    Before it, this verb's idempotency cell was empty and a draft row could be
    destroyed under an unknown version. What a blind delete destroys is a
    concurrent editor's uncommitted work, not the row.
    """
    plan = _create_plan(client, auth_headers, str(uuid.uuid4())).json()
    plan_id = plan["plan_id"]
    phase_id = str(uuid.uuid4())
    client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**auth_headers, "If-Match": '"0-0"'},
        json={
            "phases": [
                {
                    "phase_id": phase_id,
                    "kind": "evergreen",
                    "ordinal": 0,
                    "converts_to_phase_id": None,
                    "phase_duration_days": None,
                    "display_trial_days": None,
                }
            ]
        },
    )
    price = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/prices",
        headers={**auth_headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "scope_key": {
                "currency": "USD",
                "region": "EU",
                "phase": phase_id,
                "price_eligibility": "all_subscriptions",
                "charge_kind": "recurring",
                "cohort": None,
            },
            "content": {"model_kind": "flat", "amount_minor": 900},
        },
    ).json()

    blind = client.delete(
        f"/bss-pricing/v1/plans/{plan_id}/prices/{price['price_id']}",
        headers=auth_headers,
    )
    assert blind.status_code == 400, blind.text

    survivor = client.get(
        f"/bss-pricing/v1/plans/{plan_id}/prices", headers=auth_headers
    )
    assert [row["price_id"] for row in survivor.json()["items"]] == [price["price_id"]]


# ---------------------------------------------------------------------------
# Idempotency: a header contract, so only provable end to end.
# ---------------------------------------------------------------------------


def test_the_same_key_and_body_replays_the_same_plan_id(
    client, auth_headers, idempotency_key
):
    """The property that is *only* provable over HTTP, because it is a header
    contract: the caller sends one header twice and must be handed the same id."""
    first = _create_plan(client, auth_headers, idempotency_key)
    assert first.status_code == 201, first.text

    replay = _create_plan(client, auth_headers, idempotency_key)

    assert replay.status_code == 201, replay.text
    assert replay.json()["plan_id"] == first.json()["plan_id"]


def test_the_same_key_with_a_different_body_is_refused_by_its_code(
    client, auth_headers, idempotency_key
):
    _create_plan(client, auth_headers, idempotency_key, tier="gold")

    clash = _create_plan(client, auth_headers, idempotency_key, tier="platinum")

    assert clash.status_code == 409, clash.text
    assert "IDEMPOTENCY_PAYLOAD_MISMATCH" in _problem_codes(clash.json())


def test_a_create_without_the_header_is_refused(client, auth_headers):
    response = client.post(
        "/bss-pricing/v1/plans", headers=auth_headers, json={"plan_tier": "gold"}
    )

    assert response.status_code == 400, response.text


# ---------------------------------------------------------------------------
# Cross-tenant isolation, genuinely.
# ---------------------------------------------------------------------------


def test_tenant_b_cannot_see_tenant_as_plan_and_gets_the_same_answer_as_for_a_random_id(
    client, auth_headers, auth_headers_tenant_b, idempotency_key
):
    """The owner's 200 is the baseline.

    Without it the 404 below would be consistent with the plan simply not
    existing, and the test would prove nothing about isolation.
    """
    created = _create_plan(client, auth_headers, idempotency_key)
    assert created.status_code == 201, created.text
    plan_id = created.json()["plan_id"]

    owner = client.get(f"/bss-pricing/v1/plans/{plan_id}", headers=auth_headers)
    assert owner.status_code == 200, owner.text
    assert owner.json()["plan_id"] == plan_id

    foreign = client.get(
        f"/bss-pricing/v1/plans/{plan_id}", headers=auth_headers_tenant_b
    )
    absent = client.get(
        f"/bss-pricing/v1/plans/{uuid.uuid4()}", headers=auth_headers_tenant_b
    )

    assert foreign.status_code == 404, foreign.text
    assert absent.status_code == 404, absent.text
    assert _problem_codes(foreign.json()) == _problem_codes(absent.json())


# ---------------------------------------------------------------------------
# The status-rendering rule, where a consumer would discover it.
# ---------------------------------------------------------------------------


def test_a_design_set_422_arrives_as_a_400_carrying_its_code(
    client, auth_headers, idempotency_key
):
    """§3.3 states it once for the whole design set; this is where a consumer
    would find out.

    ``LIFECYCLE_FORBIDDEN`` is annotated 422 in the documents. The platform's
    canonical family has no 422 category at all, so it reaches the wire as a
    **400 carrying its code** — and the code, not the status, is what a consumer
    matches on. A plan holding no open draft revision is the cheapest way to
    provoke it: abandon it once, then abandon it again.
    """
    plan_id = _create_plan(client, auth_headers, idempotency_key).json()["plan_id"]
    first = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/abandon",
        headers={**auth_headers, "If-Match": '"0-0"'},
    )
    assert first.status_code == 200, first.text

    again = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/abandon",
        headers={**auth_headers, "If-Match": '"0-1"'},
    )

    assert again.status_code == 400, again.text
    assert "PLAN_ABANDONED_NO_SUCCESSOR" in _problem_codes(again.json()), again.text


# ---------------------------------------------------------------------------
# The entrance: a material change, two principals, and the audit trail.
# ---------------------------------------------------------------------------


def _assemble_publishable_shape(client, headers):
    """A plan whose **shape** the publish rule set passes, carrying no price row.

    Every value here is load-bearing, and each one was found by driving the
    route rather than by reading the rules:

    * ``sku_id``, ``plan_tier``, ``billing_cycle`` and ``frequency`` — the
      Foundation shape rules run at publish, not at save;
    * one **evergreen** phase, because the ``phase`` axis of a price row's
      scope key must resolve to a phase the revision holds;
    * a descriptor set, which Billing's hand-off requires.

    Split out of :func:`_assemble_publishable_plan` rather than copied, because a
    rowless plan is a state the rule set genuinely admits — the coverage rule ranges
    over the *billable* set and an empty set holds no key to find uncovered — and it
    is the world :func:`test_an_empty_first_publish_stops_at_the_absent_registry`
    drives.

    Returns ``(plan_id, phase_id, etag)`` where the tag names the open draft revision
    and its version, which is what ``POST .../publish`` swaps on.
    """
    phase_id = str(uuid.uuid4())
    created = client.post(
        "/bss-pricing/v1/plans",
        headers={**headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "sku_id": str(uuid.uuid4()),
            "plan_tier": "gold",
            "billing_cycle": "recurring",
            "frequency": {"kind": "monthly"},
        },
    )
    assert created.status_code == 201, created.text
    plan_id = created.json()["plan_id"]

    phased = client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**headers, "If-Match": '"0-0"'},
        json={
            "phases": [
                {
                    "phase_id": phase_id,
                    "kind": "evergreen",
                    "ordinal": 0,
                    "converts_to_phase_id": None,
                    "phase_duration_days": None,
                    "display_trial_days": None,
                }
            ]
        },
    )
    assert phased.status_code == 200, phased.text

    described = client.patch(
        f"/bss-pricing/v1/plans/{plan_id}",
        headers={**headers, "If-Match": phased.headers["etag"]},
        json={
            "descriptor_set": {
                "invoice_line_template": "{plan}",
                "gl_code": "4000",
                "itemization_rule": "per_charge",
                "additional": {},
            }
        },
    )
    assert described.status_code == 200, described.text

    return plan_id, phase_id, described.headers["etag"]


def _assemble_publishable_plan(client, headers):
    """:func:`_assemble_publishable_shape`, plus the one billable row and its window.

    The two remaining load-bearing values, both found by driving the route:

    * ``rounding_policy_ref`` on the row. Without it the pre-check refuses
      ``ROUNDING_POLICY_UNRESOLVED`` — rounding decides the last minor unit of
      every charge and no tenant default policy can be configured here — and the
      publish never reaches materiality at all;
    * a **window** on the row's canonical scope key. Same consequence, different
      rule: ``inst-wc-required`` refuses ``WINDOW_COVERAGE_MISSING`` for a billable
      row with no active or scheduled window, and a plan that cannot publish never
      reaches the two-person workflow this module exists to walk.

    Returns ``(plan_id, etag)``.
    """
    plan_id, phase_id, etag = _assemble_publishable_shape(client, headers)

    priced = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/prices",
        headers={**headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "scope_key": {
                "currency": "USD",
                "region": "EU",
                "phase": phase_id,
                "price_eligibility": "all_subscriptions",
                "charge_kind": "recurring",
                "cohort": None,
            },
            "content": {
                "model_kind": "flat",
                "amount_minor": 1500,
                "rounding_policy_ref": "half_up",
                "billing_timing": "advance",
                "tax_inclusive": False,
                # `inst-pi-required`: a recurring row MUST publish all three.
                # Subscriptions computes proration from the frozen values and the
                # catalog substitutes no defaults, so a row without them is one no
                # consumer can prorate. Absent here, every publish below answered
                # `PRORATION_INPUTS_MISSING` (400) instead of reaching the arm it
                # was written for — including the window test, whose
                # `WINDOW_COVERAGE_MISSING` was masked by this earlier refusal.
                # The Rust tier had the same fixture defect and D-344 fixed it
                # there (`rest_cutovers.rs`): the fixture was authoring an
                # unpublishable row, not the rule refusing a legal one.
                "billing_anchor_policy": "calendar_month",
                "proration_basis": "calendar_days_actual",
                "credit_on_downgrade": False,
            },
        },
    )
    assert priced.status_code == 201, priced.text
    price_id = priced.json()["price_id"]

    # `inst-wc-required` (S7 §3): the row does not publish until its canonical
    # scope key holds an active or scheduled `PriceWindow`. Without this the two
    # publishing tests below are refused `WINDOW_COVERAGE_MISSING` at the submit
    # arm and never reach materiality, the approval unit or the commit — which is
    # the rule working, and is asserted as such by
    # `test_a_billable_row_with_no_window_is_refused_on_the_wire`.
    #
    # Scheduled in the deployment's own store rather than over HTTP, and **not**
    # because the route is unmounted — it is mounted, and
    # `test_the_window_route_is_mounted_and_requires_its_idempotency_key` drives it.
    # A window mutation needs the plan's *current* revision, and on this deployment
    # no plan ever holds one: the commit's version request fails closed with no
    # registry gear (`test_an_empty_first_publish_stops_at_the_absent_registry`). See
    # `conftest.grant_window_coverage` for the whole argument and what deletes it.
    grant_window_coverage(price_id)

    return plan_id, etag


def _submit_for_publish(client, headers, plan_id, etag):
    """Take the submit arm and hand back the unit it opened."""
    response = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/publish",
        headers={**headers, "If-Match": etag},
    )
    assert response.status_code == 202, response.text
    return response.json()


def test_a_material_change_blocks_until_a_second_principal_has_seen_it(
    client, auth_headers, auth_headers_reviewer, audit_segment, api_base
):
    """The whole reason Slice 5 exists, on the wire, as one narrative.

    One test rather than five, because the value is the **sequence**: the
    approval id comes from the 202, the 403 is only meaningful against a unit
    that is genuinely pending, the audit record is only evidence if the refusal
    that wrote it was the one just made, and the commit arm is only reachable if
    the approve before it landed. Split into five, each would re-stage the four
    steps before it and assert against its own private world.
    """
    _assert_no_effective_policy(client, auth_headers, api_base)
    plan_id, etag = _assemble_publishable_plan(client, auth_headers)

    # 1. A material publish does not publish. Every publish is material here and
    #    that is by rule: this tenant has no approval-threshold policy in force
    #    (asserted just above, because it is a precondition and not a property of
    #    this deployment), so `inst-mat-failsafe` answers `noConfiguredThreshold`
    #    for every call - and configuring the policy that would change that is
    #    itself an always-material act (D-10).
    submitted = _submit_for_publish(client, auth_headers, plan_id, etag)
    assert submitted["outcome"] == "submitted_for_approval"
    assert submitted["materiality"] == {
        "material": True,
        "reason": "noConfiguredThreshold",
        # D-187: the evaluator now stores which row reached which currency's bar. The
        # fail-safe is an answer about the *policy*, so no row tripped and the field is
        # null - asserted rather than dropped, because a row named here would be a row
        # put in front of a reviewer as though it had tripped something.
        "tripped": None,
        # `null` for the same reason `tripped` is: the fail-safe answers about the
        # policy's absence, so no registered trigger fired. Asserted rather than
        # omitted - a trigger named here would be an act attributed to a rule that
        # never ran.
        "trigger": None,
    }
    assert submitted["receipt"] is None, "nothing was frozen"
    unit = submitted["approval"]
    approval_id = unit["approval_id"]
    assert unit["state"] == "submitted"
    assert unit["subject_ref"] == f"{plan_id}/0"
    assert unit["submitter_principal"] == SUBMITTER_PRINCIPAL_ID
    assert unit["approver_principal"] is None

    # 2. D-61: the reviewer reads what they are being asked to sign for, not a
    #    digest. A hash-blind approve certifies only that somebody clicked.
    detail = client.get(
        f"/bss-pricing/v1/approvals/{approval_id}", headers=auth_headers_reviewer
    )
    assert detail.status_code == 200, detail.text
    detail = detail.json()
    assert detail["content_matches_pin"] is True
    assert detail["pinned_content"]["plan_id"] == plan_id
    assert [row["scope_key"]["region"] for row in detail["pinned_content"]["rows"]] == [
        "EU"
    ]

    # 3. The submitter's own approve is refused on IDENTITY, holding every role
    #    the surface asks for — the token's PDP decision is `true` for
    #    `approval x approve`, so the only thing standing in the way is that the
    #    two principals are the same person.
    self_approval = client.post(
        f"/bss-pricing/v1/approvals/{approval_id}/approve", headers=auth_headers
    )
    assert self_approval.status_code == 403, self_approval.text
    assert "SELF_APPROVAL_FORBIDDEN" in _problem_codes(self_approval.json())

    # ...and the refusal did not merely bounce: `inst-tp-selfaudit` requires the
    # ATTEMPT on the trail, because an authority violation nobody can see is not
    # governed. This is the assertion no in-process suite can make about a
    # deployed binary, and it is read from the deployment's own store because the
    # gear serves no audit route.
    denials = [row for row in audit_segment(plan_id) if row["action"] == "deny"]
    assert len(denials) == 1, "exactly one refusal was attempted"
    denial = denials[0]
    assert uuid.UUID(bytes=denial["actor_principal_id"]) == uuid.UUID(
        SUBMITTER_PRINCIPAL_ID
    )
    assert uuid.UUID(bytes=denial["approval_ref"]) == uuid.UUID(approval_id)
    assert "SELF_APPROVAL_FORBIDDEN" in denial["after_state"]

    # The record it names is untouched: a refused decision is not a decision.
    still_pending = client.get(
        f"/bss-pricing/v1/approvals/{approval_id}", headers=auth_headers
    ).json()["approval"]
    assert still_pending["state"] == "submitted"
    assert still_pending["approver_principal"] is None

    # 4. A different principal in the same tenant decides it.
    approved = client.post(
        f"/bss-pricing/v1/approvals/{approval_id}/approve",
        headers=auth_headers_reviewer,
    )
    assert approved.status_code == 200, approved.text
    approved = approved.json()
    assert approved["state"] == "approved"
    assert approved["approver_principal"] == REVIEWER_PRINCIPAL_ID
    assert approved["submitter_principal"] == SUBMITTER_PRINCIPAL_ID

    # 5. The second publish takes the COMMIT arm — and stops at the absent
    #    registry gear. See this module's header: `request_version` refuses
    #    `Unconfigured`, the commit's whole transaction rolls back, and the
    #    caller gets 503. The three assertions after it are what make this a
    #    proof that the arm was reached rather than an accepted failure.
    published = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": etag},
    )
    assert published.status_code == 503, published.text

    #    (0) the 503 is not the PEP's. A PDP outage fails closed as a 503 too,
    #        and it would fire BEFORE the route did anything - leaving exactly
    #        the three traces (a)-(c) look for. So the gate is re-asked here, on
    #        the same route with the same token, under a tag that must be
    #        answered by a guard AFTER it: `publish_scope` runs first, then
    #        `require_same_revision`. A 409 naming STALE_VERSION means
    #        `plan x publish` is being ALLOWED right now, which leaves the
    #        registry as the only thing the 503 above can be.
    #
    #        The tag names a different REVISION, not merely a different version.
    #        `require_same_revision` compares only the revision; the version half
    #        rides through to the commit's compare-and-swap, so `"0-99"` reaches
    #        the registry and answers 503 as well - which would have made this a
    #        second reading of the same failure rather than a discriminator.
    gate_is_open = client.post(
        f"/bss-pricing/v1/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": '"9-0"'},
    )
    assert gate_is_open.status_code == 409, gate_is_open.text
    assert "STALE_VERSION" in _problem_codes(gate_is_open.json())

    #    (a) it did not take the submit arm: no second unit was opened over this
    #        subject, which a route that had not matched the approval would have
    #        done;
    units = client.get(
        "/bss-pricing/v1/approvals", headers=auth_headers, params={"limit": 1000}
    ).json()["items"]
    mine = [row for row in units if row["subject_ref"] == f"{plan_id}/0"]
    assert [row["approval_id"] for row in mine] == [approval_id]

    #    (b) the commit rolled back whole: the revision is still an open draft
    #        under the same tag, so the fail-closed refusal froze nothing;
    plan = client.get(f"/bss-pricing/v1/plans/{plan_id}", headers=auth_headers)
    assert plan.status_code == 200, plan.text
    assert plan.json()["lifecycle_state"] == "draft"
    assert plan.headers["etag"] == etag

    #    (c) and no publish record reached the trail.
    assert [row["action"] for row in audit_segment(plan_id)].count("publish") == 0

    #    The approve itself IS on the trail, by the independent principal.
    approvals = [row for row in audit_segment(plan_id) if row["action"] == "approve"]
    assert len(approvals) == 1
    assert uuid.UUID(bytes=approvals[0]["actor_principal_id"]) == uuid.UUID(
        REVIEWER_PRINCIPAL_ID
    )


def test_the_frontier_still_reads_empty_because_nothing_can_publish_here(
    client, auth_headers, api_base
):
    """The consequence of the registry's absence, stated where it is observable.

    Distinct from :func:`test_the_frontier_reads_empty_and_says_so_explicitly`,
    which asserts the empty reading's *shape* on a fresh deployment. This one
    asserts the reading is **still** empty after the walk above drove a plan all
    the way to the commit arm — so a future change that let a publish through
    without a registry would redden here rather than pass silently.
    """
    plan_id, etag = _assemble_publishable_plan(client, auth_headers)
    _submit_for_publish(client, auth_headers, plan_id, etag)

    frontier = client.get(f"{api_base}/catalog-version/frontier", headers=auth_headers)

    assert frontier.status_code == 200, frontier.text
    assert frontier.json()["pin_eligible"] is False
    assert frontier.json()["catalog_version"] is None


# ---------------------------------------------------------------------------
# Slice 7: the coverage rule, and the report an operator remediates from.
# ---------------------------------------------------------------------------


def test_a_first_publish_covers_its_own_key_rather_than_being_refused(
    client, auth_headers, api_base, idempotency_key
):
    """**D-332 on the wire**: a key this publish freezes first is covered by it.

    This test asserted the opposite until 2026-08-19 - a 400 carrying
    ``WINDOW_COVERAGE_MISSING`` - and that was the rule before D-332 narrowed it.
    `coverage.rs` states the narrowing and its reason: the rules run on **both**
    the submit and the commit arm, so refusing a first-freeze key at submit means
    the approval unit never opens and the commit that would have opened the window
    is never reached. The carve-out is what makes any publish here possible at all.

    `inst-wc-required` keeps its whole force where it means something - a key
    carrying an **already published** row has had coverage and lost it, which is an
    author's mistake rather than an artefact of ordering. That case is
    unreachable on this deployment and is not silently dropped: see the note below.

    So the assertion is the carve-out itself, stated so that reverting D-332
    reddens it: the publish is accepted, and **no** coverage violation is raised
    for a plan that holds no window at all.

    # Why the refused half cannot be tested here

    It needs a row that is already published, and nothing on this deployment ever
    publishes: no ``CatalogVersionRegistryV1`` implementation exists in this
    repository and ``config/e2e-local.yaml`` exposes no key that would supply one,
    so every commit arm fails closed at the version request - which
    :func:`test_an_empty_first_publish_stops_at_the_absent_registry` drives to its
    503. The in-crate suites own that half.
    """
    plan_id, etag = _assemble_publishable_plan_without_coverage(
        client, auth_headers, api_base, idempotency_key
    )

    accepted = client.post(
        f"{api_base}/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": etag},
    )

    assert accepted.status_code == 202, accepted.text
    body = accepted.json()
    assert body["outcome"] == "submitted_for_approval", body
    # The plan holds no `PriceWindow`; the point is that this is not a refusal.
    # Asserted against the whole response text rather than a parsed violation list,
    # because a 202 carries no violations and the absence has to be checked
    # somewhere a future refusal would show up.
    assert "WINDOW_COVERAGE_MISSING" not in accepted.text, accepted.text


def test_the_window_route_is_mounted_and_requires_its_idempotency_key(
    client, auth_headers, api_base, idempotency_key
):
    """`POST /prices/{priceId}/windows` is mounted, and D-171's header is live.

    The e2e's only direct contact with a §5 window mutation, and it is on the one
    property this deployment can actually show. A **400** here is the load-bearing
    part: an unmounted path answers 404, and the gear reserves its whole
    ``/bss-pricing/v1`` prefix with a 404 for exactly that reason
    (``module.rs``) — so 404 could never distinguish "mounted and refusing" from
    "never mounted", while a 400 about a missing header can only come from a handler
    that ran. It is also what pins ``conftest.grant_window_coverage``'s **corrected**
    premise: that fixture used to say the route was not mounted, and this is the
    assertion that says it is.

    The key is read before the price row is resolved, deliberately (``windows.rs``),
    so this passes without the plan holding anything publishable — which matters,
    because on this deployment it could not.
    """
    plan_id, _etag = _assemble_publishable_plan_without_coverage(
        client, auth_headers, api_base, idempotency_key
    )
    price_id = _the_one_price_id(client, auth_headers, api_base, plan_id)

    refused = client.post(
        f"{api_base}/prices/{price_id}/windows",
        headers=auth_headers,
        json={
            "effective_from": "2099-08-04T00:00:00Z",
            "effective_to": None,
            "reason_code": "e2eCoverage",
        },
    )

    assert refused.status_code == 400, (
        "a mounted handler refuses the absent Idempotency-Key; a 404 would mean the "
        f"route is not there at all: {refused.text}"
    )
    assert "Idempotency-Key" in refused.text, refused.text


def test_a_plan_with_no_current_revision_has_no_window_surface(
    client, auth_headers, api_base, idempotency_key
):
    """The window surface's **precondition**, on the wire: current means published.

    ``WindowService`` resolves the plan's *current* revision and current means
    ``published | retired`` (``LifecycleState::is_current_revision``), so a plan whose
    only revision is a ``draft`` gets **404 ``current plan revision``** from all three
    §5 surfaces. Asserted with the request otherwise complete — valid body, valid
    ``Idempotency-Key``, a price row that exists, a caller the PDP allows — so the 404
    can only be the precondition's.

    **This test used to claim more than it checks, and the claim was false.** Its name
    was ``..._the_first_window_of_a_plan_cannot_be_authored_through_the_route`` and its
    docstring read *"so this plan can never publish, and neither can any other"*. The
    body was always honest about its own scenario; the generalisation was not. A plan
    with **no price row** publishes — ``coverage::check`` ranges over the billable set,
    so there is no key to find uncovered, and there is no minimum-row rule — which
    leaves a current revision for a window mutation to freeze. The in-crate
    ``rest_windows.rs::a_plans_first_window_is_authorable_through_the_routes_after_an_empty_publish``
    executes the whole sequence through the mounted routes and ends on a 202.

    What *this* deployment cannot do is take that sequence, and the reason is a
    different one: it has no ``CatalogVersionRegistryV1``, so no publish of any kind
    completes here — :func:`test_an_empty_first_publish_stops_at_the_absent_registry`.
    That, and not a deadlock, is why ``conftest.grant_window_coverage`` still writes
    into the store.
    """
    plan_id, _etag = _assemble_publishable_plan_without_coverage(
        client, auth_headers, api_base, idempotency_key
    )
    price_id = _the_one_price_id(client, auth_headers, api_base, plan_id)

    refused = client.post(
        f"{api_base}/prices/{price_id}/windows",
        headers={**auth_headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "effective_from": "2099-08-04T00:00:00Z",
            "effective_to": None,
            "reason_code": "e2eCoverage",
        },
    )

    assert refused.status_code == 404, (
        "a draft-only plan has no current revision to freeze - see this test's "
        f"docstring: {refused.text}"
    )
    assert "current plan revision" in refused.text, (
        "the 404 must be the precondition's and not a routing miss or an absent "
        f"price row: {refused.text}"
    )


def test_an_empty_first_publish_stops_at_the_absent_registry(
    client, auth_headers, auth_headers_reviewer, api_base
):
    """**Why the window fixture cannot move onto the route on this deployment.**

    The sequence a plan's first window is authored by — an empty publish, then the row,
    then the window — is driven here through the mounted surfaces as far as it goes:
    submit (**202**), an independent approve (**200**), the commit (**503**). The
    rowless plan is a real publish candidate, which is the half worth executing: the
    submit arm accepts it, opens a unit and returns a materiality verdict, so nothing
    about "no price rows" is refused on the way in.

    Then it stops, and not on anything this group could fix: there is no
    ``CatalogVersionRegistryV1`` gear in this repository and **no config key that would
    supply one** (``config/e2e-local.yaml``), so the commit's version request fails
    closed. The plan therefore never reaches ``published``, never holds a current
    revision, and no window on it can be authored through a route — which is
    ``conftest.grant_window_coverage``'s true and only remaining reason to exist.

    **When the registry gear lands, this test's 503 becomes a 200 and the fixture is
    deleted**: at that point the sequence completes here exactly as it already does
    in-crate, and every caller of the fixture posts instead.
    """
    plan_id, _phase_id, etag = _assemble_publishable_shape(client, auth_headers)

    submitted = client.post(
        f"{api_base}/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": etag},
    )
    assert submitted.status_code == 202, (
        f"a plan with no price row is a publish candidate, not a refusal: {submitted.text}"
    )
    assert submitted.json()["outcome"] == "submitted_for_approval", submitted.text
    approval_id = submitted.json()["approval"]["approval_id"]

    approved = client.post(
        f"{api_base}/approvals/{approval_id}/approve",
        headers=auth_headers_reviewer,
    )
    assert approved.status_code == 200, approved.text

    committed = client.post(
        f"{api_base}/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": etag},
    )
    assert committed.status_code == 503, (
        "a 200 here means the registry gear landed: delete conftest.grant_window_coverage "
        f"and post the window instead: {committed.text}"
    )

    # And the consequence the fixture rests on, read back rather than inferred.
    described = client.get(f"{api_base}/plans/{plan_id}", headers=auth_headers)
    assert described.status_code == 200, described.text
    assert described.json()["lifecycle_state"] == "draft", (
        f"nothing froze, so the plan holds no current revision: {described.text}"
    )


def test_the_coverage_report_answers_for_the_key_the_refusal_named(
    client, auth_headers, api_base, idempotency_key
):
    """``GET .../coverage`` is mounted in the real deployment, and agrees with it.

    Two facts at once, and the first is not a formality: a route can be registered,
    declared in the census and catalogued for authz while being mounted **nowhere**
    — that was live in this gear on 2026-08-04 and every in-process census stayed
    green through it. Only a request to a running binary settles it.

    The second is the surface's whole purpose: the key the report calls uncovered is
    the key the refusal named, character for character, so an operator can act on
    one from the other.
    """
    plan_id, _etag = _assemble_publishable_plan_without_coverage(
        client, auth_headers, api_base, idempotency_key
    )

    # The key used to be read out of a `WINDOW_COVERAGE_MISSING` violation, which
    # is what made "the report names the key the refusal named" checkable. D-332
    # removed that refusal for a first-freeze key (see
    # :func:`test_a_first_publish_covers_its_own_key_rather_than_being_refused`),
    # and no surviving refusal on this deployment carries a **scope key** as its
    # subject - the proration one names the price row. The cross-check is
    # therefore withdrawn rather than faked, and what remains is the report's own
    # two facts, which are the ones the docstring calls load-bearing: it is really
    # mounted, and its `covered` flag tracks the window rather than answering
    # `False` for everything.
    report = client.get(f"{api_base}/plans/{plan_id}/coverage", headers=auth_headers)
    assert report.status_code == 200, report.text
    body = report.json()
    assert body["plan_id"] == plan_id
    assert len(body["keys"]) == 1, body
    reported_key = body["keys"][0]["scope_key"]
    assert reported_key.startswith(plan_id), (
        f"the report names the plan's own key, not another: {reported_key}"
    )

    entry = body["keys"][0]
    assert entry["required"] is True
    assert entry["covered"] is False
    assert entry["coverage_end"] == {"kind": "uncovered", "at": None}
    assert entry["intervals"] == []
    assert entry["interior_gaps"] == []

    # And the same report after the key is covered, so `covered: False` above is
    # about the window and not about the surface answering `False` for everything.
    price_id = _the_one_price_id(client, auth_headers, api_base, plan_id)
    grant_window_coverage(price_id)

    covered = client.get(f"{api_base}/plans/{plan_id}/coverage", headers=auth_headers)
    assert covered.status_code == 200, covered.text
    entry = covered.json()["keys"][0]
    assert entry["scope_key"] == reported_key
    assert entry["covered"] is True
    assert entry["coverage_end"] == {"kind": "open_ended", "at": None}
    assert len(entry["intervals"]) == 1
    # `scheduled` is a fact here and used to be a race: the fixture's window began
    # at the price row's own `created_at_utc`, so it was already due at boot and the
    # `WindowActivationJob` ticker flipped it within a minute of a run that takes
    # thirteen seconds. `conftest.grant_window_coverage` dates it 2099 for that
    # reason.
    assert entry["intervals"][0]["state"] == "scheduled"
    assert entry["intervals"][0]["effective_to"] is None
    assert entry["interior_gaps"] == []


# ---------------------------------------------------------------------------
# Slice 5 / G5: the sellability surface, and the two traps that are only at the
# wire.
# ---------------------------------------------------------------------------
#
# The market every sellability request below is about, spelled once so a query
# string and the plan's one price row cannot disagree about it. It is deliberately
# the row's own market: on this deployment the answer does not depend on it (see
# the case's docstring), but on a deployment where a publish completes, a case
# asking about a market the plan binds no key on would read `not_sellable` for a
# reason that has nothing to do with what it is testing.
SELLABILITY_CURRENCY = "USD"
SELLABILITY_REGION = "EU"

# One instant, in the two spellings RFC 3339 allows for it. They name the **same
# moment**; only one of them survives a query string, which is the whole of
# `test_the_sellability_surface_holds_its_contract_…`'s second property.
AT_UTC_DESIGNATOR = "2099-08-04T12:00:00Z"
AT_NUMERIC_OFFSET = "2099-08-04T12:00:00+00:00"


def _sellability_query(at: str) -> str:
    """The three required parameters as a **raw** query string.

    Raw on purpose, and it is load-bearing rather than a shortcut: `httpx`'s
    ``params=`` percent-encodes a ``+`` to ``%2B``, which arrives at the handler
    intact and would make :data:`AT_NUMERIC_OFFSET` *pass*. Put in the URL
    literally, the ``+`` reaches the server as a ``+``, form-urlencoding decodes it
    to a space, and the refusal this module asserts is the one a real client
    written the obvious way actually meets.
    """
    return f"at={at}&currency={SELLABILITY_CURRENCY}&region={SELLABILITY_REGION}"


def _json_booleans(node, path="") -> list:
    """Every JSON boolean in ``node``, by the path it sits at.

    The e2e twin of ``rest_windows.rs``'s ``collect_booleans``. A sweep of the
    **whole** document rather than of the field a reader expects one in, because
    the failure it guards against is a boolean arriving somewhere nobody thought
    to look.
    """
    found = []
    if isinstance(node, bool):
        found.append(path or "<root>")
    elif isinstance(node, dict):
        for key, value in node.items():
            found.extend(_json_booleans(value, f"{path}.{key}"))
    elif isinstance(node, list):
        for index, item in enumerate(node):
            found.extend(_json_booleans(item, f"{path}[{index}]"))
    return found


def test_the_sellability_surface_holds_its_contract_and_publishes_no_boolean(
    client, auth_headers, api_base, idempotency_key
):
    """`GET .../sellability` on the wire: what it refuses, and what it publishes.

    Four properties in one case: two about what the surface **refuses**, and two
    about the **one document** an accepted request answers with — so the second pair
    costs one request between them rather than one each. The refusals come first
    because ``market_of`` runs before the authz gate, which is what makes them
    assertions about the parameter contract rather than about this caller's grants.

    1. **All three query parameters are required**, each refused by name. `at` is
       required rather than defaulted to this server's clock, deliberately: the
       contract of this surface is that the instant is the *caller's*, and a
       defaulted one would put the server's clock inside an answer the caller reads
       as being about their own moment (``market_of``). Asserted by equality on the
       problem document's ``detail``, which names the parameter.
    2. **A numeric offset is refused rather than repaired.** `+` is a *space* under
       form-urlencoding, so `…+00:00` written literally in a query string arrives as
       `…00:00` with a space where the sign was — not an instant in any format. Four
       in-crate cases answered 400 on exactly this before their helper switched to
       the `Z` form, and it is pinned in-crate by
       ``an_unencoded_offset_instant_is_refused_rather_than_guessed_at``. The two
       spellings here name the **same moment**, so the 400 can only be about the
       encoding: :data:`AT_UTC_DESIGNATOR` answers 200 in the same test.
    3. **There is no boolean anywhere in the document, at any depth.** What the
       surface publishes is intervals, states, a derived coverage end and
       per-predicate tokens; a boolean would freeze an answer to a question about
       the *reader's* clock into a store whose contract is that a completed version
       never changes (D-99). What this adds over the in-crate sweep is the
       **deployed** serializer through the real gateway; what it does **not** add is
       reach, and that limit is stated rather than left to be assumed: on this stand
       ``keys`` is empty, so the sweep here covers the plan-level half of the
       document only and a boolean introduced inside a per-key entry would pass it.
       ``rest_windows.rs``'s
       ``the_document_carries_intervals_and_a_coverage_end_and_no_boolean_anywhere``
       is the assertion that covers that half, because it stages a world with
       intervals in it — which is a thing no case on this deployment can do. When
       the registry lands and ``keys`` is populated here, this sweep becomes the
       stronger of the two and the sentence above should be deleted.
    4. **The surface is no existence oracle.** A plan id that names nothing at all
       is answered the same document, character for character, as a real plan of the
       caller's — which is the route's own decision (a 404 here would tell an
       unauthorized caller which plan ids exist). Whole-document equality modulo
       ``plan_id``, so a future arm that leaked existence through *any* field
       reddens.

    **This case pins a limitation as well as a capability, and the limitation is
    the answer's content.** No `CatalogVersionRegistryV1` exists on this stand, so
    no publish completes, so the tenant has no pin-eligible frontier and
    ``sellability_facts`` takes its ``NotAddressable`` arm for every plan. That is
    why the verdict is ``not_sellable`` with an empty key list and why predicate (2)
    is the one that ``failed``. **When the registry gear lands this changes, and the
    change is good news:** a published plan reads ``catalog_version`` non-null,
    predicate (2) ``satisfied``, one entry in ``keys`` for the row's market carrying
    predicate (1) and its intervals, and a verdict of ``not_evaluable`` — because
    predicates (5) and (6) owe a gear that is not in this repository and
    ``not_evaluable`` is **not** a yes. At that point the roster assertion below is
    what tells you which arm you are on, and it should be split in two.

    Nothing here reads the projected `state` token as the answer to "active at
    `t`": that token is the state at *projection* time and "active at `t`" is
    derived from ``interval ∧ now`` (D-99). On this arm there are no intervals to
    misread, which is precisely why the assertion that would misread them is absent
    rather than commented out.
    """
    plan_id, _etag = _assemble_publishable_plan_without_coverage(
        client, auth_headers, api_base, idempotency_key
    )
    path = f"{api_base}/plans/{plan_id}/sellability"

    # (1) Each parameter, absent, refused by name.
    present = {
        "at": AT_UTC_DESIGNATOR,
        "currency": SELLABILITY_CURRENCY,
        "region": SELLABILITY_REGION,
    }
    for missing in ("at", "currency", "region"):
        query = "&".join(
            f"{name}={value}" for name, value in present.items() if name != missing
        )
        refused = client.get(f"{path}?{query}", headers=auth_headers)
        assert refused.status_code == 400, refused.text
        assert refused.json()["detail"] == (
            f"the `{missing}` query parameter is required on the sellability surface"
        ), refused.text

    # (2) The same instant, written with a numeric offset, is refused. Read by
    #     equality on the part of the sentence the gear owns: the tail after the
    #     colon is chrono's own parse message and pinning it here would couple this
    #     module to a dependency's wording.
    mangled = client.get(
        f"{path}?{_sellability_query(AT_NUMERIC_OFFSET)}", headers=auth_headers
    )
    assert mangled.status_code == 400, mangled.text
    head, _colon, _chrono = mangled.json()["detail"].partition(":")
    assert head == "the `at` query parameter is not an RFC 3339 instant", mangled.text

    # The Z form of that same moment is answered — so the 400 above is about the
    # encoding and not about the instant, and not about this plan.
    answered = client.get(
        f"{path}?{_sellability_query(AT_UTC_DESIGNATOR)}", headers=auth_headers
    )
    assert answered.status_code == 200, answered.text
    body = answered.json()

    # The caller's own market and instant, echoed, so a stored response cannot be
    # mistaken for one about a different moment.
    assert body["plan_id"] == plan_id
    assert body["at"] == AT_UTC_DESIGNATOR
    assert body["currency"] == SELLABILITY_CURRENCY
    assert body["region"] == SELLABILITY_REGION

    # The `NotAddressable` arm, in full — see this docstring's fifth paragraph.
    assert body["catalog_version"] is None
    assert body["verdict"] == "not_sellable"
    assert body["keys"] == [], (
        "a plan no pin-eligible version carries binds no key, and an empty key set "
        f"is not sellable rather than vacuously sellable: {body}"
    )
    assert [
        (entry["ordinal"], entry["predicate"], entry["answer"])
        for entry in body["predicates"]
    ] == [
        (2, "committed_version", "failed"),
        (3, "availability_dates", "not_evaluable"),
        (4, "plan_lifecycle_state", "not_evaluable"),
        (6, "registry_sellable", "not_evaluable"),
    ], body

    # `failed` carries a `detail` and `not_evaluable` an `owed_to`, never both:
    # the whole of D-167 clause (3) is that a consumer can tell "this predicate is
    # false" from "this version cannot evaluate it".
    by_ordinal = {entry["ordinal"]: entry for entry in body["predicates"]}
    assert by_ordinal[2]["detail"], body
    assert by_ordinal[2]["owed_to"] is None, body
    owed = {ordinal: by_ordinal[ordinal]["owed_to"] for ordinal in (3, 4, 6)}
    for ordinal in (3, 4, 6):
        assert by_ordinal[ordinal]["detail"] is None, body
    # And on **this** arm all three owe the same thing — a committed version —
    # rather than each owing its own slice. That equality is what identifies the
    # arm: on a pinned delta (3) and (4) are answered outright and only (6) is
    # unevaluable, owing the registry gear instead.
    assert set(owed.values()) == {owed[3]}, owed
    assert owed[3], "an unevaluable predicate names what owes the fact it reads"

    # (3) No boolean anywhere, at any depth.
    booleans = _json_booleans(body)
    assert booleans == [], (
        f"the sellability document carries a boolean at {booleans}: {body}"
    )

    # (4) No existence oracle: a plan id naming nothing reads identically.
    stranger_id = str(uuid.uuid4())
    stranger = client.get(
        f"{api_base}/plans/{stranger_id}/sellability?"
        f"{_sellability_query(AT_UTC_DESIGNATOR)}",
        headers=auth_headers,
    )
    assert stranger.status_code == 200, stranger.text
    assert stranger.json() == {**body, "plan_id": stranger_id}, (
        "a plan that does not exist must read exactly as one no pin carries, or the "
        f"surface tells an unauthorized caller which ids exist: {stranger.text}"
    )


def test_a_schedule_through_the_route_is_refused_before_the_materiality_gate(
    client, auth_headers, auth_headers_reviewer, api_base, audit_segment
):
    """The window plane's two-person path, driven to where **this stand** stops it.

    The sequence a plan's first window is authored by is forced and is not a
    deadlock: an **empty first publish**, then the billable row, then the window,
    then the publish that freezes them. ``coverage::check`` ranges over the
    *billable* set and an empty set yields no violations, so a rowless revision is a
    real publish candidate. This case drives that sequence in order through the
    mounted surfaces, and the step it cannot take is step one's **commit**: there is
    no ``CatalogVersionRegistryV1`` on this stand and ``config/e2e-local.yaml``
    exposes no key that would supply one, so ``request_version`` fails closed at
    **503** and the plan never holds a current revision.

    The `POST` that follows is therefore answered **404** by
    ``plan_repo::load_current``, and that 404 is the property this case is named
    for: it is raised while *assembling* the mutation, which is upstream of §3
    step 3a's materiality gate. So on this deployment **no window mutation is ever
    put in front of a reviewer** — not because nothing is material here (with no
    threshold policy configured every unit is material, which since G6's Task 2
    includes a schedule and a lengthening `PATCH`, and since the G4 fix wave always
    includes a `DELETE` and a shortening `PATCH` under D-62) but because the act is
    refused before that question is asked.

    **That absence is asserted rather than assumed, and it is not vacuous.** Step
    one's submit arm opens a real `plan_revision` unit on this very plan, so the
    unit-opening machinery is demonstrably live in this request path; what the
    approvals surface holds afterwards is that one unit and no `window` unit. A test
    that only asserted "no window unit" would pass on a deployment where no unit of
    any kind could be opened.

    **This case pins a limitation, and its reddening is good news.** When the
    registry gear lands, the 503 becomes a receipt, the plan holds a current
    revision, and the `POST` answers **202** with ``outcome =
    submitted_for_approval``, a pinned unit and no window — at which point this test
    should be rewritten to walk submit → independent approve → the act taking
    effect. Note before doing so that a materially-refused **`POST`** has no
    completion path today: the window id is minted per request, so the call made
    after the approve names a different subject and opens a *second* unit. The
    completable acts are `PATCH` and `DELETE`, whose window id is in the path; a
    schedule's remedy is a register entry nobody has minted, and the operator
    sequence the design set actually names is to configure a threshold policy first
    (which ``test_a_threshold_policy_takes_two_principals_before_it_is_in_force``
    walks) so that a schedule's zero delta reaches no bar and commits on one
    principal.
    """
    # Step one of the forced order: the rowless plan, and its publish.
    plan_id, phase_id, etag = _assemble_publishable_shape(client, auth_headers)

    submitted = _submit_for_publish(client, auth_headers, plan_id, etag)
    plan_unit_id = submitted["approval"]["approval_id"]
    assert submitted["approval"]["subject_kind"] == "plan_revision", submitted
    approved = client.post(
        f"{api_base}/approvals/{plan_unit_id}/approve", headers=auth_headers_reviewer
    )
    assert approved.status_code == 200, approved.text

    committed = client.post(
        f"{api_base}/plans/{plan_id}/publish",
        headers={**auth_headers, "If-Match": etag},
    )
    assert committed.status_code == 503, (
        "a 200 here means the registry gear landed — see this test's docstring for "
        f"what to rewrite: {committed.text}"
    )
    still_draft = client.get(f"{api_base}/plans/{plan_id}", headers=auth_headers)
    assert still_draft.json()["lifecycle_state"] == "draft", still_draft.text

    # Step two: the billable row the window would cover.
    priced = client.post(
        f"{api_base}/plans/{plan_id}/prices",
        headers={**auth_headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "scope_key": {
                "currency": SELLABILITY_CURRENCY,
                "region": SELLABILITY_REGION,
                "phase": phase_id,
                "price_eligibility": "all_subscriptions",
                "charge_kind": "recurring",
                "cohort": None,
            },
            "content": {
                "model_kind": "flat",
                "amount_minor": 1500,
                "rounding_policy_ref": "half_up",
                "billing_timing": "advance",
                "tax_inclusive": False,
                # `inst-pi-required`: a recurring row MUST publish all three.
                # Subscriptions computes proration from the frozen values and the
                # catalog substitutes no defaults, so a row without them is one no
                # consumer can prorate. Absent here, every publish below answered
                # `PRORATION_INPUTS_MISSING` (400) instead of reaching the arm it
                # was written for — including the window test, whose
                # `WINDOW_COVERAGE_MISSING` was masked by this earlier refusal.
                # The Rust tier had the same fixture defect and D-344 fixed it
                # there (`rest_cutovers.rs`): the fixture was authoring an
                # unpublishable row, not the rule refusing a legal one.
                "billing_anchor_policy": "calendar_month",
                "proration_basis": "calendar_days_actual",
                "credit_on_downgrade": False,
            },
        },
    )
    assert priced.status_code == 201, priced.text
    price_id = priced.json()["price_id"]

    # Step three: the window, through the mounted route, with nothing else wrong —
    # a valid `Idempotency-Key`, a well-formed body, a future start, a price row
    # that exists and a caller the PDP allows. So the 404 can only be the current
    # revision's, which is what the equality on `detail` says.
    refused = client.post(
        f"{api_base}/prices/{price_id}/windows",
        headers={**auth_headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "effective_from": "2099-09-01T00:00:00Z",
            "effective_to": None,
            "reason_code": "e2eSchedule",
        },
    )
    assert refused.status_code == 404, refused.text
    assert refused.json()["detail"] == (
        f"current plan revision {plan_id} not found"
    ), refused.text
    # And the machine-readable half, which is what a client would branch on: the
    # subject the 404 is about is the **plan**, not the price row in the path and
    # not the window that was never minted.
    assert refused.json()["context"] == {
        "resource_type": "gts.cf.bss.pricing.plan.v1~",
        "resource_name": plan_id,
    }, refused.text

    # The twin: the machinery that would have opened a unit is live on this plan,
    # and it opened no `window` unit — because the refusal is upstream of the gate.
    units = client.get(
        f"{api_base}/approvals", headers=auth_headers, params={"limit": 1000}
    ).json()["items"]
    mine = [row for row in units if row["subject_ref"].startswith(plan_id)]
    assert [(row["approval_id"], row["subject_kind"]) for row in mine] == [
        (plan_unit_id, "plan_revision")
    ], (
        "the only unit over this plan is step one's; a schedule that had reached the "
        f"materiality gate would have opened a `window` unit beside it: {mine}"
    )

    # And nothing reached the window plane, read from the surface an operator would
    # read it from: the key is still uncovered and holds no interval at all.
    report = client.get(f"{api_base}/plans/{plan_id}/coverage", headers=auth_headers)
    assert report.status_code == 200, report.text
    assert len(report.json()["keys"]) == 1, report.text
    entry = report.json()["keys"][0]
    assert entry["required"] is True
    assert entry["covered"] is False
    assert entry["intervals"] == []
    assert entry["coverage_end"] == {"kind": "uncovered", "at": None}

    # No window record reached the plan's audit segment either. A window mutation
    # writes `action = publish` under `subject_kind = window` on the **plan's**
    # chain (D-135 keys a segment on the audited subject's aggregate, and a
    # window's aggregate is the plan its row prices), so `subject_kind` is the
    # discriminator and the verb is not: this segment already holds `publish` rows
    # from step one's submit. The non-empty assertion is what keeps the `window`
    # count from being vacuously zero over a segment that was never written.
    kinds = [row["subject_kind"] for row in audit_segment(plan_id)]
    assert kinds, "this plan's audit segment was written by the steps above"
    assert kinds.count("window") == 0, kinds


# ---------------------------------------------------------------------------
# Slice 6 / G6: the one two-person act this deployment can complete.
# ---------------------------------------------------------------------------
#
# A currency no other case uses — which is tidy and is **not** what protects the
# module. The argument that used to stand here was false and is worth stating so it
# is not reinvented: it claimed `inst-mat-percurrency`'s fail-safe half would answer
# `noConfiguredThreshold` for a `USD` row while a `CHF` policy was in force, leaving
# every other case untouched. `evaluate` never reaches that half here. Its order is
# registered trigger, then unset policy, then **`baseline is None` ->
# `firstPublish`**, and only then the per-currency walk — and on this stand no plan
# ever publishes, so `baseline` is `None` for every plan. With any policy approved,
# a `USD` publish therefore answers `firstPublish`, not `noConfiguredThreshold`, and
# the currency chosen is irrelevant. (The old counterfactual was wrong the same way:
# `rowWithoutBaseline` needs a baseline that exists.)
#
# **What actually protects the module is order plus the wipe**, neither of which is
# enforced: pytest collects in source order and this is the last case in the file,
# and the run protocol's `rm -rf ~/.cf-gears/bss-pricing` clears the store between
# runs. Both matter because the policy store is the one piece of tenant-singleton
# state this module writes, and the version this case approves is dated in the **past**,
# so it is in force the moment it is approved. (Until D-188 landed, a 2099 date was in
# force too — `effective_from` was compared against nothing — which is why an earlier
# revision of this comment said a future date was harmless. It is not harmless now for
# the opposite reason: a future-dated policy is never in force at all, and this case
# would assert against `null`.)
#
# So the cases that depend on an unset policy assert it as a **precondition** rather
# than trusting the ordering — see `_assert_no_effective_policy`. A run whose order
# changed then fails at the case that broke it, naming the policy, instead of three
# hundred lines away on a materiality token nobody was thinking about.
POLICY_CURRENCY = "CHF"
POLICY_ABSOLUTE_MINOR = 500
# **In the past, and that is what makes this case's name true.** Since D-188 a version
# is not the tenant's policy before its own `effective_from`, so a 2099 date would leave
# `effective` null however many principals signed it — the case asserts the diff is *in
# force*, which is the whole of "before it is in force". Nothing here is authored
# forward, so the instant only recedes.
POLICY_EFFECTIVE_FROM = "2020-01-01T00:00:00Z"


def test_a_threshold_policy_takes_two_principals_before_it_is_in_force(
    client, auth_headers, auth_headers_reviewer, api_base
):
    """D-10 end to end: `PUT` opens a unit, and **the diff does not apply** until
    an independent principal approves it.

    The two-person path walked to completion, on the one act this deployment can
    complete it for. Every other governed act in this gear needs a
    ``CatalogVersion`` at its commit — a plan publish and all three window
    mutations — and there is no registry here, so each stops at 503 or at the
    absent current revision. The policy store needs none: it is versioned and
    append-only, and "in force" is a fact read off the **approval** store rather
    than a column anything flips. So this is where an operator can be shown that
    the second signature is load-bearing rather than decorative.

    One case rather than four, because the value is the sequence: the unit id comes
    from the 202, the "still unset" read is only meaningful against a unit that is
    genuinely open, the self-approve refusal is only meaningful against one still
    pending, and the final read is only evidence if the approve before it landed.

    **The bootstrap is the point, and it is fail-safe.** A tenant with no policy has
    everything material (``inst-mat-failsafe``), so its *first* proposal is itself
    an always-material act — which is why no tenant can configure a threshold
    without completing an approved unit first, and why the verdict on the unit reads
    ``alwaysMaterialTrigger`` from ``materiality::evaluate`` over
    ``Trigger::ThresholdPolicyDiff`` rather than from a constant this surface wrote.

    This case pins a **capability**.

    **One thing about re-running it, established by watching it happen rather than
    reasoned about.** It asserts about *the version this `PUT` proposed* rather than
    about version 0, so a surviving SQLite file left by a **completed** run reads the
    same as a fresh one. A run this case *aborted part way* is different: the policy
    store is the one piece of tenant-singleton state this module writes, so a failure
    between the `PUT` and the approve leaves a **pending** unit, and the next run then
    fails at the first assertion below — ``pending_approval is None``, its own
    precondition — rather than at whatever was actually broken. If you meet that,
    the `pending_approval` in the failure message will name a version and the remedy
    is the ``rm -rf ~/.cf-gears/bss-pricing`` the run protocol already mandates; do
    not go looking for a defect in the policy store. Nothing else in this module has
    this property: every other case names a fresh plan and a fresh idempotency key.
    """
    # Nothing under review, which is this act's own precondition: a second proposal
    # while one is open is refused `PENDING_CHANGE_UNIT_EXISTS`.
    before = client.get(f"{api_base}/config/approval-threshold-policy", headers=auth_headers)
    assert before.status_code == 200, before.text
    assert before.json()["pending_approval"] is None, before.text

    # The `PUT` requires an `If-Match` (D-186), and this read is where the tag comes
    # from: it is an opaque digest over the representation above, so there is nothing
    # to compute and nothing to hard-code. Asserted rather than fetched silently,
    # because a `GET` that stopped emitting it would make the `PUT` unsatisfiable for
    # every client and the failure would otherwise surface as a confusing 400 below.
    tag = before.headers.get("ETag")
    assert tag, (
        "the policy GET must carry the ETag its PUT demands; without it no client can "
        f"satisfy the precondition: {dict(before.headers)}"
    )

    proposed = client.put(
        f"{api_base}/config/approval-threshold-policy",
        headers={**auth_headers, "If-Match": tag},
        json={
            "effective_from": POLICY_EFFECTIVE_FROM,
            "entries": [
                {
                    "currency": POLICY_CURRENCY,
                    "absolute_minor": POLICY_ABSOLUTE_MINOR,
                }
            ],
        },
    )
    assert proposed.status_code == 202, (
        f"the PUT opens a unit; it does not apply a diff: {proposed.text}"
    )
    body = proposed.json()
    version = body["proposed"]["version"]
    unit = body["approval"]
    approval_id = unit["approval_id"]
    assert unit["state"] == "submitted"
    assert unit["subject_kind"] == "policy"
    assert unit["subject_ref"] == str(version), (
        f"a policy unit's subject is its version number: {unit}"
    )
    assert unit["submitter_principal"] == SUBMITTER_PRINCIPAL_ID
    assert unit["approver_principal"] is None
    # `tripped` is null: `alwaysMaterialTrigger` is an answer about the act (D-10 makes
    # any policy diff material), so no row reached a bar here (D-187).
    assert unit["materiality"] == {
        "material": True,
        "reason": "alwaysMaterialTrigger",
        "tripped": None,
        # Which always-material trigger, not merely that one fired: `reason` is the
        # same token for every one of them, so without this the assertion would hold
        # if a policy diff were recorded as an overlay mutation.
        "trigger": "thresholdPolicyDiff",
    }
    assert body["proposed"]["entries"] == [
        {
            "currency": POLICY_CURRENCY,
            "absolute_minor": POLICY_ABSOLUTE_MINOR,
            "percent_bp": None,
        }
    ], body

    # **Not in force**, and this is the assertion the 202 is only a hint about.
    during = client.get(
        f"{api_base}/config/approval-threshold-policy", headers=auth_headers
    ).json()
    assert during["pending_approval"]["approval_id"] == approval_id, during
    assert during["effective"] is None or during["effective"]["version"] != version, (
        f"a proposal under review is not the tenant's policy: {during}"
    )

    # The submitter's own approve is refused on IDENTITY — the two-person rule is
    # about principals and not about roles, and this token's PDP decision for
    # `approval x approve` is `true`.
    self_approval = client.post(
        f"{api_base}/approvals/{approval_id}/approve", headers=auth_headers
    )
    assert self_approval.status_code == 403, self_approval.text
    assert "SELF_APPROVAL_FORBIDDEN" in _problem_codes(self_approval.json())

    # ...and the refused decision is not a decision: the record is untouched.
    still_open = client.get(
        f"{api_base}/config/approval-threshold-policy", headers=auth_headers
    ).json()
    assert still_open["pending_approval"]["approval_id"] == approval_id
    assert still_open["pending_approval"]["state"] == "submitted"

    # An independent principal in the same tenant decides it.
    decided = client.post(
        f"{api_base}/approvals/{approval_id}/approve", headers=auth_headers_reviewer
    )
    assert decided.status_code == 200, decided.text
    assert decided.json()["state"] == "approved"
    assert decided.json()["approver_principal"] == REVIEWER_PRINCIPAL_ID
    assert decided.json()["submitter_principal"] == SUBMITTER_PRINCIPAL_ID

    # Only now is it the tenant's policy — and it is the content that was signed
    # for, not merely a version number that moved.
    after = client.get(
        f"{api_base}/config/approval-threshold-policy", headers=auth_headers
    ).json()
    assert after["effective"]["version"] == version, after
    assert after["effective"]["effective_from"] == POLICY_EFFECTIVE_FROM
    assert after["effective"]["entries"] == [
        {
            "currency": POLICY_CURRENCY,
            "absolute_minor": POLICY_ABSOLUTE_MINOR,
            "percent_bp": None,
        }
    ], after
    assert after["pending_approval"] is None, (
        f"the proposal is decided, so nothing is waiting: {after}"
    )


def _the_one_price_id(client, headers, api_base, plan_id):
    """The plan's single authoring price row, read back over HTTP."""
    listed = client.get(f"{api_base}/plans/{plan_id}/prices", headers=headers)
    assert listed.status_code == 200, listed.text
    rows = listed.json()["items"]
    assert len(rows) == 1, listed.text
    return rows[0]["price_id"]


def _assemble_publishable_plan_without_coverage(client, headers, api_base, key):
    """A plan that would publish but for its missing window.

    Deliberately **not** :func:`_assemble_publishable_plan` with a flag: a fixture
    an argument can switch off is a rule the next fixture switches off. This is a
    different world, built for the two cases whose subject is the absence, and it
    passes every other publish rule so that the refusal it provokes can only be the
    coverage one — which the assertions check by counting the violations rather than
    by looking for theirs among others.
    """
    phase_id = str(uuid.uuid4())
    created = client.post(
        f"{api_base}/plans",
        headers={**headers, "Idempotency-Key": key},
        json={
            "sku_id": str(uuid.uuid4()),
            "plan_tier": "gold",
            "billing_cycle": "recurring",
            "frequency": {"kind": "monthly"},
        },
    )
    assert created.status_code == 201, created.text
    plan_id = created.json()["plan_id"]

    phased = client.patch(
        f"{api_base}/plans/{plan_id}",
        headers={**headers, "If-Match": '"0-0"'},
        json={
            "phases": [
                {
                    "phase_id": phase_id,
                    "kind": "evergreen",
                    "ordinal": 0,
                    "converts_to_phase_id": None,
                    "phase_duration_days": None,
                    "display_trial_days": None,
                }
            ]
        },
    )
    assert phased.status_code == 200, phased.text

    described = client.patch(
        f"{api_base}/plans/{plan_id}",
        headers={**headers, "If-Match": phased.headers["etag"]},
        json={
            "descriptor_set": {
                "invoice_line_template": "{plan}",
                "gl_code": "4000",
                "itemization_rule": "per_charge",
                "additional": {},
            }
        },
    )
    assert described.status_code == 200, described.text

    priced = client.post(
        f"{api_base}/plans/{plan_id}/prices",
        headers={**headers, "Idempotency-Key": str(uuid.uuid4())},
        json={
            "scope_key": {
                "currency": "USD",
                "region": "EU",
                "phase": phase_id,
                "price_eligibility": "all_subscriptions",
                "charge_kind": "recurring",
                "cohort": None,
            },
            "content": {
                "model_kind": "flat",
                "amount_minor": 1500,
                "rounding_policy_ref": "half_up",
                "billing_timing": "advance",
                "tax_inclusive": False,
                # `inst-pi-required`: a recurring row MUST publish all three.
                # Subscriptions computes proration from the frozen values and the
                # catalog substitutes no defaults, so a row without them is one no
                # consumer can prorate. Absent here, every publish below answered
                # `PRORATION_INPUTS_MISSING` (400) instead of reaching the arm it
                # was written for — including the window test, whose
                # `WINDOW_COVERAGE_MISSING` was masked by this earlier refusal.
                # The Rust tier had the same fixture defect and D-344 fixed it
                # there (`rest_cutovers.rs`): the fixture was authoring an
                # unpublishable row, not the rule refusing a legal one.
                "billing_anchor_policy": "calendar_month",
                "proration_basis": "calendar_days_actual",
                "credit_on_downgrade": False,
            },
        },
    )
    assert priced.status_code == 201, priced.text

    return plan_id, described.headers["etag"]
