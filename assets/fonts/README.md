Ubuntu-Bold.ttf — bundled FALLBACK for non-Windows dev machines, so real
bold still renders there (egui itself bundles only Ubuntu-Light).

On Windows the app does not use this file: it loads Segoe UI (regular +
bold) and Consolas from %WINDIR%\Fonts at runtime — Segoe UI cannot be
redistributed with the app. Same typeface family (Ubuntu) and license
(UFL.txt) as the regular font egui bundles.
