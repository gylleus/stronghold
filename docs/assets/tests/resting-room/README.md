# Resting room · myr-d21

Native farm captures, 2026-10-15. The baseline is local main `c8cad26` with only
the example's `--parked-count` fixture option added. The same two workspaces,
King, Envoy, Ledger and window dimensions are used before and after.

The empty room stays visible as a compact chamber with one spare bed, using free
space beside working rooms when it fits. It expands into a separate sleeping band
when dwarves arrive and contracts immediately when the last sleeper leaves. The
occupied room has masonry, an entrance per sleeping floor, and a reserved furniture
alcove.

| Parked dwarves | Before | After |
| --- | --- | --- |
| 0 | [Before](before-0.png) | [After](after-0.png) |
| 3 | [Before](before-3.png) | [After](after-3.png) |
| 12 | [Before](before-12.png) | [After](after-12.png) |

Additional checks: [one sleeper](after-1.png), [48 sleepers in a 600×800 window](after-overflow.png),
and [12 sleepers with an imported palette](after-custom.png). The latter uses the
existing [Endesga 16 fixture](../custom-palettes/endesga-16.json), imported as hex
colours through the normal importer, with 32-pixel figures, bold outlines and
reduced motion. Palette credit: ENDESGA; see [the palette documentation](../../../custom-palettes.md).

Reproduce the settled frames from the repository root:

```sh
cargo build --example farm
target/debug/examples/farm --workspace-counts 3,2 --heaps 0 --parked-count 0 --shot /tmp/rest-0.png
target/debug/examples/farm --workspace-counts 3,2 --heaps 0 --parked-count 3 --shot /tmp/rest-3.png
target/debug/examples/farm --workspace-counts 3,2 --heaps 0 --parked-count 12 --shot /tmp/rest-12.png
target/debug/examples/farm --workspace-counts 3,2 --heaps 0 --parked-count 48 --size 600x800 --shot /tmp/rest-overflow.png
target/debug/examples/farm --workspace-counts 3,2 --heaps 0 --parked-count 12 --import e4a672-b86f50-743f39-3f2832-9e2835-e53b44-fb922b-ffe762-63c64d-327345-193d3f-4f6781-afbfd2-ffffff-2ce8f4-0484d1 --pixels 32 --outline bold --reduced-motion --shot /tmp/rest-custom.png
```

The preview creates synthetic dwarves and a temporary palette library. It emits
additional animation-stage screenshots; the comparisons use the first settled
frame. Daylight and animation phase follow capture time.

Regression coverage includes 1, 3, 12, 48 and 120 sleepers at 1280-, 600- and
420-point widths, bed containment and separation from furniture, hit testing,
parked taskmaster labels, sleeping poses, wake transitions and parked counts.
The empty-room regression also checks expansion, immediate contraction and stable
placement after the old grace period expires.
Crowds beyond the viewport's capacity retain separate rows at the minimum scale
and can be reached with the existing pan/zoom camera. The shared renderer tests
exercise every prop, outline and bundled palette; the imported-palette test also
checks that the fire reuses four uploaded textures while the furniture stays static.
