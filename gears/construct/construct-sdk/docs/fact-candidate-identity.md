# Fact candidate identity derivation

This spec gives the exact form of `provenance` and `version` for a record of type `gts.cf.connectors.core.record.v1~cf.construct.enrichment.fact_candidate.v1~`, and the conformance vectors that check it. The type is `FactCandidateV1` in `construct_sdk::gts`, whose generated schema is in `../schemas/`. Where this spec and the candidate schema disagree, the schema wins.

## Derivation

Python 3.13, standard library only. The Unicode tables are those of Python 3.13's `unicodedata`, version 15.1.0. A producer must check the version when it starts and must not start on other tables, as the first lines below do.

```python
import hashlib
import json
import unicodedata
import uuid

if unicodedata.unidata_version != "15.1.0":
    raise RuntimeError("fact candidate identity needs Unicode 15.1.0")


def normalize(s):
    return " ".join(unicodedata.normalize("NFKC", s).casefold().split())


def derive(subject_id, payload):
    subject = str(uuid.UUID(subject_id))
    field = payload["field"]
    value = payload["value"]
    if isinstance(value, list):  # research_areas, skills
        normalized = "\n".join(normalize(item) for item in value)
    else:
        normalized = normalize(value)
    provenance = "sha256:" + hashlib.sha256(
        f"{subject}\n{field}\n{normalized}".encode("utf-8")
    ).hexdigest()
    version = "sha256:" + hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return provenance, version
```

Both digests are written `sha256:` plus 64 lower-case hex digits, and the schema refuses any other form.

## Provenance

- `subject` is the canonical lower-case hyphenated form of `subject_id`. An upper-case `subject_id` validates against the base pattern, so without the canonical form one person would yield two provenances. The dedupe key carries the same canonical form.
- The value hashed is the value emitted. For `research_areas` and `skills` the emitted list is the source's items cleaned: each item stripped, empty items dropped, order kept, nothing cut. The schema refuses an item that starts or ends with whitespace.
- `normalize` applies NFKC, then casefold, then collapses every whitespace run to one space, which also strips the ends.
- Casefold is Unicode full case folding: the mappings of `CaseFolding.txt` with status C and F, not the Turkic mappings (status T) and not the simple foldings (status S). It is not lower-casing: `ß` folds to `ss`, and lower-casing keeps `ß`. Most languages offer only lower-casing, so a non-Python producer needs a full case-folding implementation.
- Whitespace is the set Python's `str.split()` splits on when given no argument: the characters for which `str.isspace()` is true.
- A list value is normalized item by item and joined with `"\n"` in emitted order. A reorder is a different value. Normalization removes newlines inside an item, so the join is unambiguous.
- No `value` or list item may be whitespace only, because it would normalize to `""` and give a degenerate provenance. `evidence` and `rationale` are refused when whitespace only because they carry no content. The schema refuses a whitespace-only `value`, `evidence` or `rationale` with the pattern `\S`, and a list item that is empty, whitespace only or not stripped with the pattern `^\S(?:[\s\S]*\S)?$`. The receiver check below covers the characters on which regex dialects disagree.

## Version

`version` is computed over the payload only, never the envelope or the record's other members. Its input is the payload's canonical JSON, the bytes that `json.dumps(payload, sort_keys=True, separators=(",", ":"))` gives, encoded as UTF-8. Every byte is ASCII. A non-Python producer reproduces these rules:

- An object's members are sorted by key, in code point order. An absent member is omitted, never sent as `null`.
- No whitespace separates tokens: `,` between members and items, `:` between a key and its value.
- An array keeps its emitted order.
- In a string, a character from U+0020 to U+007E is written as itself, apart from `"` and `\`, which are written `\"` and `\\`. `/` is not escaped.
- U+0008, U+0009, U+000A, U+000C and U+000D are written `\b`, `\t`, `\n`, `\f` and `\r`.
- Every other character below U+0020, U+007F, and every character above U+007F is written `\u` plus four lower-case hex digits, for example `\u00f6` for `ö`. A character above U+FFFF is written as its UTF-16 surrogate pair, each half escaped, for example `\ud83d\ude00` for U+1F600.
- A number renders as Python's `json` renders a float, which is its `repr`: the shortest digit string that reads back as the same float. A producer must emit `confidence` and `identity_confidence` as floats to get the same bytes: the schema types them as `number`, so `1` validates, but its bytes differ from `1.0`. Python writes the digits in one of two forms, chosen by the decimal exponent of the value in scientific notation, for example -5 for 0.00001 and -1 for 0.74:
  - If the exponent is -4 or more and less than 16, the number is written with a decimal point and at least one digit after it: `0.74`, `0.0001`, `1.0` (not `1`).
  - Otherwise it is written with an exponent: the digits, with a decimal point only after the first digit when there is more than one, then `e`, the sign of the exponent, and the exponent in at least two digits: `1e-05` (not `0.00001`), `1.5e-05`, `1e+16`.
  - Negative zero is written `-0.0`. The schema accepts it, because `minimum: 0` accepts negative zero.

A producer in another language must apply these rules itself. JavaScript's `JSON.stringify`, for example, writes `0.00001` for `1e-05`, `1` for `1.0` and `0` for `-0.0`. The vector `exponent_float` below has a `confidence` of `1e-05`.

`identity_confidence` and `rationale` are version inputs. A re-judgment that changes either and leaves the value unchanged gives a new version with the same provenance, and the receiver replaces the version it holds with it. A re-push of the same judgment sends the same bytes and dedupes.

The form is not RFC 8785 (JCS). The producer is Python, and this form is one call of its standard library. A non-Python producer reproduces it from the rules above and checks itself against the vectors below.

## Receiver checks

- The receiver refuses a candidate whose normalized value, or any normalized list item, is `""`. The check does not depend on the regex dialect of the receiver's schema validator. Construct's intake does it in `check_type_rules`, which also checks the two forms the generated schema cannot carry: the digests' `sha256:` form, and a list value exactly for `research_areas` and `skills`.
- No receiver recomputes the digests. A receiver that does needs the 15.1.0 tables. Construct does not recompute them.

## Residuals

- **Unicode version.** NFKC and casefold follow the Unicode tables of the running implementation. On tables other than 15.1.0, a value with rare characters, ones whose assignment or mapping differs between the versions, can give a different provenance. The receiver treats a different provenance as a different fact, so it passes the deleted-fact block, whose key includes the provenance, and a fact the user deleted can come back. A producer that runs the required start-up check does not emit on other tables. Moving to new tables is a change to this spec.
- **Whitespace set.** The schema's `\S` follows the regex dialect of the validator. Python's `jsonschema` uses `re`, whose whitespace is the `str.isspace()` set, so it refuses a value made only of U+001C to U+001F or U+0085. A validator that follows ECMA-262, as a draft-07 `pattern` specifies, accepts that value, and it normalizes to `""`. The receiver check above refuses it under either dialect. U+FEFF is whitespace to ECMA-262 and not to `str.isspace()`. The rule on the ends of a list item has the same gap for these characters.
- **End of an item.** The GTS regular-expression profile has no lookaround, so the list-item pattern ends in `$`. In ECMA-262 and the GTS profile, `$` without the multiline flag matches only at the end of the item; Python's `re` lets it match before a final newline too, so an item ending in a newline passes there. Construct's intake also checks in code that no item starts or ends with whitespace, by the derivation's set.

## Worked example

A `skills` candidate with an upper-case `subject_id`:

```text
subject_id = 5D1F2C3A-8B4E-4C6D-9A1B-2C3D4E5F6A7B
subject    = 5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b
field      = skills
value      = ["Bioinformatics", "Machine Learning", "Statistische Genetik", "Größenausschlusschromatographie"]
```

Casefold turns `ß` into `ss`, so the provenance input, with each `\n` a newline byte, is:

```text
5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nskills\nbioinformatics\nmachine learning\nstatistische genetik\ngrössenausschlusschromatographie
```

```text
provenance = sha256:78c5bf9ec52dfaabfd4a6fe45d1624a28ae09f80d0b6aee0c16df2a8d8871cf6
```

The version input is the payload's canonical JSON, these exact ASCII bytes:

```text
{"confidence":0.74,"field":"skills","identity_confidence":0.91,"source_url":"https://example.org/people/jane-doe","trust_tier":"found","value":["Bioinformatics","Machine Learning","Statistische Genetik","Gr\u00f6\u00dfenausschlusschromatographie"]}
```

```text
version = sha256:1ffe2ff0a3c0a6aa53627d942d4cf7d7ac25a713633d75b0bcafa5e26ff95dfa
```

## Conformance vectors

### Edge cases

Each line below is one vector: a payload, its provenance input, its version input and both digests, for a lower-case `subject_id`. The lines are JSON with ASCII escapes, so every input character is exact.

- `composed` and `decomposed` are the same location, `Z\u00fcrich` and `Zu\u0308rich` in the vectors' JSON escapes. They give the same provenance and different versions, because NFKC applies to the provenance input and not to the version input.
- `nfkc_compatibility` has full-width letters and the ligature `ﬁ` (U+FB01), which NFKC maps to plain letters.
- `inner_whitespace` has runs of spaces and tabs inside the value.
- `integral_float` has `confidence` and `identity_confidence` of `1.0`.
- `exponent_float` has a `confidence` of `1e-05`, which the version input writes with an exponent.
- `above_bmp` has a character above U+FFFF, which the version input writes as a surrogate pair.
- `evidence_newline` has a newline in `evidence`, which the version input writes as `\n`.

```text
{"name": "composed", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "location", "value": "Z\u00fcrich", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nlocation\nz\u00fcrich", "provenance": "sha256:a51e1982ce4f2221d37ee7bd4863f8764f9c571118b85895ff69f574b11e53d9", "version_input": "{\"confidence\":0.7,\"field\":\"location\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Z\\u00fcrich\"}", "version": "sha256:f9a4d4cfc8885fead66edaefa4d8ab8510f25c24bd651cfdfd3a7fe8fc4e0a5c"}
{"name": "decomposed", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "location", "value": "Zu\u0308rich", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nlocation\nz\u00fcrich", "provenance": "sha256:a51e1982ce4f2221d37ee7bd4863f8764f9c571118b85895ff69f574b11e53d9", "version_input": "{\"confidence\":0.7,\"field\":\"location\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Zu\\u0308rich\"}", "version": "sha256:286279d2cf3851b7b1c477c875d2165186e46aa34d4b8fa9de16c419a7541ca1"}
{"name": "nfkc_compatibility", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "current_job_title", "value": "\uff28\uff45\uff41\uff44 of \ufb01eld research", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\ncurrent_job_title\nhead of field research", "provenance": "sha256:9fae111f012407f4bb60a2ead4dc5e424ce0cf37b4b08f24644424d4788fa8d6", "version_input": "{\"confidence\":0.7,\"field\":\"current_job_title\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"\\uff28\\uff45\\uff41\\uff44 of \\ufb01eld research\"}", "version": "sha256:c50b9e8821152c4bd83661d1d19366cd5e82171f5e936b9a99b6121f50d3b2c1"}
{"name": "inner_whitespace", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "affiliation", "value": "Swiss   Institute of\t\tBioinformatics", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\naffiliation\nswiss institute of bioinformatics", "provenance": "sha256:c82eff7de5c871e6714e92064f860396ce59fbe4a5cb5814d50d1a735c767165", "version_input": "{\"confidence\":0.7,\"field\":\"affiliation\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Swiss   Institute of\\t\\tBioinformatics\"}", "version": "sha256:b4e66797def7f8cb5d9b73af487e78577c8724df36cb8c41a83e3816ac8aa051"}
{"name": "integral_float", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "linkedin_url", "value": "https://www.linkedin.com/in/jane-doe-example", "confidence": 1.0, "trust_tier": "found", "identity_confidence": 1.0}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nlinkedin_url\nhttps://www.linkedin.com/in/jane-doe-example", "provenance": "sha256:6381dbd604df4024605ebb9518e0b91ca9a4692ca9bbdb8d54bd18fb697a042e", "version_input": "{\"confidence\":1.0,\"field\":\"linkedin_url\",\"identity_confidence\":1.0,\"trust_tier\":\"found\",\"value\":\"https://www.linkedin.com/in/jane-doe-example\"}", "version": "sha256:c35c1dff1cd1e70d4bd724b5701c0bdb8ca8fbee58ea1cc56e183e8456fff961"}
{"name": "exponent_float", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "affiliation", "value": "Swiss Institute of Bioinformatics", "confidence": 1e-05, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\naffiliation\nswiss institute of bioinformatics", "provenance": "sha256:c82eff7de5c871e6714e92064f860396ce59fbe4a5cb5814d50d1a735c767165", "version_input": "{\"confidence\":1e-05,\"field\":\"affiliation\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Swiss Institute of Bioinformatics\"}", "version": "sha256:eac940a9d7cc11ebce92cea50b07bd5538bd7152acbcb7447eccf07cfead78a5"}
{"name": "above_bmp", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "mention", "value": "Named in the \ud842\udfb7\u91ce lab newsletter.", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nmention\nnamed in the \ud842\udfb7\u91ce lab newsletter.", "provenance": "sha256:19710cf75fdd379ec05e215831469e1c0c45d131dab569317802ad87f02f6613", "version_input": "{\"confidence\":0.7,\"field\":\"mention\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Named in the \\ud842\\udfb7\\u91ce lab newsletter.\"}", "version": "sha256:e5860dba9cd19a4b82bbb122658f85fc876f0dd817752a35c09b405624238e02"}
{"name": "evidence_newline", "subject_id": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b", "payload": {"field": "board", "value": "Jane Doe serves on the advisory board.", "confidence": 0.7, "trust_tier": "found", "identity_confidence": 0.91, "evidence": "Advisory board:\nJane Doe (2024)"}, "provenance_input": "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b\nboard\njane doe serves on the advisory board.", "provenance": "sha256:bae96a3f18f46c983c94ecb5657819ab040039b39c3d6c743e010148c220bce5", "version_input": "{\"confidence\":0.7,\"evidence\":\"Advisory board:\\nJane Doe (2024)\",\"field\":\"board\",\"identity_confidence\":0.91,\"trust_tier\":\"found\",\"value\":\"Jane Doe serves on the advisory board.\"}", "version": "sha256:382ab0552e82e1ca38f99b76daf44be820fde83937d886cac1a3d7dee476e2db"}
```
