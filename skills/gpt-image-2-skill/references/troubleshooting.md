# Troubleshooting

Decision tree for the most common failures. Always run `--json doctor` first to confirm runtime, provider auth, and retry policy in one shot.

## `runtime_unavailable`

The Node wrapper could not resolve a Rust binary.

Resolution order is: `GPT_IMAGE_2_SKILL_BIN` → skill `scripts/gpt-image-2-skill[.exe]` → `bin/<triple>/` → `PATH` → Tauri App bundled CLI.

Fixes:

- Put the CLI next to the skill: `scripts/gpt-image-2-skill.exe` (Windows) or `scripts/gpt-image-2-skill` (macOS/Linux)
- or set `GPT_IMAGE_2_SKILL_BIN` to an absolute binary path

## Documented command is missing

If a command from the skill docs fails with `unrecognized subcommand`, treat it as a stale CLI runtime first.

Check upstream and local runtime versions:

```bash
npm view gpt-image-2-skill version
command -v gpt-image-2-skill || true
gpt-image-2-skill --version || true
node scripts/gpt_image_2_skill.cjs --version
```

If `/Applications/GPT Image 2.app/Contents/Resources/bin/gpt-image-2-skill` exists but is older than upstream, ask the user before updating or replacing the app bundle. If no app bundle exists and the shell CLI is missing or stale, update the CLI silently:

```bash
npm install -g gpt-image-2-skill@latest
command -v gpt-image-2-skill
gpt-image-2-skill --version
node scripts/selftest.cjs
```

## `auth_missing`

Provider auth absent for the resolved provider.

- OpenAI: export `OPENAI_API_KEY` or pass `--api-key sk-...`
- Codex: ensure `~/.codex/auth.json` exists. The Codex desktop app writes it on first sign-in. `$CODEX_HOME` overrides the directory.

Use `--json auth inspect` to see which provider has `ready: true`.

## `auth_parse_failed`

`auth.json` exists but cannot be parsed. Re-sign-in through Codex desktop, or restore a known-good `auth.json` from backup.

## Codex `401` and `refresh_failed`

A `401` from the Codex endpoint triggers exactly one access-token refresh against `https://auth.openai.com/oauth/token`, then one retry. If refresh itself fails:

- `refresh_failed` with `error.detail.error` containing the underlying message
- common causes: expired refresh token (re-sign-in needed), network blocked, OAuth server outage

## Retries and transient errors

The runtime retries up to `DEFAULT_RETRY_COUNT = 3` times with exponential backoff (`1s → 2s → 4s`). Retried error classes are determined by `should_retry` in `lib.rs`; non-retryable errors fail fast.

Watch retry behavior live with `--json-events` and grep stderr for `"phase":"retry_scheduled"`.

## Image-size rejections

`code: "invalid_argument"` with a size-related message means a custom `WIDTHxHEIGHT` violated one of:

- both edges multiples of 16
- max edge 3840
- max total pixels 8,294,400
- max aspect ratio 3:1

Use `2K` or `4K` aliases when in doubt.

## `transparent_verification_failed`

The transparent pipeline wrote an output image, but the final alpha gate did not pass. Do not deliver the file.

Common causes:

- the source background was not flat
- the matte color appears inside the subject
- the subject touches the image edge
- a checkerboard image is masquerading as transparency
- fully transparent pixels still contain matte-colored RGB values
- strict profile requirements do not match the asset type
- a glow/smoke/glass asset was extracted with chroma instead of dual-background extraction
- black/white dual sources were not aligned

Fixes:

- retry with `--report-dir` or `--keep-sources` so the source image can be inspected
- for AI-generated flat backgrounds, run extraction with `--matte-color auto` so the CLI samples the actual source matte
- use a different matte: `magenta`, `cyan`, `blue`, `green`, `black`, or `white`
- use `--material soft-3d`, `flat-icon`, `sticker`, or `glow` when broad edge behavior matches better than the standard chroma defaults
- generate source prompts that explicitly forbid shadows, gradients, textures, and scenery
- for semi-transparent effects, generate aligned black/white source images and run `transparent extract --method dual`
- choose a profile explicitly: `--profile icon`, `product`, `sticker`, `seal`, `translucent`, `glow`, `shadow`, or `effect`
- pass `--expected-matte-color <color>` during verification when checking chroma residue
- always re-run `transparent verify --profile <profile> --strict` on the final PNG

Use `failure_reasons` from the JSON to pick the retry:

| Reason | Retry |
|---|---|
| `missing_alpha_channel` | run local extraction or reject provider-native transparency |
| `checkerboard_detected` | reject and regenerate with controlled matte |
| `subject_touches_edge` / `effect_touches_edge` | regenerate with larger margin |
| `matte_residue_too_high` | retry with `--matte-color auto`, then try a different source matte color |
| `profile_requires_partial_alpha` | use dual extraction or a translucent/glow/shadow source flow |
| `too_many_stray_pixels` | retry with cleaner matte, or use `sticker`, `seal`, or `effect` if multiple components are intentional |
| `transparent_rgb_not_scrubbed` | re-run extraction so transparent RGB is scrubbed |

## `transparent_input_mismatch`

Dual extraction requires black/white source images with identical dimensions. Regenerate both images with the same `--size`, or use reference-image editing to preserve alignment.

## Moderation refusals (OpenAI)

OpenAI may reject prompts. The runtime surfaces the upstream error verbatim under `error.detail`. Adjust the prompt or set `--moderation low` (where supported by the account) and retry.

## Windows: a new `gpt-image-2-skill.exe` on every image

Prefer `scripts\gpt-image-2-skill.exe` in the skill directory. Codex must enqueue with `--no-wait`, save `job_id`, and poll `jobs status --file tickets.json`. Do not `Start-Process -WindowStyle Hidden`. A single blocking `images edit` is fine for one page. The daemon at `http://127.0.0.1:8787/api` must keep the same pid.

If every page prints `starting shared daemon` (instead of `reusing daemon pid=...`), or `daemon status` pid changes between images, the client is still inside a Windows Job Object that kills the daemon when `& $cli` returns. Fix:

1. Replace `scripts\gpt-image-2-skill.exe` with a build that detaches from the Job (breakaway / WMI). Older binaries spawn the daemon as a child of the client, so it dies at the end of each page.
2. Start once before the loop: `& $cli daemon start`
3. Confirm `& $cli daemon status` reports the same `pid` after page 1 and page 2.
4. Do not `daemon stop` until the batch is done.

`GPT_IMAGE_2_SKIP_DAEMON=1` disables the shared process and is slower; do not use it for a multi-page batch.

## Network and timeout

- `network_error` — transport-level failure (DNS, TLS, connection reset). Retried automatically.
- Hard timeout: `DEFAULT_REQUEST_TIMEOUT = 300s` for image requests, `DEFAULT_REFRESH_TIMEOUT = 60s` for token refresh.

## Self-test

Run the bundled self-test as a smoke check:

```bash
node scripts/selftest.cjs
```

It calls `--json doctor` and `--json auth inspect`, then prints a one-line summary including which providers are ready.
