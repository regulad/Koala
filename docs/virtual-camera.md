# The virtual camera

The iPhone's camera reaches Windows apps as a camera of Windows' own: a Media
Foundation virtual camera (Windows 11, build 22000 on), "iPhone Camera
(Koala)" (Parker, 2026-11-21). DirectShow apps see it too, as "iPhone Camera
(Koala) (Windows Virtual Camera)": Windows adds that.

```text
iPhone ──HEVC over RTP, LLW──▶ Koala (decode.rs: HEVC to NV12)
                                 │ writes each picture
                                 ▼
             shared memory, Global\Koala.Camera.1.<SID>   (koala-vcam: frames.rs)
                                 │ reads the latest
                                 ▼
 Windows' camera service (Frame Server, LocalService, session 0)
   └─ koala_vcam_source.dll: the media source             (koala-vcam-source)
                                 │
                                 ▼
                     apps: Camera, browsers, OBS, …
```

## The pieces

- **Decoding** (`crates/koala/src/decode.rs`): Windows' HEVC decoder (Media
  Foundation; Microsoft's HEVC Video Extensions), on the GPU where it takes a
  Direct3D 11 device; else FFmpeg's `hevc`. "Force software decoding" in the
  tray's Options, or `koala guest --software-decode`, takes FFmpeg's
  (`crates/koala/src/settings.rs`). The two give the same pictures, bit for
  bit, as H.265 requires; the tests hold them to it.
- **The feed** (`crates/koala/src/vcam.rs`): each session's camera stream,
  decoded on a thread of its own from its one keyframe, whether or not an
  app is watching, and written to the shared memory once an app has started
  the camera.
- **The camera** (`crates/koala-vcam`, `VirtualCamera`): made with
  `MFCreateVirtualCamera` for the current user, for as long as Koala runs
  (a session camera). Koala gives it the user's SID as an attribute.
- **The shared memory** (`crates/koala-vcam/src/frames.rs`): made by the
  source, since only a process in session 0 can make an object in the
  global namespace, and allowed to the system, LocalService and that SID.
- **The media source** (`crates/koala-vcam-source`): a COM DLL the camera
  service loads. One stream, NV12 1920×1080 at 30 a second, BT.709 in
  limited range. It answers the service's requests with the latest picture,
  and with black once Koala has sent none for 2 s, in samples of its own in
  memory: it touches no GPU. It logs what it does to `koala-vcam.log` in the
  service's temporary directory
  (`C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp`, which takes
  Administrator rights to read).

## Installing it

The source is a COM class under `HKEY_LOCAL_MACHINE`, since the service reads
no user's classes, and its DLL must be where LocalService can read it, not in
a user's profile. So it takes Administrator rights, once. For development and
trials, `tools/vcam.ps1`, elevated, copies the DLL to `%ProgramData%\Koala`
and registers it; an installer will do the same.

The DLL loads only what Windows has: the Visual C++ runtime is linked in
(`crates/koala-vcam-source/build.rs`), and it imports Windows' DLLs alone.

## Tried (2026-11-21)

- **The decoders:** 1,687 pictures of trial 29's recording, the same from
  both. Windows' on the GPU (an AMD Radeon 8060S) decoded 249 a second,
  FFmpeg's 219: about eight times the camera's rate either way.
- **The source in-process** (its test): activated with the camera's
  attributes, it starts, sends black, then Koala's picture byte for byte.
  The presentation descriptor's stream has to be selected when it's made:
  `MFCreatePresentationDescriptor` leaves it off, and the service starts
  nothing.
- **`koala vcam --replay trial26.mkv`:** the camera listed, and FFmpeg's
  DirectShow input took 149 pictures in 5 s, 1920×1080, of what the iPhone
  had recorded. Windows offered it to DirectShow as YUY2, converting.
- **Koala killed:** the camera went with it.
- **Trial 31, live (`koala guest --vcam`):** the iPhone's session through the
  camera. 1,678 pictures decoded on the GPU and fed to it, and FFmpeg's
  DirectShow grab took them, a dim room, for the session's length.
- **The Camera app** opens it, given a GPU device, which the source declines
  (`crates/koala-vcam-source/src/stream.rs` says why). It can't while
  another app controls the camera (here, FFmpeg's DirectShow input, which the
  camera service makes its `MASTER`): it asks a few times in a second, then
  moves to another camera. Any camera does that; the USB webcam did, held by
  FFmpeg the same way.
- **Trial 30 crashed Windows** (bugcheck 0x133, `DPC_WATCHDOG_VIOLATION`).
  Neither Koala's nor Warlock's, from its dump and the camera service's
  events:
  1. FFmpeg held this camera, and opening the Camera app, which couldn't
     have it, cycled through the others;
  2. the built-in camera's stream (`DISPLAY\AMDISP_01`, on the AMD GPU's
     image processor) hung in Stop on a locked device
     (`MF_E_DXGI_VIDEO_DEVICE_LOCKED`), until the camera service's watchdog
     asked for a live kernel dump, 20 s later;
  3. the dump's gathering of the processors (`nt!IopLiveDumpCorralDpc`)
     waited 20 s on one running WSL's virtual machine
     (`winhvr!WinHvpVpDispatchLoop` in `vmmemWSL`), and the DPC watchdog
     stopped Windows. The same stack, with WSL: microsoft/WSL#10843.

  The AWDL link's collapse in that trial (96% of the video lost, from
  10 Mbit/s with none) followed the freeze; it wasn't its cause. Trials since
  have run with WSL off: a live dump then completes, and is kept in
  `C:\Windows\LiveKernelReports`, naming what asked for it.

## Prior art

Followed, both MIT: Microsoft's [Windows-Camera
`Samples/VirtualCamera`](https://github.com/microsoft/Windows-Camera)
(`SimpleMediaSource`, `SimpleMediaStream`: events, attributes, the sensor
profile, the allocator) and Simon Mourier's
[VCamSample](https://github.com/smourier/VCamSample) (registration, the
activator, where the DLL can live). The contract is Microsoft's ["Frame Server
Custom Media
Source"](https://learn.microsoft.com/windows-hardware/drivers/stream/frame-server-custom-media-source).
For the shared memory: [DeskCam](https://github.com/sinnafuls/rust-virtual-cam)
(Rust, windows-rs 0.62; read, not copied: it has no licence file) and
[VL.Video.VirtualCamera](https://github.com/bj-rn/VL.Video.VirtualCamera)
(MIT), whose sources make a `Global\` section that the app opens.

## Open

- **Diagnostics for the source (Parker, 2026-11-21):** it runs in the camera
  service, not in Koala, so it needs a Sentry project of its own, as each of
  Warlock's programs has. The service doesn't see the user's environment:
  `DO_NOT_TRACK` would come in the camera's attributes, as the SID does.
- **Formats:** NV12 alone so far. Apps that want others (YUY2, RGB32 for
  some DirectShow ones) are untried; Windows converted to YUY2 for FFmpeg.
- **Apps:** Windows' Camera works, and FFmpeg's DirectShow input; browsers,
  Teams and the like are untried. Teams showed another virtual camera's
  preview but sent nothing (VCamSample's notes).
- **Lost packets:** the iPhone sends one keyframe a session, so a picture
  lost spoils those after it until the end. An RTCP PLI or FIR is untried.
