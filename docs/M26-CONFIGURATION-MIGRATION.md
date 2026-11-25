# M2.6 configuration migration

`--migrate-config --config <path>` explicitly migrates an unversioned (schema v0) TOML file to schema version 1. The loader preserves recognized values and rejects unknown fields with a parse error instead of silently dropping them. Future schema versions are rejected and left untouched.

Before rewriting an older file, Pathweave copies it to `<filename>.toml.bak`, writes a same-directory temporary file, and replaces the original atomically. A current v1 file is validated but not rewritten. If parsing, validation, backup, or replacement fails, the original remains in place.

Existing `--qos-interface-limit INDEX=UPLOAD:DOWNLOAD` remains supported as a temporary startup override. The index is resolved to the adapter GUID for that run and is not written to TOML. Use `[qos.interfaces."GUID"]` for persistent settings. This migration does not infer a GUID from an old display name or index.

```powershell
cargo run -- --migrate-config --config .\Pathweave.toml
```

Schema v0 is the prior unversioned form of the current field set. Other legacy layouts are rejected with a specific unknown-field or type error; they are not auto-rewritten.
