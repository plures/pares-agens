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

## IP Protection — Microsoft CELA Process

### What Exists
- **Invention Disclosure Form** (CELA/Anaqua portal)
- **CELA Patent Team** review determines ownership
- **Assignment** only occurs if CELA proceeds AND you sign during patent filing

### What Does NOT Exist
- ❌ No "carve-out" form
- ❌ No self-declared invention exemption
- ❌ No manager-approved IP waiver
- ❌ Side-project COI disclosure does NOT change IP ownership

### Action Items (BEFORE Hackathon)
1. [ ] File invention disclosure through CELA portal (Anaqua)
2. [ ] Document: personal time, personal equipment, no Microsoft resources
3. [ ] Git history shows development predates DTMS-related work
4. [ ] Ensure plures LLC is formally registered
5. [ ] Consult with CELA Patent Team for your business unit

### Key Legal Points
- CIIAA assigns inventions created "during employment" AND related to Microsoft business
- CELA evaluates: timing, relationship to business, use of company resources
- Washington RCW 49.44.140 protects personal inventions on own time/equipment
- The tension: "infrastructure AI ops" arguably relates to Microsoft's business
- Resolution: CELA disclosure + legal review is the ONLY path

## Acqui-Hire Intelligence (2024-2026)

### Microsoft's Pattern
- Buys TEAMS, not companies
- Products are abandoned post-hire
- IP is licensed, not transferred
- Investors: mixed outcomes (Inflection = mostly whole, Cove = zero)

### What This Means for You
- No "internal acqui-hire" mechanism for current employees
- Realistic paths:
  1. **License model** — DTMS becomes customer, you keep platform (BEST)
  2. **Internal absorption** — CELA determines Microsoft owns it (WORST)
  3. **Leave → external → re-enter** — standard acqui-hire path (HIGH RISK)
- Path 1 (licensing) is the strategy: demonstrate value at hackathon,
  DTMS adopts via enterprise license, expand from there
