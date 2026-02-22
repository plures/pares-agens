# Remote Device Security Management

**Component**: Pares Agens — Remote Device Security  
**Source**: development-guide/design/REMOTE-DEVICE-SECURITY.md  
**Status**: Design Phase  
**Last Updated**: 2026-02-22

This document defines the architecture for remote device security management in Pares Agens — a capability that goes far beyond standard "Find My Device" because pares-agens maintains a persistent agent with root-level Pares Manus node access on each enrolled device.

## Overview

When a device is lost or stolen, the device owner can issue security commands from any other device or web frontend. Pares Agens relays those commands over the encrypted Hyperswarm P2P mesh to the Pares Manus node running on the target device, which then executes platform-native operations and reports back.

```
User (from any device or web frontend):
  "My phone was stolen. Lock it down."

pares-agens:
  1. Identifies target node via PluresDB mesh
  2. Sends commands via Hyperswarm (encrypted P2P)
  3. Target's pares-manus node executes:
     - Change lock PIN
     - Display "This device has been reported stolen" on lock screen
     - Encrypt storage (if not already enabled)
     - Begin GPS tracking
     - Capture photo from front camera (if Tier 4 was opted in)
  4. Reports back with location + photo + audit record
```

## Security Tiers

Capabilities are divided into four tiers of increasing sensitivity. Each tier builds on the previous. Tier 4 requires explicit opt-in during device setup.

### Tier 1: Locate & Alert

Passive and non-destructive. Always available after device enrollment.

| Capability | Description | Platforms |
|---|---|---|
| `security.locate` | GPS location on demand or continuous polling | iOS, Android |
| `security.play_sound` | Play loud alert sound, even if device is silenced | All |
| `security.lock_screen_message` | Display a custom message on the lock screen | All |
| `security.vibrate` | Trigger a vibration pattern | iOS, Android |

### Tier 2: Secure

Actively locks down the device. Requires device-owner authorization.

| Capability | Description | Platforms |
|---|---|---|
| `security.force_lock` | Immediately lock the device | All |
| `security.change_password` | Change OS login password | Android, Windows, macOS, Linux |
| `security.reset_pin_biometric` | Reset PIN and biometric credentials | Android |
| `security.disable_usb` | Block new USB connections | Android, Windows, macOS, Linux |
| `security.disable_bluetooth` | Turn off Bluetooth radio | All |
| `security.wipe_browser_sessions` | Clear browser cookies and active sessions | All |

### Tier 3: Protect Data

Destructive or irreversible actions. Each action is individually confirmed and recorded in the audit log.

| Capability | Description | Platforms |
|---|---|---|
| `security.encrypt_disk` | Trigger full-disk encryption (FileVault / BitLocker / LUKS) | Android, Windows, macOS, Linux |
| `security.wipe_local_db` | Wipe PluresDB local data (cloud replica retained) | All |
| `security.revoke_api_keys` | Revoke API keys and tokens stored on the device | All |
| `security.clear_clipboard` | Clear clipboard and recent file history | All |

### Tier 4: Investigate

Opt-in only. Must be explicitly enabled during device setup — not after a theft event. A visible notification is shown on the device after 24 hours (delayed to allow theft recovery without tipping off a thief immediately).

| Capability | Description | Platforms |
|---|---|---|
| `security.camera_capture` | Capture a photo from front or rear camera | Android, Windows, macOS |
| `security.microphone_record` | Record a short audio clip | Android, Windows, macOS |
| `security.screen_capture` | Capture the current screen state | All |
| `security.network_log` | Log active network connections and current Wi-Fi | All |
| `security.geofence_alert` | Notify owner if the device moves outside a defined area | iOS, Android |

## Platform Matrix

| Feature | iOS | Android | Windows | macOS | Linux |
|---|---|---|---|---|---|
| GPS tracking | ✅ | ✅ | ❌ | ❌ | ❌ |
| Play sound | ✅ | ✅ | ✅ | ✅ | ✅ |
| Lock screen message | ✅ | ✅ | ✅ | ✅ | ✅ |
| Force lock | ✅ | ✅ | ✅ | ✅ | ✅ |
| Password change | ❌* | ✅ | ✅ | ✅ | ✅ |
| Disable USB/Bluetooth | ❌* | ✅ | ✅ | ✅ | ✅ |
| Disk encryption | ❌* | ✅ | ✅ | ✅ | ✅ |
| Wipe browser sessions | ✅ | ✅ | ✅ | ✅ | ✅ |
| Camera capture | ❌* | ✅ | ✅ | ✅ | ❌ |
| Mic recording | ❌* | ✅ | ✅ | ✅ | ❌ |
| Geofence alerts | ✅ | ✅ | ❌ | ❌ | ❌ |

*iOS sandbox restrictions limit some capabilities — an MDM profile may be required for full support.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│              Pares Agens Core (any device)               │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │         Security Command Dispatcher             │   │
│  │  - Resolves target node from PluresDB mesh      │   │
│  │  - Enforces tier authorization rules            │   │
│  │  - Writes audit record before dispatching       │   │
│  │  - Awaits confirmation + result from node       │   │
│  └────────────────────┬────────────────────────────┘   │
│                       │ JSON-RPC over Hyperswarm         │
└───────────────────────┼─────────────────────────────────┘
                        │ (Noise-encrypted P2P)
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
   ┌────────────┐ ┌──────────┐ ┌──────────┐
   │  Android   │ │  macOS   │ │ Windows  │
   │Pares Manus │ │Pares Man.│ │Pares Man.│
   │    Node    │ │   Node   │ │   Node   │
   └────────────┘ └──────────┘ └──────────┘
   Camera, GPS,    FileVault,    BitLocker,
   PIN reset,      AV capture,   USB policy,
   Android DPM     Keychain      Credential Mgr
```

### Command Flow

1. **Authorization check** — The dispatcher verifies the requesting identity owns the target device (PluresDB device registry).
2. **Audit pre-write** — An audit entry is written to PluresDB _before_ the command is sent. This ensures no command is executed without a record, even if the agent crashes mid-flight.
3. **Dispatch** — The command is sent to the target node over Hyperswarm (Noise-encrypted).
4. **Execution** — The Pares Manus node on the target device executes the platform-native operation.
5. **Result** — The node responds with a result payload (location fix, photo bytes, success/failure status).
6. **Audit close** — The audit entry is updated with the result and a completion timestamp.

## Command Protocol

Security commands follow the same JSON-RPC protocol used by all Pares Manus nodes. The `security.*` namespace is reserved for this feature.

```json
// Agent → Node: Force lock device
{
    "jsonrpc": "2.0",
    "id": "sec-001",
    "method": "security.force_lock",
    "params": {
        "requestId": "req-a1b2c3",
        "authorizedBy": "did:plures:owner-key-fingerprint",
        "auditId": "audit-xyz789"
    }
}

// Node → Agent: Confirmation
{
    "jsonrpc": "2.0",
    "id": "sec-001",
    "result": {
        "success": true,
        "executedAt": "2026-02-22T09:15:42Z",
        "platform": "android",
        "details": "Device locked via DevicePolicyManager"
    }
}
```

```json
// Agent → Node: Request GPS location
{
    "jsonrpc": "2.0",
    "id": "sec-002",
    "method": "security.locate",
    "params": {
        "requestId": "req-b2c3d4",
        "authorizedBy": "did:plures:owner-key-fingerprint",
        "auditId": "audit-abc123",
        "mode": "single"
    }
}

// Node → Agent: Location result
{
    "jsonrpc": "2.0",
    "id": "sec-002",
    "result": {
        "latitude": 55.6761,
        "longitude": 12.5683,
        "accuracyMeters": 12.0,
        "altitude": 14.0,
        "timestamp": "2026-02-22T09:15:50Z",
        "provider": "gps"
    }
}
```

```json
// Agent → Node: Capture front camera photo (Tier 4)
{
    "jsonrpc": "2.0",
    "id": "sec-003",
    "method": "security.camera_capture",
    "params": {
        "requestId": "req-c3d4e5",
        "authorizedBy": "did:plures:owner-key-fingerprint",
        "auditId": "audit-def456",
        "camera": "front",
        "format": "jpeg"
    }
}

// Node → Agent: Photo result
{
    "jsonrpc": "2.0",
    "id": "sec-003",
    "result": {
        "format": "jpeg",
        "widthPx": 1280,
        "heightPx": 960,
        "data": "<base64-encoded-jpeg>",
        "capturedAt": "2026-02-22T09:15:55Z"
    }
}
```

## Capability Advertisement

The Pares Manus node advertises its security capabilities during the `node.hello` handshake. The `requiresConsent` field signals that Tier 4 capabilities need prior opt-in recorded in PluresDB before they will execute.

```json
{
    "jsonrpc": "2.0",
    "method": "node.hello",
    "params": {
        "nodeId": "android-phone-a1b2c3",
        "platform": "android",
        "version": "0.1.0",
        "hostname": "Pixel-9-Pro",
        "capabilities": [
            {
                "name": "security.locate",
                "tier": 1,
                "description": "GPS location fix (single or continuous)",
                "requiresConsent": false,
                "params": { "mode": "single|continuous", "intervalSeconds": "number?" }
            },
            {
                "name": "security.force_lock",
                "tier": 2,
                "description": "Lock device immediately via DevicePolicyManager",
                "requiresConsent": false,
                "params": {}
            },
            {
                "name": "security.encrypt_disk",
                "tier": 3,
                "description": "Trigger full-disk encryption",
                "requiresConsent": false,
                "params": {}
            },
            {
                "name": "security.camera_capture",
                "tier": 4,
                "description": "Capture photo from front or rear camera",
                "requiresConsent": true,
                "params": { "camera": "front|rear", "format": "jpeg|png" }
            },
            {
                "name": "security.microphone_record",
                "tier": 4,
                "description": "Record a short audio clip",
                "requiresConsent": true,
                "params": { "durationSeconds": "number", "format": "opus|wav" }
            }
        ]
    }
}
```

## Security & Privacy Model

### Authorization

Every security command is authorized by a cryptographic signature from the device owner's Pares identity key (DID). The Pares Manus node verifies the signature before executing any command. Commands signed by an unrecognized key are rejected and the attempt is logged.

### Audit Log

Every remote security command generates an immutable audit record in PluresDB:

```typescript
interface SecurityAuditRecord {
    auditId: string;              // Unique audit entry ID
    requestId: string;            // Idempotency key from the caller
    targetNodeId: string;         // Which device received the command
    command: string;              // e.g. "security.force_lock"
    tier: 1 | 2 | 3 | 4;
    authorizedBy: string;         // DID of the owner who issued the command
    issuedAt: string;             // ISO 8601 — when the command was sent
    executedAt?: string;          // ISO 8601 — when the node confirmed execution
    result?: "success" | "failure" | "pending";
    resultDetail?: string;        // Human-readable outcome
    attachments?: string[];       // PluresDB refs to photos, location fixes, etc.
}
```

Audit records are stored in the owner's PluresDB topic and are write-once (append-only CRDT). They cannot be deleted retroactively — only the owner's private key can sign new entries, and existing entries cannot be modified.

### Tier 4 Opt-In

Tier 4 capabilities (camera, microphone, screen capture) require explicit consent recorded in PluresDB **before any theft event occurs**. The consent record is signed with the device owner's key during setup:

```typescript
interface Tier4ConsentRecord {
    deviceNodeId: string;          // Which device this consent applies to
    grantedBy: string;             // Owner DID
    grantedAt: string;             // ISO 8601
    capabilities: string[];        // Which Tier 4 capabilities are enabled
    notificationDelayHours: number; // Default: 24 — delayed visible notification
    expiresAt?: string;            // Optional expiry
}
```

Without a valid consent record, the Pares Manus node will refuse all Tier 4 commands and log the refusal.

### Delayed Notification (Tier 4)

When a Tier 4 action executes, the device displays a visible notification after a configurable delay (default: 24 hours). This delay is long enough to allow a legitimate theft recovery without alerting the thief, but short enough to ensure the device owner (or anyone else who later holds the device) learns that surveillance occurred.

### Kill Switch

The device owner can permanently revoke all remote security access from the device itself by deleting the pairing record from the local Pares Manus node. This revocation is broadcast to PluresDB and takes effect immediately. Once revoked, the device will refuse all remote security commands regardless of signature validity, until re-enrolled with manual confirmation on the device.

### Offline Resilience

Security commands are queued in PluresDB if the target device is currently offline. When the device reconnects to Hyperswarm, pending commands are delivered in order. Commands with a `deadline` parameter are discarded if the deadline passes before delivery.

## Enrollment

Device enrollment for remote security management is a deliberate, multi-step process:

```bash
# On the device to be enrolled
pares manus enroll --security-tier=3

# To enable Tier 4 (surveillance capabilities) — explicit additional step
pares manus enable-tier4 \
    --capabilities="camera_capture,microphone_record" \
    --notification-delay=24h
```

Enrollment requires physical access to the device. The owner's identity key must sign the enrollment record; it cannot be performed remotely.

## PluresDB Integration

### Device Registry

Each enrolled device has a registry entry in the owner's PluresDB topic:

```typescript
interface SecurityDeviceRecord {
    nodeId: string;               // Pares Manus node ID
    platform: "ios" | "android" | "windows" | "macos" | "linux";
    hostname: string;
    enrolledAt: string;           // ISO 8601
    enrolledBy: string;           // Owner DID (must match command signer)
    securityTier: 1 | 2 | 3 | 4; // Highest tier enabled
    tier4Consent?: Tier4ConsentRecord;
    lastSeenAt?: string;          // ISO 8601 — last Hyperswarm connection
    lastKnownLocation?: {
        latitude: number;
        longitude: number;
        timestamp: string;
    };
}
```

### Pares Nuvem Relay

When a target device cannot be reached directly over Hyperswarm (e.g., offline or behind a restrictive network), commands are relayed through Pares Nuvem (the cloud replica component). Pares Nuvem acts as a message queue: it stores encrypted commands and delivers them when the device reconnects. Pares Nuvem never decrypts command payloads — it handles only opaque ciphertext.

## Related Components

- **[Pares Manus](https://github.com/plures/pares-manus)** — Capability nodes that execute platform-native security operations on each device
- **[Pares Nuvem](https://github.com/plures/pares-nuvem)** — Cloud relay for command delivery when devices are not directly reachable
- **[PluresDB](https://github.com/plures/pluresdb)** — Append-only CRDT data store used for device registry and audit log
- **[pluresLM Memory Attribution](https://github.com/plures/plures/issues/37)** — All security commands attributed to the originating identity in memory

---

*Remote device security management extends the Pares Agens capability node architecture with a dedicated security tier system, strong audit guarantees, and explicit consent requirements — providing enterprise-grade lost/stolen device protection while preserving user privacy and control.*
