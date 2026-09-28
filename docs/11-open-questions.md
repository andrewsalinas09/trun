# 11 · Open questions

Resolved questions have moved to the [decisions log](decisions.md).

1. **Log storage at extreme volume.** SQLite is the decision (D13). Revisit only if
   real multi-GB logs show a measured problem. The likely mitigation would be
   compressed blobs per chunk of lines inside SQLite, not a second storage system.
