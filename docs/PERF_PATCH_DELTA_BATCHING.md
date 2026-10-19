# macrdp — scaled legacy frame batching / tear mitigation (experimental)

**Baseline:** commit `3c0c4ef` (scaled output-pixel tile damage patch).
**Scope:** only scaled legacy BitmapUpdate connections (for example Android Windows App negotiating `AVC_DISABLED`). H.264/EGFX and unscaled native-damage connections are unchanged.

## Why

The 64×64 lossless output tile tracker fixed accumulating latency at 45 FPS, but a rapidly changing Dock sometimes appears horizontally torn / temporally mismatched in an Android screenshot. The tester's trace shows approximately 3–4 capture-side damage rectangles per frame. `ironrdp-server` can diff and subdivide each input rectangle a second time, so the client may paint multiple separately transmitted regions from one captured frame at different moments.

This is **consistent with** the screenshot but is not proven to be the sole cause. Basic legacy BitmapUpdate is not guaranteed to present a whole screen atomically. Nothing in this patch should be interpreted as a promise of tear-free display or an RDP frame marker implementation.

## Changes

1. Group nearby already-changed 64×64 tile rectangles into larger bounding rectangles, with conservative limits: `merged_area <= 2 × original_dirty_area` and `merged_area <= 1280 × 720` pixels. Distant areas remain independent. The matcher examines at most 32 incoming rectangles per frame to bound CPU work; larger sets use the original tile path.
2. Mark these output-pixel-verified rectangles as `trusted_damage`, so the vendored IronRDP encoder does not immediately re-diff a merged region into many smaller updates. The encoder's normal max-request-size split remains in place; the new flag does **not** bypass protocol size limits.
3. Preserve original output tile tracking and baseline semantics. All rectangles use the *same locked SCK pixel buffer*; unmodified areas included in a bounding box contain that same frame's pixels. Only affected scaled legacy sessions opt into the change; other kinds of updates retain their previous behavior.
4. Add `original_rects`, `rects`, and `batched_frames` to the 5-second `legacy scaled-frame delta statistics` event.
5. Add `--no-scaled-delta-batching` to turn off **only this follow-up fix**, returning to the already-working tile update + downstream diff implementation. The older `--no-scaled-delta` still disables the original tile tracker altogether.

## Mac test / rollback

Build (requires Rust and macOS SDK):

```bash
cargo test --release frame_delta
cargo test --release
cargo build --release
```

Use the existing `packaging/make-app.sh` / signing workflow to install the rebuilt `.app`. Avoid overwriting a signed executable without re-signing; keep another remote-access path available.

Set the installed config file `~/Library/Application Support/macrdp/config.env` to:

```ini
# new batching enabled (default)
EXTRA_FLAGS="--fps 45"
```

For immediate A/B comparison without rebuilding:

```ini
# previous optimized implementation (no new batching / no trusted-damage hint)
EXTRA_FLAGS="--fps 45 --no-scaled-delta-batching"
```

Restart the user's LaunchAgent (this disconnects active RDP clients):

```bash
launchctl kickstart -k "gui/$(id -u)/com.clintcan.macrdp"
grep 'legacy scaled-frame delta statistics' "$HOME/Library/Logs/macrdp.log" | tail -n 8
```

On the same Android app with the same desktop resolution and 45 FPS, move the cursor rapidly across Dock for 30–60 seconds. Compare (a) visible horizontal seams, (b) latency accumulation and drain time, (c) macrdp CPU, (d) `original_rects / rects / batched_frames`, and (e) bitmap encoding warnings. If seams persist with a low final rectangle count, inspect the actual client paint/decoder schedule and legacy frame boundaries rather than increasing the coalescing ratio blindly.

## Validation limits

- `git diff --check` and portable Python reference/invariant tests were executed in the patch-authoring Linux container.
- Rust compiler and macOS frameworks are absent there: actual Rust type checking, tests, native build, RDP interoperability, screenshot correctness and performance improvements **have not been verified**. Run the commands above on the real Mac.
- The source code change touches the public vendored `BitmapUpdate` struct by adding a `trusted_damage` field; all in-repository constructors were updated. External code compiling directly against this fork's public struct will need to initialize the field as well.
