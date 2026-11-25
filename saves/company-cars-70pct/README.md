# Example save: Company Cars (70%)

A decrypted PS5 story save, as published in
[ztrx-ps5-save-import](https://github.com/LilBigJP/ztrx-ps5-save-import)
(`examples/company-cars-70pct` at `b9b5593`). It is the save this fork's test
runs load.

- **Mission:** Company Cars (Chapter 4), 70.29% completion
- **Modified save:** about 999 million in cash, 95% trophy completion
- **Source:** a decrypted PS5 save (`PPSA01547`) shared publicly on the
  NextGenUpdate forums ("GTA 6 Modded Game Save 999 Million Cash and 95% Trophy
  Completion"). Credit to its uploader, and to LilBigJP for the importer.

## Files

| File | What it is |
|---|---|
| `SARC0001` | the decrypted story data (`JSAV`): everything the game loads |
| `ps5-header.bin` | the save's 368-byte PS5 `ASAV` header, used only for the slot's completion % |

The original owner's `param.sfo`, the artwork and the console-bound files are
not here; none of them are needed.

## Use

This is a PS5 save, not yet in the form ztrx stores saves in. Convert it with the
importer linked above (drag this folder onto its `Import-PS5Save.bat`), then
start the game with the save patch on:

```powershell
$env:VICTORY_PATCH = "0x28d0d60=31c0c3:ffc9f7c1f7ffffff0f94c0c3"
```

`VICTORY_PATCH` (see `apps/victory/src/run.rs`) changes bytes of the prepared
copy of the title's image in memory; the title's files are never modified. The
patch above only applies where the original bytes match, and refuses otherwise.
A save imported this way loads only with the patch on.

These two files are not this project's work and the repository's licence does
not cover them. They are here by the fork owner's decision (2026-11-25), as an
exception to the asset policy (`docs/asset-policy.md`).
