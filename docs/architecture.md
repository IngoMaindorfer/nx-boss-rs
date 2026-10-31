# Architecture and delivery

```mermaid
flowchart LR
    scanner[fi-7300NX] -->|HTTP 20447; HTTPS 20448 on firmware 0T00| caddy[Caddy]
    browser[LAN browser] -->|HTTPS 443| caddy
    caddy -->|internal HTTP 10447| api[nx-boss scanner API]
    caddy -->|internal HTTP 10448| ui[nx-boss admin UI]
    api --> batches[(scan files and metadata)]
    ui --> batches
    api --> worker[Paperless delivery worker]
    worker -->|HTTP API with token| paperless[Paperless-ngx v3]
    ui --> events[(system-events.json)]
```

The Rust service publishes no host port in Compose. Caddy is the only ingress. Scanner and admin routes use separate internal listeners. The physical scanner on firmware `0T00` completed its HTTPS connection check on port 20448 with TLS 1.2 and `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`; a full scan over this path remains to be checked. Scanner HTTP remains available. The admin HTTPS certificate uses DNS-01 for `nx.maindorfer.name`.

```mermaid
stateDiagram-v2
    [*] --> Open: batch created
    Open --> Complete: pages and PDF synced
    Complete --> UploadAttempt: attempt persisted
    UploadAttempt --> TaskKnown: Paperless task ID saved
    UploadAttempt --> Reconcile: response lost / restart
    Reconcile --> TaskKnown: task found by batch filename
    Reconcile --> UploadAttempt: no task, settle delay elapsed
    TaskKnown --> Imported: positive document ID
    TaskKnown --> NeedsAttention: failure or overdue
    NeedsAttention --> TaskKnown: retry or reconciliation
```

The PDF upload is at-least-once after an uncertain response. Paperless has no documented idempotency key for this API; task lookup and duplicate detection reduce but do not eliminate duplicate risk. A batch with no confirmed document ID is retained. The dashboard counts documents and pages from completed **stored** batches, so deletion/archival changes those totals. “Imported” means a Paperless document ID was confirmed. The event browser keeps the latest 200 redacted WARN/ERROR events; container JSON logs remain the complete operational log stream.
