# Cross-Platform Agent Architecture

**Component**: Pares Agens - Local-First AI Agent Framework
**Source**: development-guide/design/CROSS-PLATFORM-AGENT-ARCHITECTURE.md
**Status**: Design Phase
**Last Updated**: 2026-02-17

This document defines the architecture for cross-platform agent capabilities that enable Pares Agens to interact with any operating system or device through lightweight capability nodes.

## Problem Statement

Traditional AI agents assume they run on the same OS as the things they need to control. This creates fundamental limitations:

- **WSL2 agents** cannot interact with Windows GUI applications
- **Container-based agents** have no display server access  
- **Cloud agents** cannot interact with local desktops
- **Multi-machine setups** require per-platform agent installations

## Solution: Capability Nodes

Pares Agens separates the **agent brain** (reasoning, memory, conversation) from **platform capabilities** (screen, GUI, apps, sensors). The agent core runs anywhere. Capabilities are provided by lightweight **nodes** that run on each target platform.

```
┌─────────────────────────────────────────────────────────┐
│              Pares Agens Core (runs anywhere)            │
│   - Reasoning, planning, memory, conversation             │
│   - Platform-agnostic (Linux, Mac, container, cloud)     │
│   - Requests capabilities, doesn't assume OS access      │
└──────────────────────┬──────────────────────────────────┘
                       │ Pares Protocol (WebSocket/Hyperswarm)
          ┌────────────┼────────────┬────────────┐
          ▼            ▼            ▼            ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ Windows  │ │  macOS   │ │  Mobile  │ │ Browser  │
    │ Desktop  │ │ Desktop  │ │   Node   │ │   Node   │
    │   Node   │ │   Node   │ │(iOS/And) │ │(existing)│
    └──────────┘ └──────────┘ └──────────┘ └──────────┘
    Win32, DXGI   AppleScript   Touch,       Playwright
    UI Automation Accessibility  camera,
    Shell, Audio  AVFoundation   sensors
```

### Design Principles

1. **Capabilities as nodes, not assumptions** - `screen_capture`, `gui_click`, `app_launch` are capabilities that nodes advertise. The agent requests capabilities; the protocol routes to the right node.

2. **Discovery** - Nodes find each other via:
   - Localhost (WSL ↔ Windows host, same machine)
   - Hyperswarm (home network, P2P)
   - Tailscale (remote, across networks)
   No manual configuration required.

3. **Same protocol everywhere** - Whether the node is on Windows, Mac, a phone, or a Raspberry Pi — the protocol is identical.

4. **Capability negotiation** - Node connects → advertises capabilities → agent discovers available actions dynamically.

5. **Security** - Nodes require pairing. Sensitive capabilities require explicit approval per-session or per-action.

6. **Lightweight** - Nodes are small, single-purpose binaries. No AI inference on the node — that stays in the agent core.

## Windows Desktop Node

### Capabilities

| Capability | API | What It Unlocks |
|-----------|-----|-----------------|
| `screen_capture` | DXGI Desktop Duplication / BitBlt | Screenshots, screen recording, OCR |
| `gui_automation` | Windows UI Automation API | Click, type, read UI elements, find controls |
| `app_launch` | `ShellExecute` / `CreateProcess` | Open any Windows app with arguments |
| `window_management` | Win32 `EnumWindows`, `SetWindowPos` | List, focus, resize, minimize, close windows |
| `clipboard` | Win32 clipboard API | Read/write text, images, files |
| `file_access` | Direct NTFS | Read/write files without `/mnt/c/` overhead |
| `audio_capture` | WASAPI (loopback) | Record system audio output |
| `audio_playback` | WASAPI / MediaFoundation | Play audio files |
| `notifications` | Windows Toast Notification API | Show rich notifications |
| `process_management` | Win32 Process API | List, start, stop processes |
| `registry` | Win32 Registry API | Read system/app configuration |
| `input_simulation` | SendInput API | Keyboard/mouse input at OS level |

### Architecture

```
┌──────────────────────────────────────────────────────┐
│                    Windows Host                       │
│                                                       │
│  ┌─────────────────────────────────────────────────┐ │
│  │         pares-agens-windows (tray app)          │ │
│  │                                                   │ │
│  │  ┌───────────┐  ┌───────────┐  ┌─────────────┐ │ │
│  │  │  Screen   │  │   GUI     │  │    App      │ │ │
│  │  │  Capture  │  │Automation │  │  Launcher   │ │ │
│  │  │  (DXGI)   │  │  (UIA)    │  │(ShellExec)  │ │ │
│  │  └─────┬─────┘  └─────┬─────┘  └──────┬──────┘ │ │
│  │        │               │               │         │ │
│  │  ┌─────▼───────────────▼───────────────▼──────┐ │ │
│  │  │           Capability Router                 │ │ │
│  │  │  - Receives commands from agent             │ │ │
│  │  │  - Routes to appropriate capability module  │ │ │
│  │  │  - Returns results (screenshots, status)    │ │ │
│  │  └────────────────────┬───────────────────────┘ │ │
│  │                       │                          │ │
│  │  ┌────────────────────▼───────────────────────┐ │ │
│  │  │           Transport Layer                   │ │ │
│  │  │  - WebSocket server (localhost:18790)       │ │ │
│  │  │  - Hyperswarm (optional, for remote)        │ │ │
│  │  │  - Noise encryption for all connections     │ │ │
│  │  └────────────────────────────────────────────┘ │ │
│  │                                                   │ │
│  │  System Tray: [🟢 Connected to Pares Agens]      │ │
│  └─────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────┘
         │
         │ WebSocket / Hyperswarm
         │
┌────────▼─────────────────────────────────────────────┐
│               Pares Agens Core                        │
│  ┌─────────────────────────────────────────────────┐ │
│  │     Agent Runtime + PluresDB Memory              │ │
│  │  - Agent logic, memory (pluresLM), AI inference  │ │
│  │  - Sends commands: "screenshot", "click", "type" │ │
│  │  - Receives results: images, status, UI trees    │ │
│  └─────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────┘
```

### Command Protocol

JSON-RPC over WebSocket/Hyperswarm. Every command is a request-response pair.

```json
// Agent → Node: Take a screenshot
{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "screen.capture",
    "params": {
        "monitor": 0,
        "format": "png",
        "region": null
    }
}

// Node → Agent: Screenshot result
{
    "jsonrpc": "2.0",
    "id": 1,
    "result": {
        "width": 2560,
        "height": 1440,
        "format": "png",
        "data": "<base64-encoded-png>",
        "timestamp": "2026-02-17T18:30:00Z"
    }
}

// Agent → Node: Click a UI element
{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "gui.click",
    "params": {
        "strategy": "uia",
        "selector": {
            "name": "Record",
            "controlType": "Button",
            "automationId": "RecordButton"
        }
    }
}
```

### Capability Advertisement

On connection, the node sends its capability manifest:

```json
{
    "jsonrpc": "2.0",
    "method": "node.hello",
    "params": {
        "nodeId": "windows-desktop-kbh9flow",
        "platform": "windows",
        "version": "0.1.0",
        "hostname": "KBH9Flow",
        "capabilities": [
            {
                "name": "screen.capture",
                "description": "Capture screenshots of any monitor",
                "params": { "monitor": "number", "format": "png|jpg", "region": "object?" }
            },
            {
                "name": "gui.click",
                "description": "Click a UI element by UIA selector",
                "params": { "selector": "object" }
            },
            {
                "name": "app.launch",
                "description": "Launch a Windows application",
                "params": { "path": "string", "args": "string[]?" }
            }
        ]
    }
}
```

## Implementation

### Technology Stack

**Core Agent**: Rust (aligns with PluresDB/Hyperswarm ecosystem)
**Windows Node**: Rust with `windows-rs` crate for Win32 API access
**macOS Node**: Rust with macOS frameworks via `objc` bindings
**Mobile Nodes**: Platform native (Swift for iOS, Kotlin for Android)

### Dependencies

```toml
# Core agent
[dependencies]
tokio = { version = "1", features = ["full"] }
plures-db = "0.1"
hyperswarm-rs = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
noise-protocol = "0.2"

# Windows node additional
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = [
    "Win32_UI_Accessibility",
    "Win32_Graphics_Dxgi", 
    "Win32_System_Com",
    "Win32_UI_WindowsAndMessaging",
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_System_Clipboard",
    "Win32_Media_Audio",
    "Win32_UI_Shell",
    "Win32_System_Threading",
] }
tray-item = "0.10"
```

## Security Model

1. **Node Pairing**: First connection requires manual approval (like OpenClaw nodes)
2. **Capability Permissions**: Sensitive operations require per-session or per-action approval
3. **Transport Security**: All connections encrypted via Noise protocol
4. **Sandboxing**: Nodes run with minimal required privileges
5. **Audit Trail**: All agent actions logged in PluresDB for review

## Discovery & Connectivity

### Local Discovery
- **Same machine**: WebSocket on localhost
- **Local network**: Hyperswarm topic-based discovery
- **NAT traversal**: UDP hole-punching via Hyperswarm

### Remote Discovery  
- **Tailscale**: Cross-network connectivity
- **Relay nodes**: WebSocket fallback for restrictive networks
- **Static config**: Manual IP/port configuration for enterprise

---

*This architecture enables Pares Agens to provide unified AI agent capabilities across all platforms while maintaining security, privacy, and local-first operation.*