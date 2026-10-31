# Paperless delivery: local operations

nx-boss uses the Paperless HTTP API and stores one receipt per scan batch. This is best-effort delivery, not an exactly-once guarantee. A lost POST response can leave a task undiscoverable; after the settle window nx-boss may send the PDF again. Changing Paperless is outside this approach.

## What nx-boss now records

- Before a new batch can upload, it saves the configured tag IDs and batch-ID custom field in that batch's metadata. Retries keep these fields even after nx-boss restarts with different configuration. Older receipts lack this snapshot; nx-boss pins their current settings before its next Paperless request, but cannot reconstruct settings used by an earlier POST.
- The admin dashboard counts completed Paperless batches needing review. A batch appears there after one hour without confirmation, measured from its latest upload attempt or, if there was none, from batch creation. An explicit delivery error also needs review. The scans page includes every batch needing review even when it is older than the 100 most recent scans.
- Scan details show the batch ID, Paperless task ID when known, upload-attempt count, latest attempt time, and the last error. These are local records, not proof that a Paperless document exists.

## Review an unconfirmed batch

1. Open its scan detail and record the batch ID and any task ID. Keep the staged PDF and `metadata.json` intact.
2. Check the matching Paperless task and the resulting document. Match the exact `<batch-id>.pdf` task filename; a matching name alone is weaker evidence than a task with one confirmed document ID. Check document content if task history is missing.
3. If the document exists but nx-boss has not confirmed it, preserve both sides and investigate before manually uploading anything. nx-boss continues its automatic polling and retry policy; stop that worker before performing a manual recovery that could duplicate a document.
4. If Paperless has no task or document, nx-boss normally retries after its settle window. Check the saved error and network connectivity. Never mark a batch imported based only on an HTTP 2xx POST response: the Paperless task can fail later.

Back up nx-boss scan directories, their `metadata.json` files, and Paperless at coordinated points. After restoring either side to an older point, review all unconfirmed batches and previously confirmed document IDs before resuming automatic delivery. A numeric document ID can refer to a different document after restore. There is no automatic restore fence or global exactly-once claim.
