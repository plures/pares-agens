# ADR-0021: Telegram runtime model picker

**Status:** Accepted  
**Date:** 2026-07-25

## Context

Telegram already has inline-keyboard construction and callback-query dispatch. A `/model` picker must show only models actually available to the currently configured providers. A baked-in list is unsafe: provider entitlement, availability, and local Ollama inventory change without a release.

## Decision

`/model` opens a short-lived picker only after asking the configured `ModelPool` to refresh. The pool, rather than Telegram, owns provider configuration and discovery. Discovery is performed at command-open time; callback navigation uses the resulting short-lived server-side snapshot so a click cannot turn into a different selection while pages are being read.

### Live catalogue sources

| Provider kind | Runtime source | Request / config used |
| --- | --- | --- |
| GitHub Copilot | Copilot model-picker API | `GET {ProviderConfig.endpoint}/models`, bearer token from `gh auth token`; response `data[]` |
| OpenAI-compatible | provider models API | `GET {ProviderConfig.endpoint}/models`, bearer token resolved from `ProviderAuth`; response `data[]` |
| Ollama | local live daemon inventory | `GET {ProviderConfig.endpoint}/api/tags`; response `models[]` |
| Custom | configured provider/pool state only | `ProviderConfig` + `ModelPool` entries; no claim of remote discovery where no discovery API is configured |
| Anthropic | configured provider/pool state only | Anthropic exposes no general account model-list endpoint; its entry must be explicitly configured/pool-populated. It is never manufactured by the Telegram UI. |

The exact provider endpoint, authentication source, enabled state, and discovery mode are read at runtime from `~/.pares-radix/config/models.toml` through `ModelPool`; Telegram does not duplicate that config. A failed provider stays represented by pool status, but its stale or invented models are not added to a fresh picker.

## Callback protocol and safety

- Callback data is opaque: `mp:{session}:s:{absolute-index}`, `mp:{session}:p:{page}`, or `mp:{session}:c:0`. Model identifiers remain server-side, avoiding Telegram's 64-byte callback-data limit and disclosure of data not displayed.
- Each session records Telegram user id, chat id, message id, a fixed model snapshot, and a five-minute expiry. A callback must match all three ownership coordinates.
- Foreign, malformed, expired, and already-consumed callbacks are acknowledged but do nothing. Expiry and a process restart are safe no-ops.
- Selecting a model consumes the session and applies the pool's real preference/update operation. No text command is synthesized.
- Page navigation and selection edit the original picker message in place and replace its markup. No new chat messages are emitted for clicks.

## Pagination

The picker exposes a fixed number of models per page (chosen to remain below Telegram keyboard and message-size limits), with Previous/Next and Cancel controls. Button indexes are absolute offsets into the session snapshot, so model names with punctuation or long IDs never affect protocol parsing.

## Consequences

The user sees current provider inventory whenever `/model` is opened, while click handling remains deterministic, owner-scoped, and testable without Telegram transport. The pool remains the only location that knows provider-specific discovery protocols.
