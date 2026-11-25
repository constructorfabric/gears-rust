# Query forwarded as sent

## Setup

Create an HTTP upstream (example: `alias=api.example.com`) and a route on it:

```json
{
  "match": {
    "http": {
      "methods": ["GET"],
      "path": "/search",
      "query_allowlist": ["q", "a", "b", "empty", "flag", "path", "lower", "upper", "n", "bad", "my key"]
    }
  }
}
```

## Scenario A: the upstream gets the client's bytes

Send each query below, raw (an HTTP client library may re-encode it before it leaves):

```http
GET /api/oagw/v1/proxy/api.example.com/search?q=a%2Bb&q=a+b&q=a%20b HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

| Query | Covers |
|---|---|
| `q=a%2Bb&q=a+b&q=a%20b` | `%2B` and `+` stay apart; `%20` is not turned into `+` |
| `b=2&a=1&b=1` | order and repeated keys |
| `empty=&flag&flag=` | empty values; a key without `=` keeps no `=` |
| `path=/x/y?z:@!$'()*,` | reserved characters left unescaped stay unescaped |
| `lower=%c3%a9&upper=%C3%A9` | the case of escapes |
| `q=a%26b%3Dc` | an escaped `&` and `=` stay escaped |
| ``q=a\|b{c}[d]\e^f`g&n=%C3%A9&n=é&bad=%zz`` | characters the gateway used to escape (`\|`, `{`, `}`, `[`, `]`, `\`, `^`, backtick), raw UTF-8 bytes and a malformed escape stay as sent |

A literal `;` is not forwarded as sent: see Scenario D.

### Expected behavior

- `200`, `X-OAGW-Error-Source: upstream`.
- The upstream's request line carries `/search?` followed by the query exactly as sent.
- A bare `?` with nothing after it is forwarded too (Rust test; the e2e mock cannot tell `?` from no query).
- The gateway no longer escapes the characters of the last row. A strict upstream (Tomcat, for one) may reject them with 400; the gateway forwards them anyway, as the client sent them.

## Scenario B: the allowlist checks decoded names

Allowlist names are compared with the parameter names decoded as `application/x-www-form-urlencoded`.

| Query | Result |
|---|---|
| `my%20key=v` | `200`; the upstream gets `my%20key=v` |
| `my+key=v` | `200`; the upstream gets `my+key=v` |
| `my%2520key=v` | `400` `QUERY_NOT_ALLOWED` (`my%20key` is not allowlisted), `X-OAGW-Error-Source: gateway`, not forwarded |
| `q=1&debug=1` | `400` `QUERY_NOT_ALLOWED`, not forwarded |

Because names are decoded, allowlisting `my key` also admits `my+key`, which an upstream that reads the query per RFC 3986 (`+` is a plus) sees as the name `my+key`.

## Scenario C: a transform that changes the query

A transform plugin sees the decoded pairs. When it changes them (adds, removes or edits a pair), the upstream gets the pairs form-encoded: `q=a%20b` plus an added `added=1` goes out as `q=a+b&added=1`. A transform that leaves the pairs as they were keeps the client's bytes.

Every pair is encoded again from its decoded form, the ones the transform left alone too: they lose their original encoding, and an invalid escape such as `%FF` decodes to U+FFFD and goes out as `%EF%BF%BD`.

Rust test only: no built-in transform plugin changes the query, and the e2e suite cannot register one.

## Scenario D: a literal `;` goes out encoded

The allowlist splits pairs on `&` only, so `model=x;admin=1` is the one parameter `model` with the value `x;admin=1`. Rack 2, older Python `parse_qs` and older Go `net/url` also split on `;` and would read an `admin` the allowlist never checked. A query with a literal `;` that the allowlist admits is therefore sent as its decoded pairs, form-encoded, as in Scenario C. The request is not rejected.

Route with `"query_allowlist": ["model"]`:

| Query | Result |
|---|---|
| `model=x;admin=1` | `200`; the upstream gets `model=x%3Badmin%3D1` |
| `model=x%3Badmin%3D1` | `200`; no literal `;`, the upstream gets the query as sent |

Route with `"query_allowlist": []`: `model=x;admin=1` is `400` `QUERY_NOT_ALLOWED`, `X-OAGW-Error-Source: gateway`, not forwarded. An empty allowlist admits no parameter, so a query with a `;` is never forwarded as sent.

## What to check

- The query is never decoded and re-encoded on its way through, unless a transform changed it or it has a literal `;`.
- The auth plugin's signing phase ([ADR-0023](../../../docs/ADR/0023-request-signing.md)) sees the query that will be sent: the client's bytes when nothing changed it, the form-encoded pairs otherwise (Rust tests).
