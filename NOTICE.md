# Attribution and binary distribution

This Rust port derives behavior, embedded dashboard markup, model catalog data and pricing snapshots from https://github.com/ardeyouxipianyi/workbuddy2api-hub at commit `477ba36ceea78ec4252c47a68e94a5a1612bab0b` under the repository's MIT license (`LICENSE`, Copyright (c) 2026). The dashboard and pricing snapshot are vendored in `assets/`. Retain that license and this attribution with source or binary distributions.

The static executable also links third-party Rust crates and OpenSSL. Exact resolved versions are recorded in `Cargo.lock`; each dependency retains its own license. Before publicly redistributing a packaged executable/container, generate and review a complete dependency-license inventory, including OpenSSL's Apache-2.0 notices. No claim of completed license-compliance review or public release is made here.

WorkBuddy, CodeBuddy, OpenAI and Anthropic names identify upstream services or protocol compatibility; this port is not affiliated with or endorsed by those providers. Use only accounts and services you are authorized to access, within their applicable terms and rate limits.
