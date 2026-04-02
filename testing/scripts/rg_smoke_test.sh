#!/usr/bin/env bash
# Created: 2026-04-16 by Constructor Tech
# Resource Group + AuthZ smoke test via curl
# Usage: ./testing/scripts/rg_smoke_test.sh [BASE_URL]
# Default: http://localhost:8087

set -euo pipefail

BASE="${1:-http://localhost:8087}"
API="${BASE}/cf"
PASS=0
FAIL=0
TOTAL=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Unique suffix to avoid collisions
TS=$(date +%s)

check() {
  local desc="$1"
  local expected_status="$2"
  local actual_status="$3"
  TOTAL=$((TOTAL + 1))
  if [ "$actual_status" = "$expected_status" ]; then
    PASS=$((PASS + 1))
    printf "${GREEN}PASS${NC} [%s] %s (HTTP %s)\n" "$expected_status" "$desc" "$actual_status"
  else
    FAIL=$((FAIL + 1))
    printf "${RED}FAIL${NC} [expected %s, got %s] %s\n" "$expected_status" "$actual_status" "$desc"
  fi
}

check_body() {
  local desc="$1"
  local pattern="$2"
  local body="$3"
  TOTAL=$((TOTAL + 1))
  if echo "$body" | grep -q "$pattern"; then
    PASS=$((PASS + 1))
    printf "${GREEN}PASS${NC} %s (body contains '%s')\n" "$desc" "$pattern"
  else
    FAIL=$((FAIL + 1))
    printf "${RED}FAIL${NC} %s (body missing '%s')\n" "$desc" "$pattern"
    echo "  Body: $(echo "$body" | head -c 200)"
  fi
}

echo "========================================"
echo "  RG + AuthZ Smoke Test"
echo "  Base: $API"
echo "========================================"
echo ""

# ── 0. Server reachability ───────────────────────────────────────────────

printf "${YELLOW}[0] Server reachability${NC}\n"
STATUS=$(curl -s -o /dev/null -w "%{http_code}" "${API}/openapi.json" 2>/dev/null || echo "000")
if [ "$STATUS" = "000" ]; then
  printf "${RED}Server not reachable at ${API}${NC}\n"
  exit 1
fi
check "GET /openapi.json" "200" "$STATUS"
echo ""

# ── 1. Types CRUD ────────────────────────────────────────────────────────

printf "${YELLOW}[1] Types CRUD${NC}\n"

TYPE_CODE="gts.cf.core.rg.type.v1~x.smoke.test.root.v1~"
MEMBER_TYPE_CODE="gts.cf.core.rg.type.v1~x.smoke.test.member${TS}.v1~"

# Create root type
RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/types-registry/v1/types" \
  -H "Content-Type: application/json" \
  -d "{
    \"code\": \"${TYPE_CODE}\",
    \"can_be_root\": true,
    \"allowed_parents\": [],
    \"allowed_memberships\": []
  }" 2>/dev/null)
BODY=$(echo "$RESP" | sed '$d')
STATUS=$(echo "$RESP" | tail -1)
# 201 or 409 (already exists) both OK
if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ]; then
  PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
  printf "${GREEN}PASS${NC} POST /types (create root type) (HTTP %s)\n" "$STATUS"
else
  check "POST /types (create root type)" "201" "$STATUS"
fi

# Update root type to allow self as parent + member type
curl -s -o /dev/null -X PUT "${API}/types-registry/v1/types/${TYPE_CODE//\~/%7E}" \
  -H "Content-Type: application/json" \
  -d "{
    \"can_be_root\": true,
    \"allowed_parents\": [\"${TYPE_CODE}\"],
    \"allowed_memberships\": []
  }" 2>/dev/null

# Create member type
RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/types-registry/v1/types" \
  -H "Content-Type: application/json" \
  -d "{
    \"code\": \"${MEMBER_TYPE_CODE}\",
    \"can_be_root\": true,
    \"allowed_parents\": [],
    \"allowed_memberships\": []
  }" 2>/dev/null)
STATUS=$(echo "$RESP" | tail -1)
if [ "$STATUS" = "201" ] || [ "$STATUS" = "409" ]; then
  PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
  printf "${GREEN}PASS${NC} POST /types (create member type) (HTTP %s)\n" "$STATUS"
else
  check "POST /types (create member type)" "201" "$STATUS"
fi

# Update root type to allow member type in memberships
curl -s -o /dev/null -X PUT "${API}/types-registry/v1/types/${TYPE_CODE//\~/%7E}" \
  -H "Content-Type: application/json" \
  -d "{
    \"can_be_root\": true,
    \"allowed_parents\": [\"${TYPE_CODE}\"],
    \"allowed_memberships\": [\"${MEMBER_TYPE_CODE}\"]
  }" 2>/dev/null

# List types
RESP=$(curl -s -w "\n%{http_code}" "${API}/types-registry/v1/types" 2>/dev/null)
BODY=$(echo "$RESP" | sed '$d')
STATUS=$(echo "$RESP" | tail -1)
check "GET /types (list)" "200" "$STATUS"
check_body "Types list has items" "items" "$BODY"

# Get type by code
ENCODED_CODE=${TYPE_CODE//\~/%7E}
RESP=$(curl -s -w "\n%{http_code}" "${API}/types-registry/v1/types/${ENCODED_CODE}" 2>/dev/null)
BODY=$(echo "$RESP" | sed '$d')
STATUS=$(echo "$RESP" | tail -1)
check "GET /types/{code}" "200" "$STATUS"
check_body "Type has code field" "code" "$BODY"
check_body "Type has can_be_root" "can_be_root" "$BODY"

echo ""

# ── 2. Groups CRUD ───────────────────────────────────────────────────────

printf "${YELLOW}[2] Groups CRUD${NC}\n"

# Create root group
RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/resource-group/v1/groups" \
  -H "Content-Type: application/json" \
  -d "{
    \"type\": \"${TYPE_CODE}\",
    \"name\": \"Smoke Root ${TS}\",
    \"metadata\": {\"barrier\": true}
  }" 2>/dev/null)
BODY=$(echo "$RESP" | sed '$d')
STATUS=$(echo "$RESP" | tail -1)
check "POST /groups (create root)" "201" "$STATUS"
ROOT_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null || echo "")
check_body "Group has id" "\"id\"" "$BODY"
check_body "Group has type field" "\"type\"" "$BODY"
check_body "Group has tenant_id" "tenant_id" "$BODY"
check_body "Group has metadata barrier" "barrier" "$BODY"

if [ -z "$ROOT_ID" ]; then
  printf "${RED}Cannot extract root group ID, aborting group tests${NC}\n"
  echo ""
else
  # Create child group
  RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/resource-group/v1/groups" \
    -H "Content-Type: application/json" \
    -d "{
      \"type\": \"${TYPE_CODE}\",
      \"name\": \"Smoke Child ${TS}\",
      \"parent_id\": \"${ROOT_ID}\"
    }" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "POST /groups (create child under root)" "201" "$STATUS"
  CHILD_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))" 2>/dev/null || echo "")
  check_body "Child has parent_id" "parent_id" "$BODY"

  # Get group
  RESP=$(curl -s -w "\n%{http_code}" "${API}/resource-group/v1/groups/${ROOT_ID}" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /groups/{id}" "200" "$STATUS"
  check_body "No gts_type_id in response" "" "$BODY" && true  # will check absence below
  if echo "$BODY" | grep -q "gts_type_id"; then
    FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1))
    printf "${RED}FAIL${NC} Response leaks gts_type_id (SMALLINT)\n"
  else
    PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
    printf "${GREEN}PASS${NC} No SMALLINT IDs leaked in response\n"
  fi

  # List groups
  RESP=$(curl -s -w "\n%{http_code}" "${API}/resource-group/v1/groups" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /groups (list)" "200" "$STATUS"
  check_body "List has items" "items" "$BODY"

  # PATCH group
  RESP=$(curl -s -w "\n%{http_code}" -X PATCH "${API}/resource-group/v1/groups/${ROOT_ID}" \
    -H "Content-Type: application/json" \
    -d "{\"name\": \"Patched Root ${TS}\"}" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "PATCH /groups/{id} (partial update)" "200" "$STATUS"
  check_body "Name updated" "Patched Root" "$BODY"

  # Hierarchy
  RESP=$(curl -s -w "\n%{http_code}" "${API}/resource-group/v1/groups/${ROOT_ID}/hierarchy" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /groups/{id}/hierarchy" "200" "$STATUS"
  check_body "Hierarchy has items" "items" "$BODY"
  check_body "Hierarchy has depth" "depth" "$BODY"

  echo ""

  # ── 3. Memberships ──────────────────────────────────────────────────────

  printf "${YELLOW}[3] Memberships${NC}\n"

  MT_ENCODED=${MEMBER_TYPE_CODE//\~/%7E}

  # Add membership
  RESP=$(curl -s -w "\n%{http_code}" -X POST \
    "${API}/resource-group/v1/memberships/${ROOT_ID}/${MT_ENCODED}/res-smoke-${TS}" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "POST /memberships (add)" "201" "$STATUS"
  check_body "Membership has resource_type" "resource_type" "$BODY"
  check_body "Membership has resource_id" "resource_id" "$BODY"
  if echo "$BODY" | grep -q "tenant_id"; then
    FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1))
    printf "${RED}FAIL${NC} Membership response leaks tenant_id\n"
  else
    PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
    printf "${GREEN}PASS${NC} No tenant_id in membership response\n"
  fi

  # List memberships
  RESP=$(curl -s -w "\n%{http_code}" "${API}/resource-group/v1/memberships" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /memberships (list)" "200" "$STATUS"

  # List memberships with OData filter
  FILTER=$(python3 -c "import urllib.parse; print(urllib.parse.quote('group_id eq ${ROOT_ID}'))")
  RESP=$(curl -s -w "\n%{http_code}" \
    "${API}/resource-group/v1/memberships?\$filter=${FILTER}" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /memberships with \$filter=group_id" "200" "$STATUS"
  check_body "Filtered memberships has items" "items" "$BODY"

  # Remove membership
  RESP=$(curl -s -w "\n%{http_code}" -X DELETE \
    "${API}/resource-group/v1/memberships/${ROOT_ID}/${MT_ENCODED}/res-smoke-${TS}" 2>/dev/null)
  STATUS=$(echo "$RESP" | tail -1)
  check "DELETE /memberships (remove)" "204" "$STATUS"

  echo ""

  # ── 4. Error handling (RFC 9457) ────────────────────────────────────────

  printf "${YELLOW}[4] Error handling (RFC 9457)${NC}\n"

  FAKE_ID="00000000-0000-0000-0000-000000000000"

  # 404 Not Found
  RESP=$(curl -s -w "\n%{http_code}" -D /tmp/rg_headers.txt \
    "${API}/resource-group/v1/groups/${FAKE_ID}" 2>/dev/null)
  BODY=$(echo "$RESP" | sed '$d')
  STATUS=$(echo "$RESP" | tail -1)
  check "GET /groups/{fake_id} returns 404" "404" "$STATUS"
  check_body "Error has status field" "\"status\"" "$BODY"
  check_body "Error has title field" "\"title\"" "$BODY"
  if grep -qi "application/problem+json" /tmp/rg_headers.txt 2>/dev/null; then
    PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
    printf "${GREEN}PASS${NC} Content-Type is application/problem+json\n"
  else
    FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1))
    printf "${RED}FAIL${NC} Content-Type is NOT application/problem+json\n"
  fi

  # 400 Bad Request
  RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/types-registry/v1/types" \
    -H "Content-Type: application/json" \
    -d '{"code": "invalid", "can_be_root": true}' 2>/dev/null)
  STATUS=$(echo "$RESP" | tail -1)
  check "POST /types (invalid code) returns 400" "400" "$STATUS"

  # 409 Conflict (duplicate type)
  RESP=$(curl -s -w "\n%{http_code}" -X POST "${API}/types-registry/v1/types" \
    -H "Content-Type: application/json" \
    -d "{\"code\": \"${TYPE_CODE}\", \"can_be_root\": true}" 2>/dev/null)
  STATUS=$(echo "$RESP" | tail -1)
  check "POST /types (duplicate) returns 409" "409" "$STATUS"

  echo ""

  # ── 5. Cleanup ──────────────────────────────────────────────────────────

  printf "${YELLOW}[5] Cleanup${NC}\n"

  # Force delete root (cascades to child)
  RESP=$(curl -s -w "\n%{http_code}" -X DELETE \
    "${API}/resource-group/v1/groups/${ROOT_ID}?force=true" 2>/dev/null)
  STATUS=$(echo "$RESP" | tail -1)
  check "DELETE /groups/{id}?force=true (cascade)" "204" "$STATUS"

  # Verify child is gone too
  RESP=$(curl -s -w "\n%{http_code}" "${API}/resource-group/v1/groups/${CHILD_ID}" 2>/dev/null)
  STATUS=$(echo "$RESP" | tail -1)
  check "GET deleted child returns 404 (cascade verified)" "404" "$STATUS"

  # Delete types
  curl -s -o /dev/null -X DELETE "${API}/types-registry/v1/types/${MEMBER_TYPE_CODE//\~/%7E}" 2>/dev/null
  curl -s -o /dev/null -X DELETE "${API}/types-registry/v1/types/${TYPE_CODE//\~/%7E}" 2>/dev/null
fi

rm -f /tmp/rg_headers.txt

echo ""
echo "========================================"
printf "  Results: ${GREEN}%d passed${NC}, ${RED}%d failed${NC}, %d total\n" "$PASS" "$FAIL" "$TOTAL"
echo "========================================"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
