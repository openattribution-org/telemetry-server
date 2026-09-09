# Specification fixtures

Copied byte-for-byte from the standard's own conformance suite
(`SPUR-Coalition/telemetry`, `tests/valid/` and `tests/invalid/`). They are
the specification's examples, not ours: edit them only by re-copying from
upstream, so a fixture that stops passing here means this server disagrees
with the standard rather than with a local rewrite of it.

| File | Upstream path | What it is |
|---|---|---|
| `session-access-context.json` | `tests/valid/` | A session document carrying the 5.1.3 `data.access_context` container |
| `access-context-identifier-missing-value.json` | `tests/invalid/` | An identifier with a `scheme` and no `value` |
| `access-context-identifiers-not-array.json` | `tests/invalid/` | `identifiers` as a bare string |

The `_test_description` and `_expected_error` members are the upstream
harness's annotations. They are not part of the document format, which is
why this server records them as unrecognised top-level fields rather than
interpreting them.
