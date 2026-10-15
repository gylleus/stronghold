# Recording speech in the hold

The stream starts in Karl's portal and travels toward the transcript recipient, using
sharp, irregular pixel segments, brief branching sparks, a bright core, and warm traveling
arrowheads. A gentle 1.6 Hz brightness pulse runs through the cyan glow; the hold's palette
and ink width apply to it.
It discharges for 240 ms after recording stops or is cancelled. Transcription and playback
do not keep it lit. Reduced motion keeps a static directional stream until discharge.

These frames use the synthetic clan in the existing farm preview, through the same
`FarmView::set_voice_target` interface as the app. They demonstrate rendering, not a live
microphone or hotkey capture. Routing tests cover the selected terminal's input, King chat,
King fallback, unrelated fields, and the global King talk session's precedence.

| State | Frame |
| --- | --- |
| Previous smooth stream, dwarf | [original dwarf](stream-dwarf-before.png) |
| Previous smooth stream, King | [original King](stream-king-before.png) |
| Before recording | [before](voice-before.png) |
| Recording to the selected dwarf | [dwarf](voice-dwarf.png) |
| Recording to the King (global talk destination) | [King](voice-king.png) |
| After recording and discharge | [stopped](voice-stopped.png) |
| Off-screen dwarf with the camera focused elsewhere | [off-screen](offscreen-dwarf.png) |
| Reduced motion, bold ink, Endesga palette | [alternate art](still-bold-dwarf.png) |

Reproduce from the repository root:

```sh
cargo run --example farm -- --voice-shots --shot /tmp/voice.png
cargo run --example farm -- --voice-shots --focus-run run0 --shot /tmp/offscreen.png
cargo run --example farm -- --voice-shots --reduced-motion --outline bold --palette endesga-32 --shot /tmp/still-bold.png
```

The hold follows the recipient's current walking position and camera transform. A recipient
outside the viewport gets a directional arrow at the edge. Filtered or removed dwarves have
no reliable position, so their stream is omitted. While idle, the effect allocates no geometry
and requests no repaints; active geometry is bounded to 32 trunk segments, at most seven short forks, and four
chevrons. Each segment uses at most 65 pixel samples, independent of camera zoom.
