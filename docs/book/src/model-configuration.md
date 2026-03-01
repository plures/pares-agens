# Model Configuration

Connect Pares Agens to any OpenAI-compatible model endpoint — local (Ollama, llama.cpp) or
cloud (OpenAI, Anthropic-compatible, custom).

## Configuration file

All model settings live in `~/.config/pares-agens/config.toml` under the `[model]` section.

## Ollama (local, recommended for getting started)

```toml
[model]
provider = "ollama"
base_url = "http://localhost:11434"
model    = "llama3"
```

Pull the model first:

```sh
ollama pull llama3
```

Any model available in Ollama works — `llama3`, `mistral`, `phi3`, `qwen2.5`, etc.

## OpenAI

```toml
[model]
provider  = "openai"
base_url  = "https://api.openai.com/v1"
model     = "gpt-4o"
api_key   = "sk-..."          # or set OPENAI_API_KEY env var
```

> **Security note:** Prefer the environment variable `OPENAI_API_KEY` over storing the key in
> `config.toml`. The config file is stored in plaintext.

## Anthropic (via OpenAI-compatible proxy)

Anthropic's API is not natively OpenAI-compatible, but tools like
[litellm](https://github.com/BerriAI/litellm) expose a compatible endpoint:

```sh
litellm --model claude-3-5-sonnet-20241022
```

```toml
[model]
provider = "openai"
base_url = "http://localhost:4000"
model    = "claude-3-5-sonnet-20241022"
```

## Custom / self-hosted endpoint

Any server that implements the OpenAI chat completions API works:

```toml
[model]
provider = "openai"
base_url = "https://my-internal-llm.example.com/v1"
model    = "my-model"
api_key  = "internal-secret"
```

## Local Qwen3 cluster (DMR, high-performance)

For maximum performance with a multi-node Mac Mini cluster running Qwen3:

```toml
[model]
provider = "openai"
base_url = "http://192.168.1.100:8080/v1"  # primary node
model    = "qwen3-235b-a22b"

[model.dmr]
# Distributed Model Routing — spread inference across multiple nodes
enabled  = true
nodes    = [
  "http://192.168.1.100:8080",
  "http://192.168.1.101:8080",
  "http://192.168.1.102:8080",
]
strategy = "round-robin"   # or "least-loaded"
```

> **Pro feature:** Multi-node DMR routing requires a Pro licence. See [Pro Features](pro-features.md).

## All model options

| Key | Default | Description |
|---|---|---|
| `provider` | `"ollama"` | `"ollama"` or `"openai"` |
| `base_url` | `"http://localhost:11434"` | API base URL |
| `model` | `"llama3"` | Model name |
| `api_key` | `""` | API key (prefer env var) |
| `timeout_secs` | `120` | Request timeout in seconds |
| `max_tokens` | `4096` | Maximum tokens per response |
| `temperature` | `0.7` | Sampling temperature (0.0–2.0) |
| `system_prompt` | `""` | Prepended system prompt |

## Switching models at runtime

```sh
pares agens config set model.model qwen2.5:14b
pares agens restart
```

Or update `config.toml` directly and restart the agent.

## Verifying the model connection

```sh
pares agens status
```

```
✅ Model: ollama/llama3 @ localhost:11434 (connected, 120ms latency)
```

If the model is unreachable:

```
❌ Model: ollama/llama3 @ localhost:11434 (connection refused)
```

Check that Ollama (or your model server) is running:

```sh
ollama serve
```
