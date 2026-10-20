# Physical mirror HiDPI policy (experimental)

This branch separates **physical-panel mirroring** from the **CGVirtualDisplay**
path. Both modes continue using the established capture, bitmap-delta and
input handling code. Neither mode is allowed to send bitmap updates outside
the **negotiated** RDP desktop size.

## Modes

| Topology | HiDPI flag | Output resolution | Content |
|---|---|---|---|
| Physical mirror | off | Client-requested RDP size (unless pinned) | Existing ScreenCaptureKit scaling, aspect-fit black bars by default |
| Physical mirror | on | Client-requested RDP size (unless explicitly capped with `--max-client-size`) | Prefer backing-pixel detail; use exactly the same aspect-fit scaling/letterboxing. If the requested canvas is too small for native pixels, downsample; if it is taller than the Retina source, pad with bars. |
| Virtual display | either | Client-requested RDP size (or pinned virtual-display size) | Re-mode CGVirtualDisplay; use a 1:1 native capture without letterboxing |

`--stretch` still opts out of black bars on auto-sized physical mirrors.
Explicit mirror `--width`/`--height` remains a pinned, stretch-to-fill session.
`--no-client-resolution` still selects a pinned session and **may break clients
whose reported desktop is smaller**. It is not a safe way to obtain native
Retina pixels on Android.

### Why `--hidpi` does **not** blindly double the Android output

An Android Windows App requesting `1080×1196` advertises an RDP desktop of
exactly that size. The previously forced `3360×2100` HiDPI BitmapUpdates
caused repeated Dock/menu strips, a frozen middle, and a live-session slowdown.
The server cannot infer that the Android client will accept a 2× or 3× larger
RDP bitmap just because a **local** HiDPI setting is enabled. To receive native
pixels at a higher *transmitted* resolution, **the client itself must request
an adequately large RDP desktop**. If it does not, the mirrored Retina image is
fit/downsampled into the negotiated client canvas. This limitation is explicit
and is not passed off as a higher-resolution stream.

For example (physical Retina framebuffer `3360×2100`):

* Client requests `1080×1196`: the RDP desktop is `1080×1196`. Aspect-fit
  content is `1080×675` with roughly 260.5 pixels of black bar above and below.
  All legacy tile deltas stay inside the advertised `1080×1196` bitmap.
* Client requests `3360×2100`: the RDP desktop can be `3360×2100` and the
  physical panel's native pixels fit at 1:1 without scaling.
* Client requests `1968×2184` with a `3360×2100` Retina source: the RDP
  desktop stays **1968×2184** (rather than being incorrectly clamped to
  `1968×2100`). Aspect-fit produces `1968×1230` content and 477px bars at
  the top and bottom. This avoids vertical pointer drift due to mismatched
  client/server canvas heights on portrait Windows App.
* A request wider or taller than the backing pixels is accepted when no
  explicit client-size limit is configured. The content scales to fit inside
  the announced RDP surface; the Mac's actual source pixels don't multiply.
  An explicit `--max-client-size` is still enforced on both **Demand Active**
  and the live RDPEDISP resize / Confirm Active paths.

The protocol limit is up to `8192×8192` and can be costly for legacy bitmaps.
For untrusted or resource-limited deployments, operators should configure
`MAX_CLIENT_SIZE` according to the clients they actually use. This is an
independent resource/safety policy, not a Retina aspect-ratio constraint.

## Implementation

- `src/display_geometry.rs`: `DisplayPolicy` and the common aspect-fit math.
- `src/main.rs`: selects the topology independently of the HiDPI switch;
  backing pixels determine source quality, and only an explicit operator cap
  constrains the client canvas. The acceptor receives this cap (if set) before Demand Active.
- `src/capture.rs`: applies the same explicit cap to Confirm Active echoes and
  live resize events; uses the same aspect-fit plan for scaled delta capture.
  The pre-existing 64×64 tile delta and batching are unchanged.
- `src/input.rs`: reuses the aspect-fit rectangle when mapping mouse input from
  client pixels into the Mac's point-space display, so padding and clicks agree.

## Regression verification on a real Mac

1. Keep the `30/45 FPS` stable legacy-delta setup. Before rebuilding, tag the
   previously working commit and make sure another remote-access path works.
2. Test physical mirror with `HIDPI=0`, `1080×1196` client: client-size display,
   correct black bars and input, no accumulated lag.
3. Test physical mirror with `HIDPI=1`, **same** `1080×1196` client: still serve
   `1080×1196`, no repeated stripes/frozen regions; compare pixels and latency.
4. With `HIDPI=1` and no `MAX_CLIENT_SIZE`, test Android portrait
   `1968×2184` and landscape `2184×1968` requests against `3360×2100`
   backing. Verify that the negotiated size matches the request on both axes,
   with no Y drift and no oversized bitmap updates. Repeat with an explicit cap.
5. If a client can **request** a sufficiently large RDP desktop, test native
   Retina output and a differently shaped request. Inspect where black bars
   appear and whether input coordinates remain aligned.
6. Test `VIRTUAL_DISPLAY=1` with HiDPI both on/off: output should remain driven
   by the virtual display's negotiated mode, never by the physical backing.
7. Test one live window-size change and reconnect for each relevant mode;
   confirm that no bitmap exceeds the advertised frame size.
8. Compare `--no-scaled-delta` and `--no-scaled-delta-batching` only for
   diagnostic A/B testing if artifacts remain.

Unit tests only check pure policy and coordinates; real SCK/WindowServer,
VideoToolbox, and Android RDP client behavior still require macOS testing.
