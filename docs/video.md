# Video: H.264/EGFX, display resolution, and aspect handling

How macrdp draws your screen: the H.264 pipeline (`--enable-h264`), Retina
capture (`--hidpi`), client-resolution auto-adopt and letterboxing, the mstsc
reconnect quirk and its fixes, and the color-conversion implementation notes.

## Display resolution (`--hidpi`)

There are **two independent topologies**, implemented with a common aspect-fit
capture pipeline:

1. **Mirroring the current physical display** (`--virtual-display` absent):
   `--hidpi` **off** continues to honor the client-requested RDP desktop. With
   `--hidpi` **on**, the physical panel's backing (Retina) pixel resolution
   becomes the native-detail target and automatic session ceiling. Capture fits
   the original Mac picture inside the agreed RDP canvas: black bars appear if
   the client's aspect ratio leaves free space, otherwise the native picture
   is scaled down as needed. No aspect stretching unless `--stretch` is chosen.
   Both settings use exactly the same ScreenCaptureKit scaling, output-pixel
   damage tracking, batching and mouse-coordinate mapping.
2. **Capturing a virtual display** (`--virtual-display`): independent of
   `--hidpi`. The virtual display is re-moded to the agreed client desktop
   size, with a 1:1 capture and no physical-panel Retina calculations. Current
   `CGVirtualDisplay` does not provide a reliably working logical-points vs
   backing-pixels Retina mode (see [known quirks](known-quirks.md)).

**An RDP server may not send a larger bitmap than the client negotiated.** A
physical Mac framebuffer of `3360×2100` and Android request of `1080×1196`
therefore produce a `1080×1196` RDP desktop, not a forced `3360×2100` one. The
picture fits at `1080×675` with black bars at top/bottom. Only clients that
**request enough resolution** can receive a native-resolution pixel stream;
`--hidpi` cannot silently turn a small Android RDP canvas into a 2× canvas.
The Retina backing size is a SOURCE dimension, not an RDP canvas ceiling.
A portrait client may request a taller desktop just to hold black letterbox
bars: for example, a `1968×2184` Android canvas and a `3360×2100` Mac Retina
source yield `1968×1230` content with 477px bars above and below. Only an
explicit `--max-client-size` constrains Demand Active, Confirm Active, and
live RDPEDISP resizes. Full-screen high-resolution legacy BitmapUpdates
remain substantially more expensive than reduced-size captures.

Explicit `--width`/`--height` on the mirror path pins the resolution (stretch
behavior as before). `--no-client-resolution` also fixes the initial server
resolution but can exceed the client surface and **corrupt** legacy bitmap
rendering. Never use it to force physical Retina output on a small Android
client. The controller's `HIDPI=1` maps to the same policy. For detailed
examples and verification steps see
[MIRROR_HIDPI_POLICY.md](MIRROR_HIDPI_POLICY.md).

### Aspect ratio (physical mirror with client auto-size)

The existing physical mirror captures into the *negotiated* RDP canvas while
preserving the original Mac aspect ratio. Portrait clients get top/bottom bars;
wide clients get left/right bars. The inverse mouse transform uses the same
aspect-fit rectangle, so touches on black bars clamp to the nearby Mac edge.
`--stretch` preserves the old fill-and-distort behavior instead. Virtual
sessions don't letterbox; they change their actual virtual-monitor mode.

## Video (H.264)

By default the display is sent as legacy bitmaps (RemoteFx/QOI to mstsc, NSCodec/raw to others) — works everywhere, but bandwidth-heavy. Pass **`--enable-h264`** to stream the desktop as **H.264 over the EGFX virtual channel** (MS-RDPEGFX, AVC420), hardware-encoded with VideoToolbox. Far less bandwidth, especially for video/scrolling/photos.

How it behaves:

- **Automatic fallback.** Clients that don't advertise H.264 (AVC420) decode — e.g. a FreeRDP build without an H.264 decoder — transparently fall back to legacy bitmaps. No need to match the flag to the client. mstsc, FreeRDP-with-H.264, and the macOS **Windows App** / Microsoft Remote Desktop client all decode the H.264 stream.
- **Wire format.** The AVC420 payload is Annex-B framed (what Microsoft's decoder expects). The bitstream is verified rendering on `mstsc` and on FreeRDP built with H.264 (e.g. the [Thincast client]).
- **Bitrate.** `--bitrate N` sets the target encoder bitrate in megabits/sec (default `6`, only meaningful with `--enable-h264`). Raising it sharpens detail but grows each frame, so the big per-frame writes are more likely to fill the socket buffer and delay audio on a constrained link — `6` is a good balance; try `8`–`12` if you have headroom.
- **Color.** The stream is encoded as full-range BT.709. This matters for `mstsc`, which reads AVC420 luma as full-range regardless of the bitstream flag — video-range output otherwise renders washed-out / lighter there. FreeRDP honors the flag and is correct either way. To get full range we convert each captured BGRA frame to full-range NV12 ourselves (VideoToolbox would otherwise emit video-range from a BGRA source); that conversion is **vImage**-accelerated — see [Color conversion: scalar vs vImage](#color-conversion-scalar-vs-vimage).
- **Frame rate.** `--enable-h264` defaults to **60fps** (vs 15 for legacy). mstsc holds a fixed ~2-frame presentation buffer for the H.264 stream, so at 30fps typing lags ~2 keystrokes (~66ms) while at 60fps that buffer is ~33ms and feels immediate. FreeRDP-based clients don't buffer this way and are snappy at any rate. Set `--fps` explicitly to override (lower it to save CPU/bandwidth if your client/link doesn't need 60).
- **Keyframes.** A keyframe (IDR) is forced on the first frame, then periodically every `--keyframe-interval` seconds (default `2`) as a safety net — some clients (mstsc) only fully recover a transient decode glitch on the next IDR, so a long interval leaves garbled regions (notably text) lingering. Lower it for faster recovery at the cost of bandwidth/quality; raise it for smoother typing. Optionally, pass **`--keyframe-on-change`** (off by default) to additionally force an IDR whenever a large area changes at once (window-to-front, scroll, app launch) and briefly after a mouse click, so big updates land immediately instead of waiting for the periodic interval (rising-edge detection keeps sustained churn like video from forcing an IDR every frame). It's off by default because the periodic interval plus the trailing flush-burst (`--flush-frames`) already drain mstsc's presentation buffer, so the extra forced IDRs mostly just spend bitrate/quality at a fixed bitrate for no typing benefit — enable it only if large updates visibly lag on your client/link. When enabled, the trigger thresholds are tunable: `--keyframe-change-pct` (default 20, the dirty-area % that fires an IDR), `--keyframe-click-pct` (default 5, the lowered threshold after a click), and `--keyframe-click-window-ms` (default 400, how long that lowered threshold lasts).
- **Flush frames (`--flush-frames`, default `4`).** ScreenCaptureKit only delivers a frame when the screen changes, so after the last keystroke before a pause there are no further frames to push it through mstsc's ~2-frame AVC420 presentation buffer — it would strand there until the next change or periodic keyframe (the classic "typing follows the keyframe" lag). After each change the server re-submits the last frame this many times as cheap skip-P-frames, draining the buffer so the change appears within a couple of frame intervals (~33 ms at 60fps), then goes quiet. mstsc needs ≥2; raise if a slight trailing lag remains, or set `0` to disable.

### Known limitations

- **Reconnecting `mstsc` to a still-running macrdp can show a black screen** (with a live cursor). This is an mstsc-specific quirk: it retains EGFX surfaces for the lifetime of its process and mis-composites on reconnect. It is *not* a server bug — FreeRDP reconnects cleanly over the same stream. **The server now self-heals it in place** (default, with `--enable-h264`): on a detected blank it sends a bare core RDP Deactivation–Reactivation that preserves the EGFX channel/surface, and mstsc re-maps its retained surface and presents again in ~1–2 s with no disconnect (see the blank-recovery notes in [known-quirks.md](known-quirks.md)). If the automatic recovery ever fails to heal, **close + reopen the mstsc window** — quitting the client clears its surface cache, so the desktop renders every time (no Windows reboot needed). (A server-side fresh-surface-id workaround, and an earlier fork-a-fresh-worker-process-per-connection model, were both tried before the in-place reactivation superseded them.)
- H.264 is **macOS-only** (VideoToolbox) and still maturing — bitrate and keyframe behavior are tunable (above), but dirty-region *encoding* is not yet done: every frame is a full encode (dirty rects are used only to time on-demand keyframes, not to encode sub-regions). H.264's own inter-prediction keeps unchanged regions cheap regardless.

### Color conversion: scalar vs vImage

*(Implementation detail — skip unless you're profiling CPU or porting the encoder.)*

VideoToolbox, given a BGRA source, emits **video-range** YUV (luma 16–235). `mstsc` reads AVC420 luma as **full-range**, so that looks washed out (see **Color** above). The fix is to hand VideoToolbox a YUV buffer that's already full-range, which means doing the BGRA → full-range BT.709 NV12 (`420f`) color conversion ourselves, once per captured frame, on the capture thread.

That conversion is a real per-frame cost, so it's done with **vImage** (Apple's Accelerate framework), which runs the RGB→Y'CbCr math on the CPU's vector units (NEON on Apple Silicon). A scalar reference implementation (a plain Rust loop) is kept as well: it's the fallback for any frame vImage declines (e.g. odd dimensions), the oracle the vImage path is unit-tested against, and the baseline below. Both produce identical output (within ±1 rounding).

Single-thread cost per frame, Apple M3 (`cargo test --release bench_nv12_full_range -- --ignored --nocapture`):

| Resolution | scalar | vImage | speedup |
|---|---:|---:|---:|
| 1470×956 | 3.36 ms | 0.12 ms | ~29× |
| 1920×1080 | 4.98 ms | 0.16 ms | ~32× |
| 2560×1440 | 8.88 ms | 0.33 ms | ~27× |
| 3840×2160 (4K) | 20.0 ms | 0.84 ms | ~24× |

At 60fps the frame budget is 16.67 ms. The scalar path is fine at 1080p (~30% of one core) but **exceeds the budget at 4K**, where it would cap the achievable frame rate before the encoder even runs; vImage keeps the conversion at ~1% of budget across the board, so it's never the bottleneck. The implementation lives in `src/videotoolbox.rs` (`bgra_to_nv12_full_range_vimage`, with `bgra_to_nv12_full_range` as the scalar reference).

[Thincast client]: https://thincast.com/en/products/client
