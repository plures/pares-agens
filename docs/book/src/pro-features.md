# Pro Features

Pares Agens is free forever for personal, single-machine use. **Pro** adds multi-device sync,
cloud backup, and removes the channel limit.

## What Pro unlocks

| Feature | Free | Pro |
|---|---|---|
| Local agent (1 machine) | ✅ | ✅ |
| Unlimited conversations | ✅ | ✅ |
| pluresLM memory (local) | ✅ | ✅ |
| All built-in procedures | ✅ | ✅ |
| MCP tool API | ✅ | ✅ |
| Telegram + local channels | ✅ | ✅ |
| Max channels | 2 | Unlimited |
| P2P memory sync (Hyperswarm) | ❌ | ✅ |
| Pares Nubis cloud backup | ❌ | ✅ |
| Cross-device memory sync | ❌ | ✅ |
| Multi-node DMR routing | ❌ | ✅ |
| Ledger JSON export | ❌ | ✅ |
| Priority email support | ❌ | ✅ |
| Early access to new features | ❌ | ✅ |

## Price

**$9 / month** — cancel any time.

## How to purchase

> **Note:** The Pro purchase flow is not yet available. Join the waitlist on
> [GitHub](https://github.com/plures/pares-agens) to be notified when it opens.

When the purchase flow is live:

1. Run `pares agens pro purchase` — this opens a browser to the checkout page
2. Complete payment
3. A licence key will be emailed to you

## How to activate

Once you have a licence key:

```sh
pares agens pro activate <your-licence-key>
```

This writes the key to `~/.config/pares-agens/licence.key` and restarts the agent.

Verify activation:

```sh
pares agens pro status
```

```
✅ Pro licence active
   Licensee: you@example.com
   Valid until: 2027-03-01
   Features: sync, cloud-backup, unlimited-channels, dmr, ledger-export
```

## P2P memory sync

With Pro, your agent memories sync across all your devices using Hyperswarm's encrypted P2P
network. Add to `config.toml`:

```toml
[sync]
enabled = true
topic   = "my-private-topic-key"   # any secret string shared between your devices
```

All sync traffic is encrypted with the Noise protocol. The `topic` value is hashed before
being announced on the network, so no plaintext topic is ever published.

## Pares Nubis cloud backup

Nubis provides encrypted cloud backup so you can restore your memory database on a new machine:

```toml
[backup]
enabled  = true
provider = "nubis"
```

Backups run automatically every 24 hours and are encrypted client-side before upload.

## Multi-node DMR routing

Distribute model inference across multiple local nodes:

```toml
[model.dmr]
enabled  = true
nodes    = [
  "http://192.168.1.100:8080",
  "http://192.168.1.101:8080",
  "http://192.168.1.102:8080",
]
strategy = "round-robin"
```

See [Model Configuration](model-configuration.md) for full DMR options.

## Ledger JSON export

Export the Praxis decision ledger for external analysis or compliance:

```sh
pares agens ledger export --format json > ledger.json
```

This feature is gated to Pro licences. On the Free plan the command returns an error.

## Frequently asked questions

**Can I use Pro on more than one machine?**
Yes — the licence is per-user, not per-machine. Activate with the same key on each machine.

**What happens if I cancel?**
Your data is never deleted. Pro features are disabled, but the agent continues to work on the
Free plan with local memory and up to 2 channels.

**Is there a student / open-source discount?**
We plan to offer a free Pro licence for open-source maintainers and students. Contact
[support@plures.io](mailto:support@plures.io) with details.
