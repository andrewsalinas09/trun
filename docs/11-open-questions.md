# 11 · Open questions

1. **Name.** `trun` is a placeholder. It pairs with `rrun`, but may collide with
   existing packages. Check crates.io, npm, and PyPI before publishing.
2. **Relay design.** A Cloudflare Worker plus Durable Object relay: should frames be
   end-to-end encrypted, with the relay only routing ciphertext? Who hosts it?
   Self-deploy through wrangler vs. a shared public instance.
3. **Multi-user.** Is a hub ever shared by a team? That affects auth (user tokens vs.
   OIDC), note authorship, and permissions for remote exec.
4. **Metric volume.** What should happen with very high-frequency metrics (per-batch at
   1 kHz)? Options: agent-side downsampling with min/max/avg buckets, a per-run
   budget, or a different storage format (Parquet segments) for large runs.
5. **Log storage.** Is SQLite OK for multi-GB logs, or should output go to compressed
   segment files with an SQLite index?
6. **TS panel transpiler.** esbuild-wasm (about 10 MB, full TS) vs. sucrase (small,
   strips types only). The lean is toward sucrase.
7. **Rhai vs. alternatives for checks.** Rhai is the current choice. Revisit if its
   performance or ergonomics fall short. Alternatives are Lua (mlua) or CEL for
   pure expressions.
8. **Windows service vs. per-user daemon.** Should the local daemon run as a user
   process (tray-launched) or as a service? GPU and process access differ.
9. **Relation to Claude Code background tasks.** Should `trun run` be recommended
   as the default way an agent launches any long command, for example through a hook
   or a CLAUDE.md rule?
10. **Existing tool overlap.** Borrow from pueue's daemon and queue design where it
    fits. Consider interop (import W&B/TensorBoard logs) rather than competing.
