---
name: gpt-image-2-skill
description: This skill should be used when the user asks to "generate an image", "create a logo", "draw an icon", "edit this photo", "change background to transparent", "remove background", "use GPT image", "use Codex to draw", "用 GPT image 生成图片", "用 Codex 画图", "帮我生成一张图", "改成透明背景", "把这张图编辑一下", or any prompt-to-image or reference-image-edit task that benefits from a structured CLI returning JSON results and JSONL progress events. Supports OpenAI `gpt-image-2` (via `OPENAI_API_KEY` or OpenAI-compatible base URL) and Codex `image_generation` (via `~/.codex/auth.json`) under one command surface, with masks, custom sizes up to 4K, transparent backgrounds, and a raw request escape hatch.
---

Run image generation and editing through one CLI surface that hides provider differences. Skill cwd is this directory. Prefer the CLI shipped next to the skill; the Node wrapper at `scripts/gpt_image_2_skill.cjs` uses that same binary first and otherwise falls back (env override → skill `scripts/` → `bin/<triple>/` → PATH → Tauri App). It does not `cargo run` the repo or download a GitHub release.

## When to use this skill

- Generate or edit an image and capture a structured result an agent can parse.
- Switch between `OPENAI_API_KEY`, an OpenAI-compatible base URL, and Codex `auth.json` without changing command shape.
- Respect shared provider config at `$CODEX_HOME/gpt-image-2-skill/config.json` so CLI, App, and Skill use the same default provider.
- Need final transparent PNG deliverables, masks, custom sizes up to 4K, or raw request bodies.
- Want live progress events (retries, multipart prep, Codex SSE) on stderr while the final JSON lands on stdout.

## Quick start

Always pass `--json` so the result is machine-readable. Add `--json-events` when progress visibility matters.

## Invoke the CLI in this skill directory

Do not invent a path under `%USERPROFILE%\.codex\skills\...` unless that directory is the cwd. First choice is the binary in `scripts/` next to this file:

| OS | First choice | If that file is missing |
|---|---|---|
| Windows | `scripts\gpt-image-2-skill.exe` | `node scripts\gpt_image_2_skill.cjs` |
| macOS / Linux | `scripts/gpt-image-2-skill` | `node scripts/gpt_image_2_skill.cjs` |

Override only with `GPT_IMAGE_2_SKILL_BIN`. The node wrapper prefers the same `scripts/` binary before PATH or a downloaded copy.

## Codex: enqueue, record `job_id`, poll later

This is the required path when Codex (or any agent) generates **more than one** image. Do **not** use `Start-Process`, `Start-Job`, `-WindowStyle Hidden`, or `Start-Sleep` as a job runner. Those drop JSON, lose exit codes, and can kill the daemon when the parent script exits.

Each page still has its own prompt, `--ref-image`, and `--out`. Use absolute paths. Prefer `--provider auto` or a name from `config inspect` (for example `ailili`). Do not pass `--provider openai` or `--provider codex` unless that name exists in config.

### 1. Submit (foreground, `--no-wait`)

`& $cli` (or the Unix binary) **must wait for enqueue to return**. That is one HTTP POST, not the full render. Capture stdout JSON. Save every `job_id` with its `--out`.

```powershell
$cli = Join-Path $PWD 'scripts\gpt-image-2-skill.exe'
$tickets = @()
foreach ($page in $pages) {
  $outFile = Join-Path $output ($page[0] + '.png')
  $raw = & $cli --json --no-wait --provider ailili images edit `
    --prompt ($page[1] + ' ' + $base) --ref-image $ref --out $outFile `
    --format png --size 1536x2048 --quality medium --input-fidelity high
  if ($LASTEXITCODE -ne 0) { throw "enqueue failed $($page[0])" }
  $r = $raw | ConvertFrom-Json
  $tickets += [pscustomobject]@{ job_id = $r.job_id; out = $outFile; name = $page[0] }
}
@($tickets) | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 $ticketFile
```

A successful enqueue looks like `{ "ok": true, "queued": true, "job_id": "...", "out": "...", "ticket": { "job_id", "out" } }`. Stop this tool call after writing the ticket file. Do not wait for pixels here.

### 2. Poll (later turn)

```powershell
& $cli --json jobs status --file $ticketFile
```

Read `pending`, `completed`, `failed`, `done`, `all_ok`, and `tasks[]`. If `pending > 0`, end the turn and poll again after 30–60 seconds. Do not busy-loop `Start-Sleep` for 10 minutes inside one command. When a task is `completed`, the CLI copies the image to the recorded `out`. Re-enqueue **only** `failed` names.

`jobs status --id <job_id>` works for a single id.

### Batch chat delivery: display finished pages immediately

When a batch creates user-facing images, track `name` values already displayed. On every `jobs status` poll, find tasks whose status is `completed` and whose names have not yet been displayed. Verify that each recorded `out` file exists, then immediately render every newly finished page in the chat using a Markdown image with its absolute path:

```markdown
![01 main premium](C:/absolute/path/to/01_main_premium.png)
```

Do not wait for the whole batch before displaying successful images. Continue polling pending tasks every 30-60 seconds and display newly completed pages after each poll. Never display a failed or missing output as a result. At final completion, report the total succeeded and failed counts and link the output folder/files; do not re-render pages already shown.

Daemon runs up to 10 jobs at once. Enqueue is sequential and cheap; renders overlap in the daemon.

### One image

`images generate` / `images edit` without `--no-wait` may block until `--out` exists. That is fine for a single page.

### Local wait-all (humans, not Codex)

`images fanout --jobs pages.json` starts child processes and blocks until every page finishes. Agents must not use it.

| | Windows | macOS / Linux |
|---|---|---|
| CLI | `.\scripts\gpt-image-2-skill.exe` | `./scripts/gpt-image-2-skill` |
| Submit | `& $cli --json --no-wait images edit ...` | `$CLI --json --no-wait images edit ...` |
| Poll | `& $cli --json jobs status --file tickets.json` | `$CLI --json jobs status --file tickets.json` |
| Daemon | Breaks away from the Job Object so it survives the client | `setsid` + ignore SIGHUP |

```bash
# 1. Confirm runtime + provider readiness
node scripts/gpt_image_2_skill.cjs --json config inspect
node scripts/gpt_image_2_skill.cjs --json doctor
node scripts/gpt_image_2_skill.cjs --json auth inspect

# 2. Generate a final transparent PNG deliverable
node scripts/gpt_image_2_skill.cjs --json --json-events \
  transparent generate --prompt "..." --out /tmp/asset.png \
  --size 2K --quality high

# 3. Generate a normal image (auto-selects provider; OpenAI first, then Codex)
node scripts/gpt_image_2_skill.cjs --json --json-events \
  images generate --prompt "..." --out /tmp/out.png \
  --format png --size 2K

# 4. Edit a reference image (OpenAI multipart)
node scripts/gpt_image_2_skill.cjs --json --json-events \
  images edit --prompt "..." --ref-image /tmp/in.png --out /tmp/out.png

# 5. Remove a controlled background from existing source images
node scripts/gpt_image_2_skill.cjs --json \
  transparent extract --input /tmp/source-green.png --out /tmp/asset.png \
  --method chroma --matte-color auto --strict

# 6. Verify the final file before delivery
node scripts/gpt_image_2_skill.cjs --json \
  transparent verify --input /tmp/asset.png --profile icon --strict

# 7. Raw request escape hatch
node scripts/gpt_image_2_skill.cjs --json \
  request create --request-operation generate \
  --body-file /tmp/body.json --out-image /tmp/out.png --expect-image

# 8. Self-test (calls doctor + auth inspect)
node scripts/selftest.cjs
```

Leave `--provider auto` unless `config inspect` lists a name to force. Do not pass `--provider openai` or `--provider codex` unless that exact name exists in config.

## Runtime freshness check

Before using newly documented command groups, especially `transparent generate`, `transparent extract`, or `transparent verify`, confirm the actual CLI runtime is fresh enough. Skill files, the Tauri App bundle, npm global CLI, cargo-installed CLI, and wrapper cache can drift independently.

Start by checking the latest upstream CLI version:

```bash
npm view gpt-image-2-skill version
```

Then look for the Tauri App bundled CLI first. Treat the app bundle as user-managed software: if it exists but is older than upstream, ask the user before updating or replacing the app.

```bash
APP_CLI="/Applications/GPT Image 2.app/Contents/Resources/bin/gpt-image-2-skill"
[ -x "$APP_CLI" ] && "$APP_CLI" --version
```

If no app bundle CLI exists, check the shell CLI. If it is missing or older than upstream, silently update the CLI dependency with npm, then verify that the current shell resolves the new binary:

```bash
command -v gpt-image-2-skill || true
gpt-image-2-skill --version || true
npm install -g gpt-image-2-skill@latest
command -v gpt-image-2-skill
gpt-image-2-skill --version
```

After any update, run the skill wrapper smoke checks:

```bash
node scripts/gpt_image_2_skill.cjs --json doctor
node scripts/selftest.cjs
```

If a documented subcommand fails with `unrecognized subcommand`, diagnose stale runtime first rather than changing prompts or providers. Prefer the wrapper in this skill directory for reproducible skill execution, but keep the bare CLI fresh when examples or user commands call `gpt-image-2-skill` directly.

## Shared config

Use the CLI config surface when the user asks to add or pin a provider:

```bash
node scripts/gpt_image_2_skill.cjs --json config path
node scripts/gpt_image_2_skill.cjs --json config add-provider \
  --name my-image-api \
  --type openai-compatible \
  --api-base https://example.com/v1 \
  --api-key sk-... \
  --set-default
node scripts/gpt_image_2_skill.cjs --json config test-provider my-image-api
```

Credential sources supported by CLI, App, and Skill: `file`, `env`, and `keychain`. File credentials are stored in the shared config file; JSON output redacts them.

## Flags vs prompt — what each controls

Output **properties** (not "what to draw") are flag-controlled. Putting them in the prompt is unreliable and provider-dependent.

| Property | Use this flag, not the prompt |
|---|---|
| Output background (transparent / opaque / auto) | `--background auto\|transparent\|opaque` |
| Output dimensions | `--size 2K`, `--size 4K`, or `--size WIDTHxHEIGHT` |
| Output container | `--format png\|jpeg\|webp` |
| Compression level | `--compression 0..100` |
| Render quality | `--quality low\|medium\|high\|auto` |
| Number of images | `--n <count>` (OpenAI only) |
| Edit mask region | `--mask <png>` (OpenAI only) |

The prompt is for "what is in the picture"; background, size, format, count, and mask are not. For example, to turn a transparent PNG into a white-background PNG, pass `--background opaque` — describing "white background" only in the prompt is **not reliable**.

**Provider asymmetry**: `--background`, `--n`, `--moderation`, `--mask`, and `--input-fidelity` are honored only by OpenAI (and OpenAI-compatible bases that proxy them). Codex `image_generation` does not honor `--background`; the runtime accepts the flag but the upstream tool drops it. The other four return `code: "unsupported_option"` if passed with `--provider codex`.

## Transparent PNG deliverables

For transparent output, do not rely on provider-native transparency. Use the `transparent` command group as the Agent-facing tool layer:

- `transparent generate` — prompt-to-final PNG. It generates a controlled matte source, extracts alpha locally, verifies the result, and only succeeds when the final PNG passes transparency checks.
- `transparent extract` — local background removal from controlled source images you generated yourself. It is not a general-purpose background remover for arbitrary photos.
- `transparent verify` — final gate for any PNG before delivery. Use `--strict` and the right `--profile` when the file must be accepted or fail the task.

A transparent deliverable is valid only if the final file has a real PNG alpha channel and passes verification. A visual appearance of transparency, a white background, or a checkerboard pattern is not sufficient.

`--strict` is profile-based:

| Profile | Use for | Extra strictness |
|---|---|---|
| `generic` | common alpha/file checks | does not over-police unusual assets |
| `icon` | clean single-subject icons and props | requires clean opaque core, margin, low stray noise |
| `product` | product/object cutouts | similar to icon, with residue and edge checks |
| `sticker` | decals, badges, multi-detail props | allows more intentional small components than `icon` |
| `seal` | stamps, seals, logos with inner marks | allows split components such as ring + center symbol |
| `translucent` | glass, liquid, crystal | requires partial alpha |
| `glow` | light ribbons, flame, smoke, particles | requires partial alpha and transparent margin |
| `shadow` | soft shadow assets | requires partial alpha and transparent margin |
| `effect` | hard-alpha particles, bursts, UI effects | transparent margin without requiring partial alpha |

The CLI is intentionally not a material classifier. The Agent should choose generation prompts and extraction methods based on the asset:

| Asset type | Generation guidance | Extraction guidance |
|---|---|---|
| Opaque object, icon, sticker, product | Single isolated subject, clear margin, perfectly flat chroma matte. Pick a matte color absent from the object. | `transparent generate` or `transparent extract --method chroma --matte-color auto` |
| Thin edges, hair, fur, lace, chain, netting | Use high resolution, strong subject/background contrast, no contact shadow, no background-colored details. Try magenta/cyan/green mattes if one contaminates the edge. | Chroma extraction with `--spill-suppression` when needed, then verify with `--expected-matte-color`; retry with a different matte if residue remains. |
| Glass, crystal, liquid, hologram | Ask for a centered asset on flat black and flat white backgrounds, keeping geometry identical. Use reference/edit flow when possible to keep alignment. | `transparent extract --method dual --dark-image black.png --light-image white.png` |
| Glow, flame, smoke, mist, magic particles | Generate dark and light background variants. Avoid textured backgrounds and avoid bloom reaching the image edge unless the edge is intentional. | Prefer dual extraction; verify that `partial_pixels` is non-zero. |
| Shadows | Decide whether the shadow is part of the asset. If not, explicitly forbid contact shadows. If yes, generate on a flat matte with enough margin. | Chroma for opaque shadow silhouettes; dual extraction for soft translucent shadows. |
| Unknown or unusual material | Do not classify it first. Generate controlled source variants, run extraction candidates, and keep the one that passes verification with the cleanest edge. | Use `--report-dir` / `--keep-sources` while iterating, then deliver only the final PNG. |

For chroma extraction, `--matte-color auto` samples the actual flat source background from the image edges. Prefer it when the source was AI-generated, because prompts like "pure #ff00ff" often produce near-matte colors rather than exact RGB values. Use explicit `--matte-color <name|#rrggbb>` only when the source background is known exactly.

For extraction tuning, use `--material` only as a broad hint, not as a subject classifier: `standard`, `soft-3d`, `flat-icon`, `sticker`, or `glow`. Manual `--threshold`, `--softness`, and `--spill-suppression` override the selected preset.

For style-locked transparent assets, `transparent generate` is prompt-only. Use a flat RGB reference image with `images edit --ref-image` to create a controlled matte source, then run `transparent extract`. Do not use a transparent PNG as the reference image unless you intentionally want the alpha/composited edge behavior to influence the edit.

GPT Image 2 can render accurate UI text, numbers, scores, labels, and logo marks in the bitmap when they are part of the desired artwork. Put the exact wording or numbering in the prompt and verify the output visually. Render text separately in the host app or design tool only when it must stay editable, localizable, programmatically changeable, or perfectly consistent across many generated variants.

Examples:

```bash
# Simple asset: final transparent PNG, sources hidden unless there is a failure
node scripts/gpt_image_2_skill.cjs --json --json-events \
  transparent generate \
  --prompt "a polished fantasy sword game asset, no text, no frame" \
  --out /tmp/sword.png --size 2K --quality high

# Agent-controlled chroma flow
node scripts/gpt_image_2_skill.cjs --json --json-events \
  images generate \
  --prompt "a silver necklace, centered, on a perfectly flat pure magenta background, no shadow" \
  --out /tmp/necklace-magenta.png --format png --size 2K
node scripts/gpt_image_2_skill.cjs --json \
  transparent extract --method chroma \
  --input /tmp/necklace-magenta.png --matte-color auto \
  --out /tmp/necklace.png --material sticker --strict

# Semi-transparent material flow
node scripts/gpt_image_2_skill.cjs --json \
  transparent extract --method dual \
  --dark-image /tmp/glow-on-black.png \
  --light-image /tmp/glow-on-white.png \
  --out /tmp/glow.png --strict
```

Always inspect the JSON verification fields before delivery: `passed`, `alpha_min`, `alpha_max`, `transparent_ratio`, `partial_pixels`, and `warnings`. Also inspect quality fields: `checkerboard_detected`, `touches_edge`, `edge_margin_px`, `stray_pixel_count`, `largest_component_ratio`, `matte_residue_checked`, `matte_residue_score`, `halo_score`, `transparent_rgb_scrubbed`, `alpha_health_score`, `residue_score`, `quality_score`, and `failure_reasons`. If `passed` is false, do not deliver the file as a transparent PNG. If `matte_residue_checked` is false for a chroma-derived PNG, run `transparent verify` again with the source matte via `--expected-matte-color`.

## Notes

- `openai` defaults to `gpt-image-2`; `codex` defaults to `gpt-5.4` and delegates to `image_generation`.
- Shared options actually honored everywhere: `--size`, `--quality`, `--format`, `--compression`.
- OpenAI-only options: `--background`, `--n`, `--moderation`, `--mask`, `--input-fidelity`.
- Retries: up to 3 with exponential backoff (1s → 2s → 4s). Codex `401` triggers one token refresh + one retry.
- Size aliases: `2K` → `2048x2048`, `4K` → `3840x2160`. Custom `WxH` requires both edges multiples of 16, max edge 3840, max 8,294,400 pixels, max aspect ratio 3:1.

## Reference files

Load on demand for deeper detail:

- `references/providers.md` — OpenAI / OpenAI-compatible / Codex selection, auth sources, runtime discovery, update policy, and resolution order.
- `references/sizes-and-formats.md` — size aliases, custom constraints, format/quality/compression/background, shared vs OpenAI-only flags.
- `references/transparent-png.md` — Agent playbook for prompt design, controlled mattes, dual-background extraction, verification, and retry loops.
- `references/json-output.md` — `--json` stdout schema, success and error envelopes, per-command shapes.
- `references/json-events.md` — `--json-events` JSONL phases (`request_started`, `multipart_prepared`, `retry_scheduled`) and Codex SSE passthrough.
- `references/troubleshooting.md` — `runtime_unavailable`, `auth_missing`, Codex `401` refresh, retry policy, size rejections, moderation, timeouts.

## Codex compatibility

The companion file `agents/openai.yaml` is read by Codex Skill runtime only (Claude Code ignores it). Both runtimes execute the commands above with `cwd` at the skill directory, so `scripts/gpt-image-2-skill.exe` (Windows), `scripts/gpt-image-2-skill` (macOS/Linux), and `scripts/gpt_image_2_skill.cjs` resolve without an absolute `.codex` path.
