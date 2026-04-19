# Plures LLC — Commercial Strategy

## IP Protection Checklist

- [ ] **Consult IP attorney** — Washington state RCW 49.44.140 + Microsoft employment agreement review ($500-1000)
- [ ] **File invention disclosure carve-out** with Microsoft legal BEFORE hackathon
- [ ] **Document timeline** — git history proves personal-time, personal-equipment development
- [ ] **Formalize LLC** — EIN, operating agreement, separate bank account
- [ ] **Keep hackathon deliverable = .px files only** — platform runs backstage, never shown as hackathon project
- [ ] **Record all personal hardware purchases** — praxisbot, surface, peripherals

## Licensing Strategy (BSL-1.1)

All plures repos use BSL-1.1:
- Free for non-production / development / testing
- Enterprise license required for production use
- Converts to open source after change date (4 years)
- Prevents cloud providers from offering as managed service

## Revenue Model

### Tier 1: Self-Hosted License
- Enterprise per-cluster annual license
- Includes: pares-agens, PluresDB, praxisc compiler, design-dojo
- Customer runs on their hardware (the DTMS use case)
- Pricing: $50K-$200K/year depending on cluster size

### Tier 2: Managed Service (future)
- Plures-hosted SaaS version
- .px files in browser IDE → deployed to customer infrastructure
- Pricing: per-agent-minute or monthly subscription

### Tier 3: Marketplace
- .px rule packages (like npm packages but for business logic)
- Community creates, we host + curate
- Revenue: commission on paid packages

## Microsoft Engagement Strategy

1. Hackathon → demonstrate value with .px files (Microsoft keeps domain knowledge)
2. DTMS team adopts for internal use → first enterprise customer
3. Other AGC teams see it → expand licensing within Microsoft
4. Azure Local integration → potential OEM deal
5. Azure Marketplace listing → external customers discover it

## What Microsoft Gets vs What Plures Keeps

| Deliverable | Owner |
|---|---|
| .px deployment rules for DTMS/AGC | Microsoft (domain knowledge) |
| Dialtone Sentinel configuration | Microsoft (hackathon project) |
| praxisc compiler | Plures LLC (BSL-1.1) |
| PluresDB runtime | Plures LLC (BSL-1.1) |
| pares-agens agent platform | Plures LLC (BSL-1.1) |
| BitNet inference crate | Plures LLC (BSL-1.1) |
| design-dojo UI framework | Plures LLC (MIT) |
| Hyperswarm sync layer | MIT (upstream) |
