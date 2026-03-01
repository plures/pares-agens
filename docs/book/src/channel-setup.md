# Channel Setup

Configure how you communicate with your agent. Pares Agens supports a local terminal channel
out of the box and a Telegram bot channel for mobile access.

## Local terminal (default)

No configuration required. Run:

```sh
pares agens chat
```

The agent reads from stdin and writes to stdout. Useful for scripting and quick tests.

## Telegram bot

Telegram gives you a mobile-first interface with full message history.

### Step 1 — Create a Telegram bot

1. Open Telegram and start a chat with [@BotFather](https://t.me/BotFather)
2. Send `/newbot`
3. Follow the prompts — choose a name and username
4. BotFather will give you a **bot token** that looks like `123456789:ABCdef...`

### Step 2 — Configure the bot token

Add the token to `~/.config/pares-agens/config.toml`:

```toml
[[channels]]
type  = "telegram"
token = "123456789:ABCdef..."    # or use TELEGRAM_BOT_TOKEN env var
```

> **Security note:** Prefer the environment variable `TELEGRAM_BOT_TOKEN` over storing the
> token in `config.toml`.

### Step 3 — Restrict access (recommended)

Allow only your own Telegram user ID to send messages:

```toml
[[channels]]
type       = "telegram"
token      = "123456789:ABCdef..."
allowed_ids = [987654321]        # your Telegram user ID
```

Find your user ID by messaging [@userinfobot](https://t.me/userinfobot).

### Step 4 — Start the agent

```sh
pares agens start
```

Open Telegram and send a message to your bot. You should receive a reply within a second or two.

### Verifying Telegram is connected

```sh
pares agens status
```

```
✅ Model:    ollama/llama3 (connected)
✅ Memory:   PluresDB local (42 memories)
✅ Channels: local, telegram (@my_assistant_bot)
```

## Multiple channels simultaneously

You can run both channels at once — add more `[[channels]]` entries:

```toml
[[channels]]
type = "local"

[[channels]]
type  = "telegram"
token = "123456789:ABCdef..."
allowed_ids = [987654321]
```

## Channel count limits

| Plan | Max channels |
|---|---|
| Free | 2 |
| Pro | Unlimited |

Adding a third channel on the Free plan will return an error at startup. See
[Pro Features](pro-features.md) to remove this limit.

## Coming soon

| Channel | Status |
|---|---|
| Telegram | ✅ Available |
| Local terminal | ✅ Available |
| Signal | 🗓 Planned |
| Discord | 🗓 Planned |
| Slack | 🗓 Planned |
| Web UI | 🗓 Planned |
