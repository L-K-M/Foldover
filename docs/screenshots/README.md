# README screenshots

The [project README](../../README.md#screenshots) uses these light and dark
previews of the app on demo data from the
[UI tour](../../apple/FoldoverUITests/TourTests.swift).

## Source

Both images come from the `ui-tour` artifact of
[successful run 42269704089](https://github.com/L-K-M/Foldover/actions/runs/42269704089),
built from commit `69b2604b18c399c835f2040c3b1c3c9cd285a744`:

| Committed image | Tour attachment |
| --- | --- |
| `preview-light.png` | `light-05-change-selected.png` |
| `preview-dark.png` | `dark-01-preview.png` |

Each original is 1024 × 768 pixels. The committed images are lossless PNGs
cropped to the 1000 × 640 app window at x = 12, y = 48, removing the desktop,
menu bar and Dock. The app content is otherwise unchanged.

## Updating

Run the [ui-tour workflow](../../.github/workflows/ui-tour.yml) and download
its `ui-tour` artifact, or run the tour locally and export its attachments
as the workflow does. Choose matching light and dark previews with a
selected conflict. Crop to the app window, preserve readable text, and
replace both PNGs. Update the source run, commit and crop details above,
then check the README in both appearances.
