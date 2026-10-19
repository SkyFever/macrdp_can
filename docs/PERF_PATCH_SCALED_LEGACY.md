# Scaled legacy BitmapUpdate performance patch (experimental)

## Scope and motivation

Test baseline (reported by tester on a wired Mac, Wi-Fi 7 Android client, local LAN):

| Requested FPS | macrdp CPU (Activity Monitor) | Motion latency |
| --- | ---: | --- |
| 15 | ~30% | little/no accumulation |
| 24 | ~37% | little/no accumulation |
| 30 | ~40% | little/no accumulation |
| 45 | ~43% | slight accumulation under continuous motion |
| 60 | <70% after client acceleration enabled | large accumulation |

The Android Windows App reported **`AVC_DISABLED`** and the server explicitly chose
**legacy BitmapUpdate**, even though `ENABLE_H264=1`. Thus VideoToolbox and its
hardware encoder are **not on the measured Android connection's graphics path**.

The existing scaled legacy capture path sent an entire bitmap whenever the
requested resolution matched neither the Mac's native logical size nor backing
pixel size. This was necessary because ScreenCaptureKit's *source-coordinate*
dirty rectangles do not index into the *scaled output* bitmap. The downstream
IronRDP encoder then diffed/compressed each whole update, copying and scanning
far more pixels than a small Dock animation had actually changed.

## Changes in this patch

1. `src/frame_delta.rs` implements lossless **output-coordinate 64×64 tile
   tracking** using a per-connection BGRA framebuffer snapshot. Only tiles
   whose actual pixels have changed are considered dirty. Horizontal runs are
   merged vertically where possible, and existing `split_strips` continues to
   enforce the proven-good per-BitmapUpdate size bound. Pure-Rust tests cover
   padded strides, edge tiles, sparse changes, large changes, resizing, and
   exact destination-frame reconstruction.
2. Full-image fallback remains mandatory for initial frames, dimension
   changes, >50% dirty tiles, or >96 rectangles. A small dirty animation
   avoids whole-frame BitmapUpdate copies and compression, but a busy video
   won't incur hundreds of small update PDUs.
3. Existing native (unscaled) ScreenCaptureKit dirty-rect behavior and all
   negotiated EGFX/H.264 sessions remain unchanged. The scaled fallback never
   trusts source-coordinate dirty rectangles. The output delta baseline is
   reset per new connection/reactivation, and pending bitmap updates continue
   to drain in protocol order (no cancellation of partially sent deltas).
4. `--no-scaled-delta` reverts **only** this algorithm to the original
   scaled-full-frame behavior for A/B tests. The default is ON on scaled legacy
   sessions. A 5-second `legacy scaled-frame delta statistics` log gives counts
   of frames, full frames, unchanged frames and output rectangles.
5. For clients that **actually negotiate AVC420**, the VideoToolbox session
   requests a hardware encoder, queries the `UsingHardwareAcceleratedVideoEncoder`
   property and logs `hardware_accelerated=Some(true|false)` (or `None` if not
   exposed). `MACRDP_REQUIRE_HW_ENCODER=1` optionally refuses software H.264
   fallback. The latter is an **environment variable on the macrdp process**,
   not an `EXTRA_FLAGS` CLI option. It has no effect on `AVC_DISABLED` clients.

**Not included:** a Metal framebuffer processing backend, NV12 zero-copy from
ScreenCaptureKit to VideoToolbox, HEVC over RDP, lossless/lossy UDP changes,
legacy frame-ACK pacing changes. Those require macOS-native builds/profiling
and/or supported client codec negotiation. Do not advertise them as implemented.

## Build and run on the Mac

Do this from the existing fork checkout after applying the patch. Keep an
alternative remote login method available before modifying the running server.

```sh
cargo test --release frame_delta
cargo test --release
cargo build --release
./target/release/macrdp --help | grep scaled-delta
```

For the `.app` deployment used by the existing `com.clintcan.macrdp` LaunchAgent,
`packaging/make-app.sh` builds, signs and installs the bundle. Use the same
code-signing identity as the installed app when possible so macOS TCC permissions
are not invalidated. Be aware that it replaces the existing app and can
interrupt the running remote connection. Never overwrite the signed bundle's
Mach-O directly without re-signing.

To A/B at **45 FPS** (leave resolution, codec, client and network unchanged):

```ini
# New scaled-delta path (default)
EXTRA_FLAGS="--fps 45"

# Old full-frame scaled path (A/B comparison; change the line above)
EXTRA_FLAGS="--fps 45 --no-scaled-delta"
```

After editing `~/Library/Application Support/macrdp/config.env` restart the
service (disconnects existing sessions):

```sh
launchctl kickstart -k "gui/$(id -u)/com.clintcan.macrdp"
tail -n 100 "$HOME/Library/Logs/macrdp.log" | grep -E 'scaled-frame delta|capture: first frame'
```

Continuous Dock animation is a useful **workload**, not a latency clock.
Measure both motion-to-display delay and whether latency continues increasing
when holding motion for 30–60 seconds. Compare CPU and actual transferred bytes;
`fps` supplied by the CLI is only a *requested maximum*. A server log does not
by itself measure the client's display presentation time.

### H.264 hardware diagnostic (only for an AVC-capable client)

For an existing LaunchAgent you can set the **launchd environment** before
restarting the server (only when you have an alternative access path):

```sh
launchctl setenv MACRDP_REQUIRE_HW_ENCODER 1
launchctl kickstart -k "gui/$(id -u)/com.clintcan.macrdp"
# Connect with a client that actually negotiates AVC420; check logs.
# Clear the diagnostic for subsequent sessions:
launchctl unsetenv MACRDP_REQUIRE_HW_ENCODER
```

`launchctl unsetenv` affects future launches; it does not modify a running
process. The strict requirement intentionally fails if hardware encoding cannot
be allocated. The ordinary configuration **prefers** hardware but does not fail
if only software H.264 is available.

Find the negotiated codec and hardware status:

```sh
grep -Ei 'client advertised capabilities|AVC_DISABLED|H.264 active|VideoToolbox H.264 encoder selected' \
  "$HOME/Library/Logs/macrdp.log" | tail -n 30
```

## Validation status

- This patch was authored in a Linux container without `rustc`/`cargo` and
  without macOS frameworks. No native build, Rust unit tests, macOS runtime,
  client interop or FPS improvement was measured there.
- `cargo test` and the 45 FPS A/B are **required on a Mac** before shipping.
- Rollback: `git revert <patch-commit>` in source, then rebuild/re-sign;
  or set `--no-scaled-delta` for an immediate A/B of the legacy bitmap change.
- The algorithm intentionally compares the output pixels every frame; CPU
  comparison overhead may be noticeable on very large displays. Exact dirty
  tiles should save compression/transport work on mostly-static desktops, not
  necessarily on fullscreen video. Performance gains are hypotheses until
  measured on the Mac.
