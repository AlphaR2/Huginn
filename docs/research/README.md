# Research

Design history and lab notes. Not needed to use Huginn; useful to audit it or to see why a choice was made.

| File | What it is |
|---|---|
| [`findings.md`](findings.md) | Every fact we confirmed against Agave's source and real data: pinned versions, byte layouts, surprises, how each was checked. Where it disagrees with `plan.md`, this file wins. |
| [`plan.md`](plan.md) | The original design and the revisions to it. Section numbers (§8.x) are cited from code comments. |
| [`read-layer.md`](read-layer.md) | What Solana's RPC, Yellowstone gRPC and repair protocol give us, and where Huginn's shreds can come from. |
| [`adversarial-report.md`](adversarial-report.md) | An outside review's full findings and the status of each. The short version is in [`../security.md`](../security.md). |

Personal working notes (study plans, checklists, journals) live in `notes/`, which is gitignored.
